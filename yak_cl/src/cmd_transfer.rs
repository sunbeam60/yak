use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use yak::{CreateOptions, EntryType, OpenMode, YakDefault as Yak};

use crate::error::{CliError, CliResult};
use crate::helpers::{close_yak, open_yak, resolve_password_create};

/// Buffer size for chunked I/O during export and import (1 MB).
const TRANSFER_BUF_SIZE: usize = 1024 * 1024;

struct TransferStats {
    dirs: u64,
    streams: u64,
    bytes: u64,
}

impl TransferStats {
    fn new() -> Self {
        Self {
            dirs: 0,
            streams: 0,
            bytes: 0,
        }
    }

    fn accumulate(&mut self, other: &TransferStats) {
        self.dirs += other.dirs;
        self.streams += other.streams;
        self.bytes += other.bytes;
    }
}

// ---------------------------------------------------------------------------
// Timestamp generation (UTC, no external dependencies)
// ---------------------------------------------------------------------------

/// Generate a UTC timestamp suffix in the format YYYY-MM-DD-HH-mm-SS.
/// Uses Howard Hinnant's civil_from_days algorithm for date decomposition.
fn timestamp_suffix() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs();

    let secs_per_day: u64 = 86400;
    let day_secs = (secs % secs_per_day) as u32;
    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    let (year, month, day) = civil_from_days((secs / secs_per_day) as i64);

    format!(
        "{:04}-{:02}-{:02}-{:02}-{:02}-{:02}",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert a day count since 1970-01-01 to (year, month, day).
/// Adapted from Howard Hinnant's civil_from_days (public domain).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Strip the `.yak` extension from a path to derive the companion folder name.
/// Returns None if the path doesn't end with `.yak`.
fn strip_yak_extension(yak_path: &str) -> Option<String> {
    let p = Path::new(yak_path);
    let ext = p.extension()?.to_str()?;
    if !ext.eq_ignore_ascii_case("yak") {
        return None;
    }
    Some(p.with_extension("").to_string_lossy().into_owned())
}

/// Determine the export output folder. Creates the directory.
///
/// Strips `.yak` from the yak file path to get the base folder name.
/// If it already exists, appends a UTC timestamp.
fn resolve_export_folder(yak_path: &str) -> Result<String, CliError> {
    let base = strip_yak_extension(yak_path)
        .ok_or_else(|| CliError::new(format!("Expected a .yak file path, got: {}", yak_path)))?;

    if !Path::new(&base).exists() {
        fs::create_dir(&base)?;
        return Ok(base);
    }

    let stamped = format!("{}-{}", base, timestamp_suffix());
    if Path::new(&stamped).exists() {
        return Err(CliError::new(format!(
            "Both '{}' and '{}' already exist",
            base, stamped
        )));
    }
    fs::create_dir(&stamped)?;
    Ok(stamped)
}

/// Determine the source folder and output .yak path for import.
///
/// Strips `.yak` to find the source folder (must exist).
/// If the .yak file already exists, inserts a timestamp before the extension.
fn resolve_import_paths(yak_path: &str) -> Result<(String, String), CliError> {
    let folder = strip_yak_extension(yak_path)
        .ok_or_else(|| CliError::new(format!("Expected a .yak file path, got: {}", yak_path)))?;

    if !Path::new(&folder).is_dir() {
        return Err(CliError::new(format!(
            "Source folder '{}' does not exist",
            folder
        )));
    }

    if !Path::new(yak_path).exists() {
        return Ok((folder, yak_path.to_string()));
    }

    // Conflict: insert timestamp before .yak extension
    let stamped = format!("{}-{}.yak", folder, timestamp_suffix());
    if Path::new(&stamped).exists() {
        return Err(CliError::new(format!(
            "Both '{}' and '{}' already exist",
            yak_path, stamped
        )));
    }
    Ok((folder, stamped))
}

// ---------------------------------------------------------------------------
// Stream-level copy
// ---------------------------------------------------------------------------

/// Export a single yak stream to a filesystem file using chunked reads.
fn export_stream(yak: &Yak, stream_path: &str, fs_path: &Path) -> Result<u64, CliError> {
    let handle = yak.open_stream(stream_path, OpenMode::Read)?;

    let length = match yak.stream_length(&handle) {
        Ok(l) => l,
        Err(e) => {
            let _ = yak.close_stream(handle);
            return Err(e.into());
        }
    };

    let mut file = fs::File::create(fs_path)?;
    let mut buf = vec![0u8; TRANSFER_BUF_SIZE];
    let mut remaining = length;

    while remaining > 0 {
        let to_read = (remaining as usize).min(TRANSFER_BUF_SIZE);
        let n = match yak.read(&handle, &mut buf[..to_read]) {
            Ok(n) => n,
            Err(e) => {
                let _ = yak.close_stream(handle);
                return Err(e.into());
            }
        };
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        remaining -= n as u64;
    }

    yak.close_stream(handle)?;
    Ok(length)
}

/// Import a single filesystem file as a yak stream using chunked writes.
fn import_stream(
    yak: &Yak,
    fs_path: &Path,
    stream_path: &str,
    compressed: bool,
) -> Result<u64, CliError> {
    let mut file = fs::File::open(fs_path)?;

    let handle = yak.create_stream(stream_path, compressed)?;

    let mut buf = vec![0u8; TRANSFER_BUF_SIZE];
    let mut total: u64 = 0;

    loop {
        let n = match file.read(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                let _ = yak.close_stream(handle);
                return Err(e.into());
            }
        };
        if n == 0 {
            break;
        }
        match yak.write(&handle, &buf[..n]) {
            Ok(_) => {}
            Err(e) => {
                let _ = yak.close_stream(handle);
                return Err(e.into());
            }
        }
        total += n as u64;
    }

    yak.close_stream(handle)?;
    Ok(total)
}

// ---------------------------------------------------------------------------
// Recursive tree walk
// ---------------------------------------------------------------------------

/// Recursively export a yak directory tree to the filesystem.
///
/// `yak_dir` is the path inside the yak file ("" for root).
/// `fs_dir` is the corresponding filesystem directory (already created).
fn export_recursive(yak: &Yak, yak_dir: &str, fs_dir: &Path) -> Result<TransferStats, CliError> {
    let entries = yak.list(yak_dir)?;
    let mut stats = TransferStats::new();

    for entry in entries {
        let yak_path = if yak_dir.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", yak_dir, entry.name)
        };
        let fs_path = fs_dir.join(&entry.name);

        match entry.entry_type {
            EntryType::Directory => {
                fs::create_dir(&fs_path)?;
                println!("  DIR  {}", yak_path);
                stats.dirs += 1;
                let sub = export_recursive(yak, &yak_path, &fs_path)?;
                stats.accumulate(&sub);
            }
            EntryType::Stream => {
                let bytes = export_stream(yak, &yak_path, &fs_path)?;
                println!("  FILE {} ({} bytes)", yak_path, bytes);
                stats.streams += 1;
                stats.bytes += bytes;
            }
        }
    }

    Ok(stats)
}

/// Recursively import a filesystem directory tree into a yak file.
///
/// `fs_dir` is the filesystem directory to import from.
/// `yak_dir` is the corresponding path inside the yak file ("" for root).
fn import_recursive(
    yak: &Yak,
    fs_dir: &Path,
    yak_dir: &str,
    compressed: bool,
) -> Result<TransferStats, CliError> {
    let mut entries: Vec<_> = fs::read_dir(fs_dir)?.collect::<Result<Vec<_>, _>>()?;
    // Sort by name for deterministic ordering
    entries.sort_by_key(|e| e.file_name());

    let mut stats = TransferStats::new();

    for entry in entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let fs_path = entry.path();
        let file_type = entry.file_type()?;

        let yak_path = if yak_dir.is_empty() {
            name_str.to_string()
        } else {
            format!("{}/{}", yak_dir, name_str)
        };

        if file_type.is_dir() {
            yak.mkdir(&yak_path)?;
            println!("  DIR  {}", yak_path);
            stats.dirs += 1;
            let sub = import_recursive(yak, &fs_path, &yak_path, compressed)?;
            stats.accumulate(&sub);
        } else if file_type.is_file() {
            let bytes = import_stream(yak, &fs_path, &yak_path, compressed)?;
            println!("  FILE {} ({} bytes)", yak_path, bytes);
            stats.streams += 1;
            stats.bytes += bytes;
        }
        // Symlinks and other special entries are silently skipped
    }

    Ok(stats)
}

// ---------------------------------------------------------------------------
// Public command entry points
// ---------------------------------------------------------------------------

pub fn cmd_export(args: &[String]) -> CliResult {
    if args.is_empty() {
        return Err(CliError::new("Usage: yak export <yak-file>"));
    }
    let yak_path = &args[0];

    let yak = open_yak(yak_path, OpenMode::Read)?;
    let folder = resolve_export_folder(yak_path)?;

    let stats = export_recursive(&yak, "", Path::new(&folder))?;

    close_yak(yak)?;

    println!(
        "Exported to {}/  ({} dirs, {} files, {} bytes)",
        folder, stats.dirs, stats.streams, stats.bytes
    );
    Ok(())
}

pub fn cmd_import(args: &[String]) -> CliResult {
    if args.is_empty() {
        return Err(CliError::new(
            "Usage: yak import <yak-file> [--compressed] [--encrypted] [--cbss N]",
        ));
    }
    let yak_path_arg = &args[0];
    let mut compressed = false;
    let mut encrypted = false;
    let mut cbss: Option<u8> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--compressed" => compressed = true,
            "--encrypted" => encrypted = true,
            "--cbss" => {
                i += 1;
                cbss = Some(
                    args.get(i)
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| CliError::new("--cbss requires a numeric argument"))?,
                );
            }
            other => {
                return Err(CliError::new(format!(
                    "Unknown option: {}. Usage: yak import <yak-file> [--compressed] [--encrypted] [--cbss N]",
                    other
                )));
            }
        }
        i += 1;
    }

    let (folder, output_path) = resolve_import_paths(yak_path_arg)?;

    let password_string = if encrypted {
        Some(resolve_password_create()?)
    } else {
        None
    };

    let mut opts = CreateOptions::default();
    if let Some(v) = cbss {
        opts.compressed_block_size_shift = v;
    }
    opts.password = password_string.as_deref().map(str::as_bytes);

    let yak = Yak::create(&output_path, opts)?;

    let stats = import_recursive(&yak, Path::new(&folder), "", compressed)?;

    close_yak(yak)?;

    println!(
        "Imported {} into {}  ({} dirs, {} files, {} bytes)",
        folder, output_path, stats.dirs, stats.streams, stats.bytes
    );
    Ok(())
}

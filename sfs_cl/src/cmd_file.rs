use stream_fs::{OpenMode, SfsDefault as Sfs};

use crate::error::{CliError, CliResult};
use crate::helpers::{
    close_sfs, expect_args, open_sfs, DEFAULT_BLOCK_INDEX_WIDTH, DEFAULT_BLOCK_SIZE_SHIFT,
};

pub fn cmd_create(args: &[String]) -> CliResult {
    if args.is_empty() {
        return Err(CliError::new(
            "Usage: sfs create <sfs-file> [--vbss N]",
        ));
    }

    let path = &args[0];
    let mut vbss: Option<u8> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--vbss" => {
                i += 1;
                vbss = Some(
                    args.get(i)
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| CliError::new("--vbss requires a numeric argument"))?,
                );
            }
            other => {
                return Err(CliError::new(format!(
                    "Unknown option: {}. Usage: sfs create <sfs-file> [--vbss N]",
                    other
                )));
            }
        }
        i += 1;
    }

    let sfs = match vbss {
        Some(v) => Sfs::create_with_vbss(path, DEFAULT_BLOCK_INDEX_WIDTH, DEFAULT_BLOCK_SIZE_SHIFT, v),
        None => Sfs::create(path, DEFAULT_BLOCK_INDEX_WIDTH, DEFAULT_BLOCK_SIZE_SHIFT),
    }
    .map_err(|e| CliError::new(format!("Error creating SFS file: {}", e)))?;

    close_sfs(sfs)?;
    println!("Created SFS file: {}", path);
    Ok(())
}

pub fn cmd_verify(args: &[String]) -> CliResult {
    expect_args(args, 1, "sfs verify <sfs-file>")?;

    let sfs = open_sfs(&args[0], OpenMode::Read)?;
    let issues = sfs
        .verify()
        .map_err(|e| CliError::new(format!("Error running verify: {}", e)))?;

    if issues.is_empty() {
        println!("OK — no issues found");
    } else {
        println!("Found {} issue(s):", issues.len());
        for issue in &issues {
            println!("  - {}", issue);
        }
    }

    close_sfs(sfs)?;

    if issues.is_empty() {
        Ok(())
    } else {
        Err(CliError::new("Verification found issues"))
    }
}

pub fn cmd_info(args: &[String]) -> CliResult {
    expect_args(args, 2, "sfs info <sfs-file> <stream>")?;

    let sfs = open_sfs(&args[0], OpenMode::Read)?;
    let handle = sfs
        .open_stream(&args[1], OpenMode::Read)
        .map_err(|e| CliError::new(format!("Error opening stream: {}", e)))?;

    let length = match sfs.stream_length(&handle) {
        Ok(l) => l,
        Err(e) => {
            let _ = sfs.close_stream(handle);
            return Err(CliError::new(format!("Error getting stream length: {}", e)));
        }
    };
    let reserved = match sfs.stream_reserved(&handle) {
        Ok(r) => r,
        Err(e) => {
            let _ = sfs.close_stream(handle);
            return Err(CliError::new(format!(
                "Error getting stream reserved: {}",
                e
            )));
        }
    };
    let compressed = match sfs.is_stream_compressed(&handle) {
        Ok(c) => c,
        Err(e) => {
            let _ = sfs.close_stream(handle);
            return Err(CliError::new(format!(
                "Error checking stream compression: {}",
                e
            )));
        }
    };

    println!("Stream:     {}", args[1]);
    println!("Length:     {} bytes", length);
    println!("Reserved:   {} bytes", reserved);
    println!("Compressed: {}", if compressed { "yes" } else { "no" });

    let _ = sfs.close_stream(handle);
    close_sfs(sfs)
}

pub fn cmd_hello(_args: &[String]) -> CliResult {
    println!("{}", stream_fs::hello());
    Ok(())
}

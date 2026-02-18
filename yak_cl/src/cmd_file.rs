use yak::{OpenMode, YakDefault as Yak};

use crate::error::{CliError, CliResult};
use crate::helpers::{
    close_yak, expect_args, open_yak, resolve_password_create, DEFAULT_BLOCK_INDEX_WIDTH,
    DEFAULT_BLOCK_SIZE_SHIFT,
};

pub fn cmd_create(args: &[String]) -> CliResult {
    if args.is_empty() {
        return Err(CliError::new(
            "Usage: yak create <yak-file> [--cbss N] [--encrypted]",
        ));
    }

    let path = &args[0];
    let mut cbss: Option<u8> = None;
    let mut encrypted = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--cbss" => {
                i += 1;
                cbss = Some(
                    args.get(i)
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| CliError::new("--cbss requires a numeric argument"))?,
                );
            }
            "--encrypted" => {
                encrypted = true;
            }
            other => {
                return Err(CliError::new(format!(
                    "Unknown option: {}. Usage: yak create <yak-file> [--cbss N] [--encrypted]",
                    other
                )));
            }
        }
        i += 1;
    }

    let yak = if encrypted {
        let password = resolve_password_create()?;
        let pw = password.as_bytes();
        match cbss {
            Some(v) => Yak::create_encrypted_with_cbss(
                path,
                DEFAULT_BLOCK_INDEX_WIDTH,
                DEFAULT_BLOCK_SIZE_SHIFT,
                v,
                pw,
            ),
            None => Yak::create_encrypted(
                path,
                DEFAULT_BLOCK_INDEX_WIDTH,
                DEFAULT_BLOCK_SIZE_SHIFT,
                pw,
            ),
        }
    } else {
        match cbss {
            Some(v) => {
                Yak::create_with_cbss(path, DEFAULT_BLOCK_INDEX_WIDTH, DEFAULT_BLOCK_SIZE_SHIFT, v)
            }
            None => Yak::create(path, DEFAULT_BLOCK_INDEX_WIDTH, DEFAULT_BLOCK_SIZE_SHIFT),
        }
    }
    .map_err(|e| CliError::new(format!("Error creating Yak file: {}", e)))?;

    close_yak(yak)?;
    let enc_label = if encrypted { " (encrypted)" } else { "" };
    println!("Created Yak file{}: {}", enc_label, path);
    Ok(())
}

pub fn cmd_verify(args: &[String]) -> CliResult {
    expect_args(args, 1, "yak verify <yak-file>")?;

    let sfs = open_yak(&args[0], OpenMode::Read)?;
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

    close_yak(sfs)?;

    if issues.is_empty() {
        Ok(())
    } else {
        Err(CliError::new("Verification found issues"))
    }
}

pub fn cmd_info(args: &[String]) -> CliResult {
    expect_args(args, 2, "yak info <yak-file> <stream>")?;

    let sfs = open_yak(&args[0], OpenMode::Read)?;
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
    close_yak(sfs)
}

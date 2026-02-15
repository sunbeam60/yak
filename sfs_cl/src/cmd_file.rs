use stream_fs::{OpenMode, SfsDefault as Sfs};

use crate::error::{CliError, CliResult};
use crate::helpers::{
    close_sfs, expect_args, open_sfs, DEFAULT_BLOCK_INDEX_WIDTH, DEFAULT_BLOCK_SIZE_SHIFT,
};

pub fn cmd_create(args: &[String]) -> CliResult {
    expect_args(args, 1, "sfs create <sfs-file>")?;

    let sfs = Sfs::create(
        &args[0],
        DEFAULT_BLOCK_INDEX_WIDTH,
        DEFAULT_BLOCK_SIZE_SHIFT,
    )
    .map_err(|e| CliError::new(format!("Error creating SFS file: {}", e)))?;
    close_sfs(sfs)?;
    println!("Created SFS file: {}", args[0]);
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

    println!("Stream: {}", args[1]);
    println!("Length:   {} bytes", length);
    println!("Reserved: {} bytes", reserved);

    let _ = sfs.close_stream(handle);
    close_sfs(sfs)
}

pub fn cmd_hello(_args: &[String]) -> CliResult {
    println!("{}", stream_fs::hello());
    Ok(())
}

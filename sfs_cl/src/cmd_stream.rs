use std::fs;
use std::io::{self, Write};

use stream_fs::OpenMode;

use crate::error::{CliError, CliResult};
use crate::helpers::{close_sfs, expect_args, open_sfs, read_stream_fully};

pub fn cmd_put(args: &[String]) -> CliResult {
    expect_args(args, 3, "sfs put <sfs-file> <local-file> <stream>")?;

    let local_file = &args[1];
    let stream_path = &args[2];

    let data = fs::read(local_file)
        .map_err(|e| CliError::new(format!("Error reading '{}': {}", local_file, e)))?;

    let sfs = open_sfs(&args[0], OpenMode::Write)?;
    let handle = sfs
        .create_stream(stream_path)
        .map_err(|e| CliError::new(format!("Error creating stream: {}", e)))?;

    match sfs.write(&handle, &data) {
        Ok(n) => {
            if n != data.len() {
                eprintln!("Warning: wrote {} bytes, expected {}", n, data.len());
            }
            println!(
                "Imported {} bytes from {} to {}",
                n, local_file, stream_path
            );
        }
        Err(e) => {
            let _ = sfs.close_stream(handle);
            return Err(CliError::new(format!("Error writing stream: {}", e)));
        }
    }

    sfs.close_stream(handle)
        .map_err(|e| CliError::new(format!("Error closing stream: {}", e)))?;
    close_sfs(sfs)
}

pub fn cmd_get(args: &[String]) -> CliResult {
    expect_args(args, 3, "sfs get <sfs-file> <stream> <local-file>")?;

    let stream_path = &args[1];
    let local_file = &args[2];

    let sfs = open_sfs(&args[0], OpenMode::Read)?;
    let buffer = read_stream_fully(&sfs, stream_path)?;
    close_sfs(sfs)?;

    fs::write(local_file, &buffer)
        .map_err(|e| CliError::new(format!("Error writing '{}': {}", local_file, e)))?;
    println!(
        "Exported {} bytes from {} to {}",
        buffer.len(),
        stream_path,
        local_file
    );
    Ok(())
}

pub fn cmd_cat(args: &[String]) -> CliResult {
    expect_args(args, 2, "sfs cat <sfs-file> <stream>")?;

    let sfs = open_sfs(&args[0], OpenMode::Read)?;
    let buffer = read_stream_fully(&sfs, &args[1])?;
    close_sfs(sfs)?;

    io::stdout()
        .write_all(&buffer)
        .map_err(|e| CliError::new(format!("Error writing to stdout: {}", e)))?;
    Ok(())
}

pub fn cmd_rm(args: &[String]) -> CliResult {
    expect_args(args, 2, "sfs rm <sfs-file> <stream>")?;

    let sfs = open_sfs(&args[0], OpenMode::Write)?;
    sfs.delete_stream(&args[1])
        .map_err(|e| CliError::new(format!("Error deleting stream: {}", e)))?;
    println!("Deleted stream: {}", args[1]);
    close_sfs(sfs)
}

pub fn cmd_mv(args: &[String]) -> CliResult {
    expect_args(args, 3, "sfs mv <sfs-file> <old> <new>")?;

    let sfs = open_sfs(&args[0], OpenMode::Write)?;
    sfs.rename_stream(&args[1], &args[2])
        .map_err(|e| CliError::new(format!("Error renaming stream: {}", e)))?;
    println!("Renamed stream: {} -> {}", args[1], args[2]);
    close_sfs(sfs)
}

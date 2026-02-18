use yak::{EntryType, OpenMode, YakDefault as Yak};

use crate::error::{CliError, CliResult};
use crate::helpers::{close_yak, expect_args, expect_args_range, open_yak};

pub fn cmd_ls(args: &[String]) -> CliResult {
    let (recursive, rest) = if args.first().map(|s| s.as_str()) == Some("-r") {
        (true, &args[1..])
    } else {
        (false, args)
    };

    expect_args_range(rest, 1, 2, "yak ls [-r] <yak-file> [dir]")?;

    let sfs = open_yak(&rest[0], OpenMode::Read)?;
    let dir = if rest.len() == 2 { &rest[1] } else { "" };

    let result = if recursive {
        ls_recursive(&sfs, dir, dir)
    } else {
        ls_flat(&sfs, dir)
    };

    result?;
    close_yak(sfs)
}

fn ls_flat(sfs: &Yak, dir: &str) -> CliResult {
    let entries = sfs
        .list(dir)
        .map_err(|e| CliError::new(format!("Error listing directory: {}", e)))?;

    if entries.is_empty() {
        println!("(empty)");
    } else {
        for entry in entries {
            let type_str = match entry.entry_type {
                EntryType::Directory => "DIR   ",
                EntryType::Stream => "STREAM",
            };
            println!("{} {}", type_str, entry.name);
        }
    }
    Ok(())
}

fn ls_recursive(sfs: &Yak, dir: &str, display_prefix: &str) -> CliResult {
    let entries = sfs
        .list(dir)
        .map_err(|e| CliError::new(format!("Error listing directory: {}", e)))?;

    for entry in &entries {
        let display_path = if display_prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", display_prefix, entry.name)
        };
        let type_str = match entry.entry_type {
            EntryType::Directory => "DIR   ",
            EntryType::Stream => "STREAM",
        };
        println!("{} {}", type_str, display_path);

        if entry.entry_type == EntryType::Directory {
            let child_dir = if dir.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", dir, entry.name)
            };
            ls_recursive(sfs, &child_dir, &display_path)?;
        }
    }

    Ok(())
}

pub fn cmd_mkdir(args: &[String]) -> CliResult {
    expect_args(args, 2, "yak mkdir <yak-file> <dir>")?;

    let sfs = open_yak(&args[0], OpenMode::Write)?;
    sfs.mkdir(&args[1])
        .map_err(|e| CliError::new(format!("Error creating directory: {}", e)))?;
    println!("Created directory: {}", args[1]);
    close_yak(sfs)
}

pub fn cmd_rmdir(args: &[String]) -> CliResult {
    expect_args(args, 2, "yak rmdir <yak-file> <dir>")?;

    let sfs = open_yak(&args[0], OpenMode::Write)?;
    sfs.rmdir(&args[1])
        .map_err(|e| CliError::new(format!("Error removing directory: {}", e)))?;
    println!("Removed directory: {}", args[1]);
    close_yak(sfs)
}

pub fn cmd_mv_dir(args: &[String]) -> CliResult {
    expect_args(args, 3, "yak mv-dir <yak-file> <old> <new>")?;

    let sfs = open_yak(&args[0], OpenMode::Write)?;
    sfs.rename_dir(&args[1], &args[2])
        .map_err(|e| CliError::new(format!("Error renaming directory: {}", e)))?;
    println!("Renamed directory: {} -> {}", args[1], args[2]);
    close_yak(sfs)
}

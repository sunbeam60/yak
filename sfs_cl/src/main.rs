use std::env;
use std::fs;
use std::io::{self, Write};
use std::process;
use stream_fs::{EntryType, OpenMode, SfsDefault as Sfs};

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    let result = match args[1].as_str() {
        "create" => cmd_create(&args[2..]),
        "ls" => cmd_ls(&args[2..]),
        "mkdir" => cmd_mkdir(&args[2..]),
        "rmdir" => cmd_rmdir(&args[2..]),
        "mv-dir" => cmd_mv_dir(&args[2..]),
        "put" => cmd_put(&args[2..]),
        "get" => cmd_get(&args[2..]),
        "cat" => cmd_cat(&args[2..]),
        "rm" => cmd_rm(&args[2..]),
        "mv" => cmd_mv(&args[2..]),
        "info" => cmd_info(&args[2..]),
        "verify" => cmd_verify(&args[2..]),
        "hello" => {
            println!("{}", stream_fs::hello());
            Ok(())
        }
        other => {
            eprintln!("Unknown command: {}", other);
            print_usage();
            Err(())
        }
    };

    if result.is_err() {
        process::exit(1);
    }
}

fn print_usage() {
    eprintln!("Usage: sfs <command> [args...]");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  create <sfs-file>                      Create a new SFS file");
    eprintln!(
        "  ls [-r] <sfs-file> [dir]               List directory contents (-r for recursive)"
    );
    eprintln!("  mkdir <sfs-file> <dir>                 Create a directory");
    eprintln!("  rmdir <sfs-file> <dir>                 Remove an empty directory");
    eprintln!("  mv-dir <sfs-file> <old> <new>          Rename/move a directory");
    eprintln!("  put <sfs-file> <local-file> <stream>   Import a file as a stream");
    eprintln!("  get <sfs-file> <stream> <local-file>   Export a stream to a file");
    eprintln!("  cat <sfs-file> <stream>                Print stream contents to stdout");
    eprintln!("  rm <sfs-file> <stream>                 Delete a stream");
    eprintln!("  mv <sfs-file> <old> <new>              Rename/move a stream");
    eprintln!("  info <sfs-file> <stream>               Show stream information");
    eprintln!("  verify <sfs-file>                      Verify SFS integrity");
    eprintln!("  hello                                  Print a greeting");
}

fn cmd_create(args: &[String]) -> Result<(), ()> {
    if args.len() != 1 {
        eprintln!("Usage: sfs create <sfs-file>");
        return Err(());
    }

    match Sfs::create(&args[0], 4, 12) {
        Ok(sfs) => {
            sfs.close();
            println!("Created SFS file: {}", args[0]);
            Ok(())
        }
        Err(e) => {
            eprintln!("Error creating SFS file: {}", e);
            Err(())
        }
    }
}

fn cmd_ls(args: &[String]) -> Result<(), ()> {
    let (recursive, rest) = if args.first().map(|s| s.as_str()) == Some("-r") {
        (true, &args[1..])
    } else {
        (false, args)
    };

    if rest.is_empty() || rest.len() > 2 {
        eprintln!("Usage: sfs ls [-r] <sfs-file> [dir]");
        return Err(());
    }

    let sfs_path = &rest[0];
    let dir = if rest.len() == 2 { &rest[1] } else { "" };

    let sfs = match Sfs::open(sfs_path, OpenMode::Read) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    let result = if recursive {
        ls_recursive(&sfs, dir, dir)
    } else {
        ls_flat(&sfs, dir)
    };

    sfs.close();
    result
}

fn ls_flat(sfs: &Sfs, dir: &str) -> Result<(), ()> {
    match sfs.list(dir) {
        Ok(entries) => {
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
        Err(e) => {
            eprintln!("Error listing directory: {}", e);
            Err(())
        }
    }
}

fn ls_recursive(sfs: &Sfs, dir: &str, display_prefix: &str) -> Result<(), ()> {
    let entries = match sfs.list(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error listing directory: {}", e);
            return Err(());
        }
    };

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

fn cmd_mkdir(args: &[String]) -> Result<(), ()> {
    if args.len() != 2 {
        eprintln!("Usage: sfs mkdir <sfs-file> <dir>");
        return Err(());
    }

    let sfs = match Sfs::open(&args[0], OpenMode::Write) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    match sfs.mkdir(&args[1]) {
        Ok(()) => {
            println!("Created directory: {}", args[1]);
            sfs.close();
            Ok(())
        }
        Err(e) => {
            eprintln!("Error creating directory: {}", e);
            Err(())
        }
    }
}

fn cmd_rmdir(args: &[String]) -> Result<(), ()> {
    if args.len() != 2 {
        eprintln!("Usage: sfs rmdir <sfs-file> <dir>");
        return Err(());
    }

    let sfs = match Sfs::open(&args[0], OpenMode::Write) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    match sfs.rmdir(&args[1]) {
        Ok(()) => {
            println!("Removed directory: {}", args[1]);
            sfs.close();
            Ok(())
        }
        Err(e) => {
            eprintln!("Error removing directory: {}", e);
            Err(())
        }
    }
}

fn cmd_mv_dir(args: &[String]) -> Result<(), ()> {
    if args.len() != 3 {
        eprintln!("Usage: sfs mv-dir <sfs-file> <old> <new>");
        return Err(());
    }

    let sfs = match Sfs::open(&args[0], OpenMode::Write) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    match sfs.rename_dir(&args[1], &args[2]) {
        Ok(()) => {
            println!("Renamed directory: {} -> {}", args[1], args[2]);
            sfs.close();
            Ok(())
        }
        Err(e) => {
            eprintln!("Error renaming directory: {}", e);
            Err(())
        }
    }
}

fn cmd_put(args: &[String]) -> Result<(), ()> {
    if args.len() != 3 {
        eprintln!("Usage: sfs put <sfs-file> <local-file> <stream>");
        return Err(());
    }

    let local_file = &args[1];
    let stream_path = &args[2];

    // Read the local file
    let data = match fs::read(local_file) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error reading local file: {}", e);
            return Err(());
        }
    };

    let sfs = match Sfs::open(&args[0], OpenMode::Write) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    let handle = match sfs.create_stream(stream_path) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Error creating stream: {}", e);
            return Err(());
        }
    };

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
            eprintln!("Error writing stream: {}", e);
            let _ = sfs.close_stream(handle);
            return Err(());
        }
    }

    match sfs.close_stream(handle) {
        Ok(()) => {
            sfs.close();
            Ok(())
        }
        Err(e) => {
            eprintln!("Error closing stream: {}", e);
            Err(())
        }
    }
}

fn cmd_get(args: &[String]) -> Result<(), ()> {
    if args.len() != 3 {
        eprintln!("Usage: sfs get <sfs-file> <stream> <local-file>");
        return Err(());
    }

    let stream_path = &args[1];
    let local_file = &args[2];

    let sfs = match Sfs::open(&args[0], OpenMode::Read) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    let handle = match sfs.open_stream(stream_path, OpenMode::Read) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Error opening stream: {}", e);
            return Err(());
        }
    };

    let length = match sfs.stream_length(&handle) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Error getting stream length: {}", e);
            let _ = sfs.close_stream(handle);
            return Err(());
        }
    };

    let mut buffer = vec![0u8; length as usize];
    match sfs.read(&handle, &mut buffer) {
        Ok(n) => {
            if n != length as usize {
                eprintln!("Warning: read {} bytes, expected {}", n, length);
                buffer.truncate(n);
            }
        }
        Err(e) => {
            eprintln!("Error reading stream: {}", e);
            let _ = sfs.close_stream(handle);
            return Err(());
        }
    }

    if let Err(e) = sfs.close_stream(handle) {
        eprintln!("Error closing stream: {}", e);
    }
    sfs.close();

    match fs::write(local_file, &buffer) {
        Ok(()) => {
            println!(
                "Exported {} bytes from {} to {}",
                buffer.len(),
                stream_path,
                local_file
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("Error writing local file: {}", e);
            Err(())
        }
    }
}

fn cmd_cat(args: &[String]) -> Result<(), ()> {
    if args.len() != 2 {
        eprintln!("Usage: sfs cat <sfs-file> <stream>");
        return Err(());
    }

    let sfs = match Sfs::open(&args[0], OpenMode::Read) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    let handle = match sfs.open_stream(&args[1], OpenMode::Read) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Error opening stream: {}", e);
            return Err(());
        }
    };

    let length = match sfs.stream_length(&handle) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Error getting stream length: {}", e);
            let _ = sfs.close_stream(handle);
            return Err(());
        }
    };

    let mut buffer = vec![0u8; length as usize];
    match sfs.read(&handle, &mut buffer) {
        Ok(n) => {
            buffer.truncate(n);
        }
        Err(e) => {
            eprintln!("Error reading stream: {}", e);
            let _ = sfs.close_stream(handle);
            return Err(());
        }
    }

    if let Err(e) = sfs.close_stream(handle) {
        eprintln!("Error closing stream: {}", e);
    }
    sfs.close();

    if let Err(e) = io::stdout().write_all(&buffer) {
        eprintln!("Error writing to stdout: {}", e);
        return Err(());
    }

    Ok(())
}

fn cmd_rm(args: &[String]) -> Result<(), ()> {
    if args.len() != 2 {
        eprintln!("Usage: sfs rm <sfs-file> <stream>");
        return Err(());
    }

    let sfs = match Sfs::open(&args[0], OpenMode::Write) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    match sfs.delete_stream(&args[1]) {
        Ok(()) => {
            println!("Deleted stream: {}", args[1]);
            sfs.close();
            Ok(())
        }
        Err(e) => {
            eprintln!("Error deleting stream: {}", e);
            Err(())
        }
    }
}

fn cmd_mv(args: &[String]) -> Result<(), ()> {
    if args.len() != 3 {
        eprintln!("Usage: sfs mv <sfs-file> <old> <new>");
        return Err(());
    }

    let sfs = match Sfs::open(&args[0], OpenMode::Write) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    match sfs.rename_stream(&args[1], &args[2]) {
        Ok(()) => {
            println!("Renamed stream: {} -> {}", args[1], args[2]);
            sfs.close();
            Ok(())
        }
        Err(e) => {
            eprintln!("Error renaming stream: {}", e);
            Err(())
        }
    }
}

fn cmd_verify(args: &[String]) -> Result<(), ()> {
    if args.len() != 1 {
        eprintln!("Usage: sfs verify <sfs-file>");
        return Err(());
    }

    let sfs = match Sfs::open(&args[0], OpenMode::Read) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    match sfs.verify() {
        Ok(issues) => {
            if issues.is_empty() {
                println!("OK — no issues found");
            } else {
                println!("Found {} issue(s):", issues.len());
                for issue in &issues {
                    println!("  - {}", issue);
                }
            }
            sfs.close();
            if issues.is_empty() {
                Ok(())
            } else {
                Err(())
            }
        }
        Err(e) => {
            eprintln!("Error running verify: {}", e);
            Err(())
        }
    }
}

fn cmd_info(args: &[String]) -> Result<(), ()> {
    if args.len() != 2 {
        eprintln!("Usage: sfs info <sfs-file> <stream>");
        return Err(());
    }

    let sfs = match Sfs::open(&args[0], OpenMode::Read) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error opening SFS file: {}", e);
            return Err(());
        }
    };

    let handle = match sfs.open_stream(&args[1], OpenMode::Read) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Error opening stream: {}", e);
            return Err(());
        }
    };

    match sfs.stream_length(&handle) {
        Ok(length) => {
            println!("Stream: {}", args[1]);
            println!("Length: {} bytes", length);
            let _ = sfs.close_stream(handle);
            sfs.close();
            Ok(())
        }
        Err(e) => {
            eprintln!("Error getting stream length: {}", e);
            let _ = sfs.close_stream(handle);
            Err(())
        }
    }
}

mod bench;
mod cmd_dir;
mod cmd_file;
mod cmd_stream;
mod cmd_transfer;
mod error;
mod helpers;

use std::env;
use std::process;

use crate::error::CliResult;

/// Metadata for a single command — used for dispatch and help generation.
struct CommandEntry {
    name: &'static str,
    usage: &'static str,
    description: &'static str,
    run: fn(&[String]) -> CliResult,
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// All commands. Order here = order in help output.
const COMMANDS: &[CommandEntry] = &[
    CommandEntry {
        name: "create",
        usage: "yak create <yak-file> [--cbss N] [--encrypted]",
        description: "Create a new Yak file (--encrypted enables AES-XTS encryption)",
        run: cmd_file::cmd_create,
    },
    CommandEntry {
        name: "ls",
        usage: "yak ls [-r] <yak-file> [dir]",
        description: "List directory contents (-r for recursive)",
        run: cmd_dir::cmd_ls,
    },
    CommandEntry {
        name: "mkdir",
        usage: "yak mkdir <yak-file> <dir>",
        description: "Create a directory",
        run: cmd_dir::cmd_mkdir,
    },
    CommandEntry {
        name: "rmdir",
        usage: "yak rmdir <yak-file> <dir>",
        description: "Remove an empty directory",
        run: cmd_dir::cmd_rmdir,
    },
    CommandEntry {
        name: "mv-dir",
        usage: "yak mv-dir <yak-file> <old> <new>",
        description: "Rename/move a directory",
        run: cmd_dir::cmd_mv_dir,
    },
    CommandEntry {
        name: "put",
        usage: "yak put <yak-file> <local-file> <stream>",
        description: "Import a file as a stream",
        run: cmd_stream::cmd_put,
    },
    CommandEntry {
        name: "putc",
        usage: "yak putc <yak-file> <local-file> <stream>",
        description: "Import a file as a compressed stream",
        run: cmd_stream::cmd_putc,
    },
    CommandEntry {
        name: "get",
        usage: "yak get <yak-file> <stream> <local-file>",
        description: "Export a stream to a file",
        run: cmd_stream::cmd_get,
    },
    CommandEntry {
        name: "cat",
        usage: "yak cat <yak-file> <stream>",
        description: "Print stream contents to stdout",
        run: cmd_stream::cmd_cat,
    },
    CommandEntry {
        name: "rm",
        usage: "yak rm <yak-file> <stream>",
        description: "Delete a stream",
        run: cmd_stream::cmd_rm,
    },
    CommandEntry {
        name: "mv",
        usage: "yak mv <yak-file> <old> <new>",
        description: "Rename/move a stream",
        run: cmd_stream::cmd_mv,
    },
    CommandEntry {
        name: "info",
        usage: "yak info <yak-file> <stream>",
        description: "Show stream information",
        run: cmd_file::cmd_info,
    },
    CommandEntry {
        name: "verify",
        usage: "yak verify <yak-file>",
        description: "Verify Yak integrity",
        run: cmd_file::cmd_verify,
    },
    CommandEntry {
        name: "optimize",
        usage: "yak optimize <yak-file> [--encrypted]",
        description: "Compact and defragment by rewriting without free blocks",
        run: cmd_file::cmd_optimize,
    },
    CommandEntry {
        name: "export",
        usage: "yak export <yak-file>",
        description: "Export all contents to a filesystem folder",
        run: cmd_transfer::cmd_export,
    },
    CommandEntry {
        name: "import",
        usage: "yak import <yak-file> [--compressed] [--encrypted] [--cbss N]",
        description: "Import a filesystem folder into a new Yak file",
        run: cmd_transfer::cmd_import,
    },
    CommandEntry {
        name: "bench",
        usage: "yak bench <scenario> [options]",
        description: "Run benchmark scenario",
        run: bench::cmd_bench,
    },
];

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    match args[1].as_str() {
        "--help" | "-h" => {
            print_usage();
            return;
        }
        "--version" | "-V" => {
            println!("yak {}", VERSION);
            return;
        }
        _ => {}
    }

    let cmd_name = &args[1];
    let cmd_args = &args[2..];

    let result = match COMMANDS.iter().find(|c| c.name == cmd_name.as_str()) {
        Some(cmd) => (cmd.run)(cmd_args),
        None => {
            eprintln!("Unknown command: {}", cmd_name);
            print_usage();
            process::exit(1);
        }
    };

    if let Err(e) = result {
        eprintln!("{}", e);
        process::exit(1);
    }
}

fn print_usage() {
    eprintln!("Usage: yak <command> [args...]");
    eprintln!();
    eprintln!("Commands:");

    let max_usage_len = COMMANDS.iter().map(|c| c.usage.len()).max().unwrap_or(0);
    for cmd in COMMANDS {
        eprintln!(
            "  {:<width$}  {}",
            cmd.usage,
            cmd.description,
            width = max_usage_len
        );
    }

    eprintln!();
    eprintln!("Options:");
    eprintln!("  --help, -h       Show this help message");
    eprintln!("  --version, -V    Show version");
}

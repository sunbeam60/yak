use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: sfs <command>");
        eprintln!("Commands:");
        eprintln!("  hello   Print a greeting from the SFS library");
        process::exit(1);
    }

    match args[1].as_str() {
        "hello" => {
            println!("{}", stream_fs::hello());
        }
        other => {
            eprintln!("Unknown command: {other}");
            eprintln!("Usage: sfs <command>");
            eprintln!("Commands:");
            eprintln!("  hello   Print a greeting from the SFS library");
            process::exit(1);
        }
    }
}

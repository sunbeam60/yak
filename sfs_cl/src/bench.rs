use std::sync::Arc;
use std::thread;

use stream_fs::{OpenMode, SfsDefault as Sfs};

const BENCH_FILE: &str = "_bench.sfs";
const DEFAULT_BSS: u8 = 12;
const DEFAULT_BIW: u8 = 4;
const DEFAULT_THREADS: usize = 4;

// ---------------------------------------------------------------------------
// Cleanup guard — deletes temp file on drop (even on panic)
// ---------------------------------------------------------------------------

struct CleanupGuard<'a> {
    path: &'a str,
}

impl Drop for CleanupGuard<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.path);
    }
}

// ---------------------------------------------------------------------------
// Deterministic buffer
// ---------------------------------------------------------------------------

fn make_buffer(size: usize) -> Vec<u8> {
    (0u8..=255).cycle().take(size).collect()
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

struct BenchArgs {
    scenario: String,
    bss: u8,
    biw: u8,
    threads: usize,
}

fn parse_bench_args(args: &[String]) -> Result<BenchArgs, ()> {
    if args.is_empty() {
        print_bench_usage();
        return Err(());
    }

    let scenario = args[0].clone();
    let mut bss = DEFAULT_BSS;
    let mut biw = DEFAULT_BIW;
    let mut threads = DEFAULT_THREADS;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--block-shift" => {
                i += 1;
                bss = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| eprintln!("--block-shift requires a numeric argument"))?;
            }
            "--index-width" => {
                i += 1;
                biw = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| eprintln!("--index-width requires a numeric argument"))?;
            }
            "--threads" => {
                i += 1;
                threads = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| eprintln!("--threads requires a numeric argument"))?;
            }
            other => {
                eprintln!("Unknown option: {}", other);
                print_bench_usage();
                return Err(());
            }
        }
        i += 1;
    }

    Ok(BenchArgs {
        scenario,
        bss,
        biw,
        threads,
    })
}

fn print_bench_usage() {
    eprintln!("Usage: sfs bench <scenario> [--block-shift N] [--index-width N] [--threads N]");
    eprintln!();
    eprintln!("Scenarios:");
    eprintln!("  large-write      Write 5 streams of 10MB each");
    eprintln!("  small-write      Write 500 streams of 10KB each");
    eprintln!("  threaded-write   T threads writing streams concurrently");
    eprintln!("  threaded-read    T threads reading streams concurrently");
    eprintln!("  churn            Write then delete many streams (5 rounds)");
    eprintln!("  all              Run all scenarios in sequence");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --block-shift N  Block size as power of 2 (default: 12 = 4KB blocks)");
    eprintln!("                   Examples: 10 = 1KB, 12 = 4KB, 16 = 64KB");
    eprintln!("  --index-width N  Block index size (default: 4 = 4B index / 32 bits)");
    eprintln!("                   Examples: 2 = 2B index, 4 = 4B index, 8 = 8B index");
    eprintln!("  --threads N      Thread count for threaded scenarios (default: 4)");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn cmd_bench(args: &[String]) -> Result<(), ()> {
    let bench_args = parse_bench_args(args)?;

    match bench_args.scenario.as_str() {
        "large-write" => run_large_write(bench_args.bss, bench_args.biw),
        "small-write" => run_small_write(bench_args.bss, bench_args.biw),
        "threaded-write" => run_threaded_write(bench_args.bss, bench_args.biw, bench_args.threads),
        "threaded-read" => run_threaded_read(bench_args.bss, bench_args.biw, bench_args.threads),
        "churn" => run_churn(bench_args.bss, bench_args.biw),
        "all" => run_all(bench_args.bss, bench_args.biw, bench_args.threads),
        other => {
            eprintln!("Unknown bench scenario: {}", other);
            print_bench_usage();
            Err(())
        }
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Write 5 streams of 10MB each.
fn run_large_write(bss: u8, biw: u8) -> Result<(), ()> {
    let _guard = CleanupGuard { path: BENCH_FILE };
    let sfs = Sfs::create(BENCH_FILE, biw, bss).map_err(|e| eprintln!("Error: {}", e))?;
    let buf = make_buffer(10 * 1024 * 1024);

    for i in 0..5 {
        let name = format!("large_{}.bin", i);
        let handle = sfs
            .create_stream(&name)
            .map_err(|e| eprintln!("Error: {}", e))?;
        sfs.write(&handle, &buf)
            .map_err(|e| eprintln!("Error: {}", e))?;
        sfs.close_stream(handle)
            .map_err(|e| eprintln!("Error: {}", e))?;
    }

    sfs.close();
    Ok(())
}

/// Write 500 streams of 10KB each.
fn run_small_write(bss: u8, biw: u8) -> Result<(), ()> {
    let _guard = CleanupGuard { path: BENCH_FILE };
    let sfs = Sfs::create(BENCH_FILE, biw, bss).map_err(|e| eprintln!("Error: {}", e))?;
    let buf = make_buffer(10 * 1024);

    for i in 0..500 {
        let name = format!("stream_{:04}.bin", i);
        let handle = sfs
            .create_stream(&name)
            .map_err(|e| eprintln!("Error: {}", e))?;
        sfs.write(&handle, &buf)
            .map_err(|e| eprintln!("Error: {}", e))?;
        sfs.close_stream(handle)
            .map_err(|e| eprintln!("Error: {}", e))?;
    }

    sfs.close();
    Ok(())
}

/// T threads, each writing a partition of 250 streams (40KB each).
fn run_threaded_write(bss: u8, biw: u8, threads: usize) -> Result<(), ()> {
    let _guard = CleanupGuard { path: BENCH_FILE };
    let sfs = Arc::new(Sfs::create(BENCH_FILE, biw, bss).map_err(|e| eprintln!("Error: {}", e))?);
    let buf = Arc::new(make_buffer(40 * 1024));

    let total_streams = 250usize;
    let per_thread = total_streams / threads;
    let remainder = total_streams % threads;

    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let sfs = Arc::clone(&sfs);
            let buf = Arc::clone(&buf);
            let count = per_thread + if t < remainder { 1 } else { 0 };
            let offset = t * per_thread + t.min(remainder);
            thread::spawn(move || -> Result<(), String> {
                for i in 0..count {
                    let name = format!("stream_{:04}.bin", offset + i);
                    let handle = sfs.create_stream(&name).map_err(|e| e.to_string())?;
                    sfs.write(&handle, &buf).map_err(|e| e.to_string())?;
                    sfs.close_stream(handle).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
        })
        .collect();

    let mut failed = false;
    for h in handles {
        if let Err(e) = h.join().unwrap_or(Err("thread panicked".to_string())) {
            eprintln!("Thread error: {}", e);
            failed = true;
        }
    }

    // All threads joined — we are the sole Arc owner
    match Arc::try_unwrap(sfs) {
        Ok(s) => s.close(),
        Err(_) => eprintln!("Warning: could not unwrap Arc for close"),
    }

    if failed {
        Err(())
    } else {
        Ok(())
    }
}

/// Populate 100 streams of 10KB, then T threads each read all streams.
fn run_threaded_read(bss: u8, biw: u8, threads: usize) -> Result<(), ()> {
    let _guard = CleanupGuard { path: BENCH_FILE };

    let stream_count = 100usize;
    let stream_size = 10 * 1024usize;

    // Phase 1: populate (single-threaded)
    {
        let sfs = Sfs::create(BENCH_FILE, biw, bss).map_err(|e| eprintln!("Error: {}", e))?;
        let buf = make_buffer(stream_size);
        for i in 0..stream_count {
            let name = format!("stream_{:04}.bin", i);
            let handle = sfs
                .create_stream(&name)
                .map_err(|e| eprintln!("Error: {}", e))?;
            sfs.write(&handle, &buf)
                .map_err(|e| eprintln!("Error: {}", e))?;
            sfs.close_stream(handle)
                .map_err(|e| eprintln!("Error: {}", e))?;
        }
        sfs.close();
    }

    // Phase 2: concurrent reads
    let sfs =
        Arc::new(Sfs::open(BENCH_FILE, OpenMode::Read).map_err(|e| eprintln!("Error: {}", e))?);

    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let sfs = Arc::clone(&sfs);
            thread::spawn(move || -> Result<(), String> {
                for i in 0..stream_count {
                    let name = format!("stream_{:04}.bin", i);
                    let handle = sfs
                        .open_stream(&name, OpenMode::Read)
                        .map_err(|e| e.to_string())?;
                    let len = sfs.stream_length(&handle).map_err(|e| e.to_string())? as usize;
                    let mut buf = vec![0u8; len];
                    sfs.read(&handle, &mut buf).map_err(|e| e.to_string())?;
                    sfs.close_stream(handle).map_err(|e| e.to_string())?;
                }
                Ok(())
            })
        })
        .collect();

    let mut failed = false;
    for h in handles {
        if let Err(e) = h.join().unwrap_or(Err("thread panicked".to_string())) {
            eprintln!("Thread error: {}", e);
            failed = true;
        }
    }

    match Arc::try_unwrap(sfs) {
        Ok(s) => s.close(),
        Err(_) => eprintln!("Warning: could not unwrap Arc for close"),
    }

    if failed {
        Err(())
    } else {
        Ok(())
    }
}

/// Write 20 streams of 512KB then delete them all, repeated 5 times.
fn run_churn(bss: u8, biw: u8) -> Result<(), ()> {
    let _guard = CleanupGuard { path: BENCH_FILE };
    let sfs = Sfs::create(BENCH_FILE, biw, bss).map_err(|e| eprintln!("Error: {}", e))?;
    let buf = make_buffer(512 * 1024);
    let n = 20usize;

    for _ in 0..5 {
        // Write phase
        for i in 0..n {
            let name = format!("churn_{:04}.bin", i);
            let handle = sfs
                .create_stream(&name)
                .map_err(|e| eprintln!("Error: {}", e))?;
            sfs.write(&handle, &buf)
                .map_err(|e| eprintln!("Error: {}", e))?;
            sfs.close_stream(handle)
                .map_err(|e| eprintln!("Error: {}", e))?;
        }

        // Delete phase
        for i in 0..n {
            let name = format!("churn_{:04}.bin", i);
            sfs.delete_stream(&name)
                .map_err(|e| eprintln!("Error: {}", e))?;
        }
    }

    sfs.close();
    Ok(())
}

/// Run all scenarios in sequence.
fn run_all(bss: u8, biw: u8, threads: usize) -> Result<(), ()> {
    eprintln!("[large-write]");
    run_large_write(bss, biw)?;

    eprintln!("[small-write]");
    run_small_write(bss, biw)?;

    eprintln!("[threaded-write]");
    run_threaded_write(bss, biw, threads)?;

    eprintln!("[threaded-read]");
    run_threaded_read(bss, biw, threads)?;

    eprintln!("[churn]");
    run_churn(bss, biw)?;

    Ok(())
}

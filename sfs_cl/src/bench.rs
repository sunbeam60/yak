use std::sync::Arc;
use std::thread;
use std::time::Instant;

use stream_fs::{OpenMode, SfsDefault as Sfs, StreamHandle};

use crate::error::{CliError, CliResult};
use crate::helpers::{DEFAULT_BLOCK_INDEX_WIDTH, DEFAULT_BLOCK_SIZE_SHIFT};

const BENCH_FILE: &str = "_bench.sfs";
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

/// Default virtual block size shift: 15 (2^15 = 32KB virtual blocks)
const DEFAULT_VBSS: u8 = 15;

struct BenchArgs {
    scenario: String,
    bss: u8,
    biw: u8,
    vbss: u8,
    threads: usize,
}

fn parse_bench_args(args: &[String]) -> Result<BenchArgs, CliError> {
    if args.is_empty() {
        print_bench_usage();
        return Err(CliError::new("No scenario specified"));
    }

    let scenario = args[0].clone();
    let mut bss = DEFAULT_BLOCK_SIZE_SHIFT;
    let mut biw = DEFAULT_BLOCK_INDEX_WIDTH;
    let mut vbss = DEFAULT_VBSS;
    let mut threads = DEFAULT_THREADS;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--block-shift" => {
                i += 1;
                bss = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| CliError::new("--block-shift requires a numeric argument"))?;
            }
            "--index-width" => {
                i += 1;
                biw = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| CliError::new("--index-width requires a numeric argument"))?;
            }
            "--vbss" => {
                i += 1;
                vbss = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| CliError::new("--vbss requires a numeric argument"))?;
            }
            "--threads" => {
                i += 1;
                threads = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| CliError::new("--threads requires a numeric argument"))?;
            }
            other => {
                print_bench_usage();
                return Err(CliError::new(format!("Unknown option: {}", other)));
            }
        }
        i += 1;
    }

    Ok(BenchArgs {
        scenario,
        bss,
        biw,
        vbss,
        threads,
    })
}

fn print_bench_usage() {
    eprintln!(
        "Usage: sfs bench <scenario> [--block-shift N] [--index-width N] [--vbss N] [--threads N]"
    );
    eprintln!();
    eprintln!("Each scenario runs twice: first uncompressed, then compressed.");
    eprintln!();
    eprintln!("Scenarios:");
    eprintln!("  large-write      Write 5 streams of 30MB each");
    eprintln!("  small-write      Write 180 streams of 10KB each");
    eprintln!("  large-read       Read back 17x10MB + 17x20MB (510MB total)");
    eprintln!("  small-read       Read back 2750 streams of 10KB each");
    eprintln!("  threaded-write   T threads writing streams concurrently");
    eprintln!("  threaded-read    T threads reading streams concurrently");
    eprintln!("  threaded-mixed   T/2 readers + T/2 writers concurrently");
    eprintln!("  churn            Write then delete many streams (5 rounds)");
    eprintln!("  reuse            Fresh vs recycled-block read throughput");
    eprintln!("  single-stream    Write + read one 64MB stream (contiguous I/O test)");
    eprintln!("  warm-read        Write then read 2750x10KB streams (same instance, warm cache)");
    eprintln!("  all              Run all scenarios in sequence");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --block-shift N  Block size as power of 2 (default: 12 = 4KB blocks)");
    eprintln!("                   Examples: 10 = 1KB, 12 = 4KB, 16 = 64KB");
    eprintln!("  --index-width N  Block index size (default: 4 = 4B index / 32 bits)");
    eprintln!("                   Examples: 2 = 2B index, 4 = 4B index, 8 = 8B index");
    eprintln!("  --vbss N         Virtual block size shift for compression (default: 15 = 32KB)");
    eprintln!("                   Examples: 13 = 8KB, 15 = 32KB, 17 = 128KB");
    eprintln!("  --threads N      Thread count for threaded scenarios (default: 4)");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn cmd_bench(args: &[String]) -> CliResult {
    let a = parse_bench_args(args)?;

    match a.scenario.as_str() {
        "large-write" => run_dual(a.vbss, |v| run_large_write(a.bss, a.biw, v)).map(|_| ()),
        "small-write" => run_dual(a.vbss, |v| run_small_write(a.bss, a.biw, v)).map(|_| ()),
        "large-read" => run_dual(a.vbss, |v| run_large_read(a.bss, a.biw, v)).map(|_| ()),
        "small-read" => run_dual(a.vbss, |v| run_small_read(a.bss, a.biw, v)).map(|_| ()),
        "threaded-write" => {
            run_dual(a.vbss, |v| run_threaded_write(a.bss, a.biw, v, a.threads)).map(|_| ())
        }
        "threaded-read" => {
            run_dual(a.vbss, |v| run_threaded_read(a.bss, a.biw, v, a.threads)).map(|_| ())
        }
        "threaded-mixed" => {
            run_dual(a.vbss, |v| run_threaded_mixed(a.bss, a.biw, v, a.threads)).map(|_| ())
        }
        "churn" => run_dual(a.vbss, |v| run_churn(a.bss, a.biw, v)).map(|_| ()),
        "reuse" => run_dual(a.vbss, |v| run_reuse(a.bss, a.biw, v)).map(|_| ()),
        "single-stream" => run_dual(a.vbss, |v| run_single_stream(a.bss, a.biw, v)).map(|_| ()),
        "warm-read" => run_dual(a.vbss, |v| run_warm_read(a.bss, a.biw, v)).map(|_| ()),
        "all" => run_all(a.bss, a.biw, a.vbss, a.threads),
        other => {
            print_bench_usage();
            Err(CliError::new(format!("Unknown bench scenario: {}", other)))
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Shorthand for the repeated .map_err pattern in benchmarks.
fn err(e: impl std::fmt::Display) -> CliError {
    CliError::new(format!("Benchmark error: {}", e))
}

/// Create an SFS file. Always has compression support.
fn create_bench_sfs(path: &str, biw: u8, bss: u8, vbss: u8) -> Result<Sfs, CliError> {
    if vbss > 0 {
        Sfs::create_with_vbss(path, biw, bss, vbss).map_err(err)
    } else {
        Sfs::create(path, biw, bss).map_err(err)
    }
}

/// Create a stream. Uses compressed mode when vbss > 0.
fn create_bench_stream(sfs: &Sfs, name: &str, vbss: u8) -> Result<StreamHandle, CliError> {
    sfs.create_stream(name, vbss > 0).map_err(err)
}

/// Run a benchmark in both uncompressed and compressed modes.
/// Returns (uncompressed_ms, compressed_ms) — only the measured portions.
fn run_dual(vbss: u8, f: impl Fn(u8) -> Result<f64, CliError>) -> Result<(f64, f64), CliError> {
    eprintln!("  [uncompressed]");
    let uncomp = f(0)?;
    eprintln!(
        "  [compressed]  (vbss={}, virtual block = {}KB)",
        vbss,
        1u64 << vbss.saturating_sub(10)
    );
    let comp = f(vbss)?;
    Ok((uncomp, comp))
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// Write 5 streams of 30MB each.
fn run_large_write(bss: u8, biw: u8, vbss: u8) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };
    let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;
    let buf = make_buffer(30 * 1024 * 1024);

    let t0 = Instant::now();
    for i in 0..5 {
        let name = format!("large_{}.bin", i);
        let handle = create_bench_stream(&sfs, &name, vbss)?;
        sfs.write(&handle, &buf).map_err(err)?;
        sfs.close_stream(handle).map_err(err)?;
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("    elapsed: {:.0} ms", elapsed_ms);

    sfs.close().map_err(err)?;
    Ok(elapsed_ms)
}

/// Write 180 streams of 10KB each.
fn run_small_write(bss: u8, biw: u8, vbss: u8) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };
    let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;
    let buf = make_buffer(10 * 1024);

    let t0 = Instant::now();
    for i in 0..180 {
        let name = format!("stream_{:04}.bin", i);
        let handle = create_bench_stream(&sfs, &name, vbss)?;
        sfs.write(&handle, &buf).map_err(err)?;
        sfs.close_stream(handle).map_err(err)?;
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("    elapsed: {:.0} ms", elapsed_ms);

    sfs.close().map_err(err)?;
    Ok(elapsed_ms)
}

/// T threads, each writing a partition of 125 streams (100KB each).
fn run_threaded_write(bss: u8, biw: u8, vbss: u8, threads: usize) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };
    let sfs = Arc::new(create_bench_sfs(BENCH_FILE, biw, bss, vbss)?);
    let buf = Arc::new(make_buffer(100 * 1024));

    let total_streams = 125usize;
    let per_thread = total_streams / threads;
    let remainder = total_streams % threads;

    let t0 = Instant::now();
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let sfs = Arc::clone(&sfs);
            let buf = Arc::clone(&buf);
            let count = per_thread + if t < remainder { 1 } else { 0 };
            let offset = t * per_thread + t.min(remainder);
            thread::spawn(move || -> Result<(), String> {
                for i in 0..count {
                    let name = format!("stream_{:04}.bin", offset + i);
                    let handle = sfs
                        .create_stream(&name, vbss > 0)
                        .map_err(|e| e.to_string())?;
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
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("    elapsed: {:.0} ms", elapsed_ms);

    // All threads joined — we are the sole Arc owner
    match Arc::try_unwrap(sfs) {
        Ok(s) => {
            if let Err(e) = s.close() {
                eprintln!("Error closing SFS: {}", e);
            }
        }
        Err(_) => eprintln!("Warning: could not unwrap Arc for close"),
    }

    if failed {
        Err(CliError::new("One or more threads failed"))
    } else {
        Ok(elapsed_ms)
    }
}

/// Populate streams, then T threads each read all streams.
fn run_threaded_read(bss: u8, biw: u8, vbss: u8, threads: usize) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };

    let stream_count = 230usize;
    let stream_size = 500 * 1024usize;

    // Phase 1: populate (single-threaded)
    {
        let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;
        let buf = make_buffer(stream_size);
        for i in 0..stream_count {
            let name = format!("stream_{:04}.bin", i);
            let handle = create_bench_stream(&sfs, &name, vbss)?;
            sfs.write(&handle, &buf).map_err(err)?;
            sfs.close_stream(handle).map_err(err)?;
        }
        sfs.close().map_err(err)?;
    }

    // Phase 2: concurrent reads
    let sfs = Arc::new(Sfs::open(BENCH_FILE, OpenMode::Read).map_err(err)?);

    let t0 = Instant::now();
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
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("    elapsed: {:.0} ms", elapsed_ms);

    match Arc::try_unwrap(sfs) {
        Ok(s) => {
            if let Err(e) = s.close() {
                eprintln!("Error closing SFS: {}", e);
            }
        }
        Err(_) => eprintln!("Warning: could not unwrap Arc for close"),
    }

    if failed {
        Err(CliError::new("One or more threads failed"))
    } else {
        Ok(elapsed_ms)
    }
}

/// Populate 17x10MB + 17x20MB streams, then read them all back (510MB total).
fn run_large_read(bss: u8, biw: u8, vbss: u8) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };

    let size_10mb = 10 * 1024 * 1024usize;
    let size_20mb = 20 * 1024 * 1024usize;
    let streams_per_tier = 17usize;

    // Phase 1: populate
    {
        let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;

        let buf_10 = make_buffer(size_10mb);
        for i in 0..streams_per_tier {
            let name = format!("large_10m_{}.bin", i);
            let handle = create_bench_stream(&sfs, &name, vbss)?;
            sfs.write(&handle, &buf_10).map_err(err)?;
            sfs.close_stream(handle).map_err(err)?;
        }

        let buf_20 = make_buffer(size_20mb);
        for i in 0..streams_per_tier {
            let name = format!("large_20m_{}.bin", i);
            let handle = create_bench_stream(&sfs, &name, vbss)?;
            sfs.write(&handle, &buf_20).map_err(err)?;
            sfs.close_stream(handle).map_err(err)?;
        }

        sfs.close().map_err(err)?;
    }

    // Phase 2: read back
    let sfs = Sfs::open(BENCH_FILE, OpenMode::Read).map_err(err)?;

    let t0 = Instant::now();
    for i in 0..streams_per_tier {
        let name = format!("large_10m_{}.bin", i);
        let handle = sfs.open_stream(&name, OpenMode::Read).map_err(err)?;
        let len = sfs.stream_length(&handle).map_err(err)? as usize;
        let mut buf = vec![0u8; len];
        sfs.read(&handle, &mut buf).map_err(err)?;
        sfs.close_stream(handle).map_err(err)?;
    }

    for i in 0..streams_per_tier {
        let name = format!("large_20m_{}.bin", i);
        let handle = sfs.open_stream(&name, OpenMode::Read).map_err(err)?;
        let len = sfs.stream_length(&handle).map_err(err)? as usize;
        let mut buf = vec![0u8; len];
        sfs.read(&handle, &mut buf).map_err(err)?;
        sfs.close_stream(handle).map_err(err)?;
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("    elapsed: {:.0} ms", elapsed_ms);

    sfs.close().map_err(err)?;
    Ok(elapsed_ms)
}

/// Populate 2750 streams of 10KB, then read them all back.
fn run_small_read(bss: u8, biw: u8, vbss: u8) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };

    let stream_count = 2750usize;
    let stream_size = 10 * 1024usize;

    // Phase 1: populate
    {
        let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;
        let buf = make_buffer(stream_size);
        for i in 0..stream_count {
            let name = format!("stream_{:04}.bin", i);
            let handle = create_bench_stream(&sfs, &name, vbss)?;
            sfs.write(&handle, &buf).map_err(err)?;
            sfs.close_stream(handle).map_err(err)?;
        }
        sfs.close().map_err(err)?;
    }

    // Phase 2: read back
    let sfs = Sfs::open(BENCH_FILE, OpenMode::Read).map_err(err)?;

    let t0 = Instant::now();
    for i in 0..stream_count {
        let name = format!("stream_{:04}.bin", i);
        let handle = sfs.open_stream(&name, OpenMode::Read).map_err(err)?;
        let len = sfs.stream_length(&handle).map_err(err)? as usize;
        let mut buf = vec![0u8; len];
        sfs.read(&handle, &mut buf).map_err(err)?;
        sfs.close_stream(handle).map_err(err)?;
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("    elapsed: {:.0} ms", elapsed_ms);

    sfs.close().map_err(err)?;
    Ok(elapsed_ms)
}

/// Write 20 streams of 512KB then delete them all, repeated 5 times.
fn run_churn(bss: u8, biw: u8, vbss: u8) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };
    let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;
    let buf = make_buffer(512 * 1024);
    let n = 20usize;

    let t0 = Instant::now();
    for _ in 0..5 {
        // Write phase
        for i in 0..n {
            let name = format!("churn_{:04}.bin", i);
            let handle = create_bench_stream(&sfs, &name, vbss)?;
            sfs.write(&handle, &buf).map_err(err)?;
            sfs.close_stream(handle).map_err(err)?;
        }

        // Delete phase
        for i in 0..n {
            let name = format!("churn_{:04}.bin", i);
            sfs.delete_stream(&name).map_err(err)?;
        }
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("    elapsed: {:.0} ms", elapsed_ms);

    sfs.close().map_err(err)?;
    Ok(elapsed_ms)
}

/// Write streams, read them (baseline), delete all, re-write into recycled
/// blocks, then read again. Compares "fresh" vs "reused-block" read throughput.
fn run_reuse(bss: u8, biw: u8, vbss: u8) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };
    let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;
    let stream_size = 512 * 1024usize;
    let n = 50usize;
    let buf = make_buffer(stream_size);

    // Phase 1: populate (fresh contiguous allocation)
    for i in 0..n {
        let name = format!("reuse_{:04}.bin", i);
        let h = create_bench_stream(&sfs, &name, vbss)?;
        sfs.write(&h, &buf).map_err(err)?;
        sfs.close_stream(h).map_err(err)?;
    }

    // Phase 2: read baseline (fresh contiguous blocks)
    let t_fresh = Instant::now();
    for i in 0..n {
        let name = format!("reuse_{:04}.bin", i);
        let h = sfs.open_stream(&name, OpenMode::Read).map_err(err)?;
        let mut rbuf = vec![0u8; stream_size];
        sfs.read(&h, &mut rbuf).map_err(err)?;
        sfs.close_stream(h).map_err(err)?;
    }
    let fresh_ms = t_fresh.elapsed().as_secs_f64() * 1000.0;

    // Phase 3: delete all
    for i in 0..n {
        let name = format!("reuse_{:04}.bin", i);
        sfs.delete_stream(&name).map_err(err)?;
    }

    // Phase 4: repopulate (recycled blocks)
    for i in 0..n {
        let name = format!("reuse_{:04}.bin", i);
        let h = create_bench_stream(&sfs, &name, vbss)?;
        sfs.write(&h, &buf).map_err(err)?;
        sfs.close_stream(h).map_err(err)?;
    }

    // Phase 5: read after reuse
    let t_reuse = Instant::now();
    for i in 0..n {
        let name = format!("reuse_{:04}.bin", i);
        let h = sfs.open_stream(&name, OpenMode::Read).map_err(err)?;
        let mut rbuf = vec![0u8; stream_size];
        sfs.read(&h, &mut rbuf).map_err(err)?;
        sfs.close_stream(h).map_err(err)?;
    }
    let reuse_ms = t_reuse.elapsed().as_secs_f64() * 1000.0;

    let mb = (n * stream_size) as f64 / (1024.0 * 1024.0);
    eprintln!(
        "    fresh read: {:.0} ms  ({:.1} MB/s)",
        fresh_ms,
        mb / (fresh_ms / 1000.0)
    );
    eprintln!(
        "    reuse read: {:.0} ms  ({:.1} MB/s)",
        reuse_ms,
        mb / (reuse_ms / 1000.0)
    );

    sfs.close().map_err(err)?;
    Ok(fresh_ms + reuse_ms)
}

/// Populate 60 streams, then T/2 threads read while T/2 threads write concurrently.
fn run_threaded_mixed(bss: u8, biw: u8, vbss: u8, threads: usize) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };

    let stream_count = 60usize;
    let stream_size = 100 * 1024usize;
    let write_total = 150usize;

    // Phase 1: populate streams for readers (single-threaded)
    {
        let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;
        let buf = make_buffer(stream_size);
        for i in 0..stream_count {
            let name = format!("read_{:04}.bin", i);
            let handle = create_bench_stream(&sfs, &name, vbss)?;
            sfs.write(&handle, &buf).map_err(err)?;
            sfs.close_stream(handle).map_err(err)?;
        }
        sfs.close().map_err(err)?;
    }

    // Phase 2: mixed concurrent access
    let sfs = Arc::new(Sfs::open(BENCH_FILE, OpenMode::Write).map_err(err)?);
    let buf = Arc::new(make_buffer(stream_size));

    let readers = threads / 2;
    let writers = threads - readers;
    let per_writer = write_total / writers;
    let write_remainder = write_total % writers;

    let t0 = Instant::now();
    let mut join_handles = Vec::with_capacity(threads);

    // Reader threads: each reads all pre-populated streams
    for _ in 0..readers {
        let sfs = Arc::clone(&sfs);
        join_handles.push(thread::spawn(move || -> Result<(), String> {
            for i in 0..stream_count {
                let name = format!("read_{:04}.bin", i);
                let handle = sfs
                    .open_stream(&name, OpenMode::Read)
                    .map_err(|e| e.to_string())?;
                let len = sfs.stream_length(&handle).map_err(|e| e.to_string())? as usize;
                let mut rbuf = vec![0u8; len];
                sfs.read(&handle, &mut rbuf).map_err(|e| e.to_string())?;
                sfs.close_stream(handle).map_err(|e| e.to_string())?;
            }
            Ok(())
        }));
    }

    // Writer threads: each creates and writes its partition of new streams
    for t in 0..writers {
        let sfs = Arc::clone(&sfs);
        let buf = Arc::clone(&buf);
        let count = per_writer + if t < write_remainder { 1 } else { 0 };
        let offset = t * per_writer + t.min(write_remainder);
        join_handles.push(thread::spawn(move || -> Result<(), String> {
            for i in 0..count {
                let name = format!("write_{:04}.bin", offset + i);
                let handle = sfs
                    .create_stream(&name, vbss > 0)
                    .map_err(|e| e.to_string())?;
                sfs.write(&handle, &buf).map_err(|e| e.to_string())?;
                sfs.close_stream(handle).map_err(|e| e.to_string())?;
            }
            Ok(())
        }));
    }

    let mut failed = false;
    for h in join_handles {
        if let Err(e) = h.join().unwrap_or(Err("thread panicked".to_string())) {
            eprintln!("Thread error: {}", e);
            failed = true;
        }
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("    elapsed: {:.0} ms", elapsed_ms);

    match Arc::try_unwrap(sfs) {
        Ok(s) => {
            if let Err(e) = s.close() {
                eprintln!("Error closing SFS: {}", e);
            }
        }
        Err(_) => eprintln!("Warning: could not unwrap Arc for close"),
    }

    if failed {
        Err(CliError::new("One or more threads failed"))
    } else {
        Ok(elapsed_ms)
    }
}

/// Write one large stream then read it back. Times write and read separately.
/// Designed to measure contiguous block I/O performance.
fn run_single_stream(bss: u8, biw: u8, vbss: u8) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };

    let stream_size = 64 * 1024 * 1024usize; // 64 MB
    let buf = make_buffer(stream_size);

    // Phase 1: write
    let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;
    let handle = create_bench_stream(&sfs, "big.bin", vbss)?;

    let t_write = Instant::now();
    sfs.write(&handle, &buf).map_err(err)?;
    let write_ms = t_write.elapsed().as_secs_f64() * 1000.0;

    sfs.close_stream(handle).map_err(err)?;
    sfs.close().map_err(err)?;

    // Phase 2: reopen and read back
    let sfs = Sfs::open(BENCH_FILE, OpenMode::Read).map_err(err)?;
    let handle = sfs.open_stream("big.bin", OpenMode::Read).map_err(err)?;
    let len = sfs.stream_length(&handle).map_err(err)? as usize;
    let mut read_buf = vec![0u8; len];

    let t_read = Instant::now();
    let n = sfs.read(&handle, &mut read_buf).map_err(err)?;
    let read_ms = t_read.elapsed().as_secs_f64() * 1000.0;

    sfs.close_stream(handle).map_err(err)?;
    sfs.close().map_err(err)?;

    let mb = stream_size as f64 / (1024.0 * 1024.0);
    eprintln!(
        "    write: {:.0} ms  ({:.1} MB/s)",
        write_ms,
        mb / (write_ms / 1000.0)
    );
    eprintln!(
        "    read:  {:.0} ms  ({:.1} MB/s)  [{} bytes]",
        read_ms,
        mb / (read_ms / 1000.0),
        n
    );

    Ok(write_ms + read_ms)
}

/// Write 2750 streams of 10KB, then read them all back using the same Sfs instance.
/// This tests the benefit of warm block caches across many open/read/close cycles.
fn run_warm_read(bss: u8, biw: u8, vbss: u8) -> Result<f64, CliError> {
    let _guard = CleanupGuard { path: BENCH_FILE };

    let stream_count = 2750usize;
    let stream_size = 10 * 1024usize;

    let sfs = create_bench_sfs(BENCH_FILE, biw, bss, vbss)?;
    let buf = make_buffer(stream_size);

    // Phase 1: populate (not timed)
    for i in 0..stream_count {
        let name = format!("stream_{:04}.bin", i);
        let handle = create_bench_stream(&sfs, &name, vbss)?;
        sfs.write(&handle, &buf).map_err(err)?;
        sfs.close_stream(handle).map_err(err)?;
    }

    // Phase 2: read back with warm cache (timed)
    let t0 = Instant::now();
    for i in 0..stream_count {
        let name = format!("stream_{:04}.bin", i);
        let handle = sfs.open_stream(&name, OpenMode::Read).map_err(err)?;
        let len = sfs.stream_length(&handle).map_err(err)? as usize;
        let mut read_buf = vec![0u8; len];
        sfs.read(&handle, &mut read_buf).map_err(err)?;
        sfs.close_stream(handle).map_err(err)?;
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!("    elapsed: {:.0} ms", elapsed_ms);

    sfs.close().map_err(err)?;
    Ok(elapsed_ms)
}

/// Helper to run a named scenario via run_dual and accumulate timings.
macro_rules! bench_scenario {
    ($name:expr, $uncomp:ident, $comp:ident, $vbss:expr, $body:expr) => {{
        eprintln!("[{}]", $name);
        let (u, c) = run_dual($vbss, $body)?;
        $uncomp += u;
        $comp += c;
    }};
}

/// Run all scenarios in sequence and print aggregate totals.
fn run_all(bss: u8, biw: u8, vbss: u8, threads: usize) -> CliResult {
    let mut total_uncomp = 0.0f64;
    let mut total_comp = 0.0f64;

    bench_scenario!("large-write", total_uncomp, total_comp, vbss, |v| {
        run_large_write(bss, biw, v)
    });
    bench_scenario!("small-write", total_uncomp, total_comp, vbss, |v| {
        run_small_write(bss, biw, v)
    });
    bench_scenario!("large-read", total_uncomp, total_comp, vbss, |v| {
        run_large_read(bss, biw, v)
    });
    bench_scenario!("small-read", total_uncomp, total_comp, vbss, |v| {
        run_small_read(bss, biw, v)
    });
    bench_scenario!("threaded-write", total_uncomp, total_comp, vbss, |v| {
        run_threaded_write(bss, biw, v, threads)
    });
    bench_scenario!("threaded-read", total_uncomp, total_comp, vbss, |v| {
        run_threaded_read(bss, biw, v, threads)
    });
    bench_scenario!("threaded-mixed", total_uncomp, total_comp, vbss, |v| {
        run_threaded_mixed(bss, biw, v, threads)
    });
    bench_scenario!("churn", total_uncomp, total_comp, vbss, |v| {
        run_churn(bss, biw, v)
    });
    bench_scenario!("reuse", total_uncomp, total_comp, vbss, |v| {
        run_reuse(bss, biw, v)
    });
    bench_scenario!("single-stream", total_uncomp, total_comp, vbss, |v| {
        run_single_stream(bss, biw, v)
    });
    bench_scenario!("warm-read", total_uncomp, total_comp, vbss, |v| {
        run_warm_read(bss, biw, v)
    });

    eprintln!();
    eprintln!(
        "=== Total uncompressed: {:.0} ms | compressed: {:.0} ms ===",
        total_uncomp, total_comp
    );

    Ok(())
}

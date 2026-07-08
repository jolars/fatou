//! Warm-loop throughput harness for Fatou's formatter, used by the cross-tool
//! benchmark against Runic and JuliaFormatter (see `bench/compare_format.sh`).
//!
//! This is a plain `main` (Cargo bench with `harness = false`), not a Criterion
//! bench: it must emit the same JSON schema as the Julia harness
//! (`bench/julia_bench.jl`) so the two can be merged. It measures the pure
//! `format(&str)` call in a warm loop, so process startup and any first-call
//! costs are excluded and the number is directly comparable to the Julia tools'
//! warm `format_string`/`format_text` timings.
//!
//! Driven entirely by environment variables:
//!   FATOU_BENCH_FILELIST     path to a file with one source path per line (required)
//!   FATOU_BENCH_ITERATIONS   timed iterations per file (default 50)
//!   FATOU_BENCH_WARMUP       warmup iterations per file (default 3)
//!   FATOU_BENCH_OUTPUT_JSON  output path (default: stdout)

use std::fs;
use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

use fatou::formatter::format;
use serde_json::{Value, json};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn stats(samples: &[u128]) -> (u128, u128, f64, f64) {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let min = sorted[0];
    let median = sorted[sorted.len() / 2];
    let mean = sorted.iter().map(|&n| n as f64).sum::<f64>() / sorted.len() as f64;
    let var = sorted
        .iter()
        .map(|&n| (n as f64 - mean).powi(2))
        .sum::<f64>()
        / sorted.len() as f64;
    (min, median, mean, var.sqrt())
}

fn bench_file(path: &str, iterations: usize, warmup: usize) -> Value {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return json!({ "path": path, "ok": false, "error": format!("read: {e}") });
        }
    };
    let bytes = src.len();

    // Sanity gate: the file counts only if Fatou can format it without error.
    for _ in 0..warmup.max(1) {
        if let Err(e) = format(&src) {
            return json!({
                "path": path, "bytes": bytes, "ok": false,
                "error": format!("format: {e:?}"),
            });
        }
    }

    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let out = format(&src);
        let elapsed = start.elapsed().as_nanos();
        black_box(out.ok());
        samples.push(elapsed);
    }

    let (min, median, mean, stddev) = stats(&samples);
    json!({
        "path": path,
        "bytes": bytes,
        "ok": true,
        "min_ns": min,
        "median_ns": median,
        "mean_ns": mean,
        "stddev_ns": stddev,
    })
}

fn main() -> ExitCode {
    let filelist = match std::env::var("FATOU_BENCH_FILELIST") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("FATOU_BENCH_FILELIST is required");
            return ExitCode::FAILURE;
        }
    };
    let iterations = env_usize("FATOU_BENCH_ITERATIONS", 50);
    let warmup = env_usize("FATOU_BENCH_WARMUP", 3);

    let listing = match fs::read_to_string(&filelist) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read file list {filelist}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let files: Vec<&str> = listing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    let results: Vec<Value> = files
        .iter()
        .map(|path| bench_file(path, iterations, warmup))
        .collect();

    let report = json!({
        "tool": "fatou",
        "version": env!("CARGO_PKG_VERSION"),
        "iterations": iterations,
        "warmup": warmup,
        "files": results,
    });

    let out = serde_json::to_string_pretty(&report).expect("serialize report");
    match std::env::var("FATOU_BENCH_OUTPUT_JSON") {
        Ok(path) => {
            if let Err(e) = fs::write(&path, out) {
                eprintln!("cannot write {path}: {e}");
                return ExitCode::FAILURE;
            }
        }
        Err(_) => println!("{out}"),
    }
    ExitCode::SUCCESS
}

#!/usr/bin/env python3
"""Measure elapsed time and peak RSS for fresh formatter processes on one file.

The warm-loop harnesses (benches/format_compare.rs, bench/julia_bench.jl) load
each tool once and time a hot inner loop, deliberately excluding process startup
and first-call JIT so the numbers reflect a long-lived editor or language-server
session. This harness measures the opposite: the end-to-end cost a command-line
user pays on the first run. Each iteration spawns a brand-new process that starts
up, loads the tool (paying Julia's package load and first-call compilation for the
Julia tools), formats the single file once, and exits.

  - Fatou runs through `fatou format` (stdin -> stdout, built-in defaults).
  - Runic and JuliaFormatter run through the same `julia -e 'using X; ...'` path a
    shell user would take, so Julia startup, package load, and JIT all count.

We emit the same per-file JSON report shape as bench/julia_bench.jl (a list of
tools, each with its ok/skipped file records) so bench/merge.py can fold the
result in as a `cold_start` scenario. Only the single-file scenario is measured;
cold-start time is dominated by fixed startup and compilation cost, not file size.

Usage:
  cold_start.py --file <path> --iterations N --out <out.json> \
      --fatou <fatou-binary> [--julia julia] [--julia-project <path>]
"""

import argparse
import json
import os
import statistics
import subprocess
import time
from contextlib import nullcontext
from pathlib import Path

from cli_memory import MEASUREMENT, find_gnu_time

# String -> String formatting scripts for the Julia tools. `write(devnull, ...)`
# keeps the result live so the format call is not elided, without any real IO.
RUNIC_SCRIPT = "using Runic; write(devnull, Runic.format_string(read(ARGS[1], String)))"
JLFMT_SCRIPT = "using JuliaFormatter; write(devnull, JuliaFormatter.format_text(read(ARGS[1], String)))"


def run_once(cmd, stdin_path):
    """Run `cmd` as a fresh process and capture elapsed time and diagnostics."""
    with (
        Path(stdin_path).open("rb") if stdin_path else nullcontext(subprocess.DEVNULL)
    ) as stdin_file:
        t0 = time.perf_counter_ns()
        proc = subprocess.run(
            cmd,
            stdin=stdin_file,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            check=False,
        )
        t1 = time.perf_counter_ns()
    return t1 - t0, proc


def failure_reason(proc):
    stderr = proc.stderr.decode("utf-8", "replace")
    return " ".join(stderr.split())[:200] or f"exit code {proc.returncode}"


def measure(gnu_time, name, cmd, stdin_path, path, iterations):
    """Probe once, then collect time and peak RSS over fresh runs."""
    bytes_ = os.path.getsize(path)

    # Probe: a nonzero exit here means the tool is unavailable or crashed, so we
    # report the file skipped rather than timing a broken invocation. The probe
    # also pays any one-time precompilation, keeping it out of the samples.
    _, probe = run_once(cmd, stdin_path)
    if probe.returncode != 0:
        reason = failure_reason(probe)
        return {
            "tool": name,
            "available": False,
            "version": None,
            "files": [
                {"path": str(path), "bytes": bytes_, "ok": False, "error": reason}
            ],
        }

    # Measure RSS separately so launching GNU time does not inflate latency.
    memory_cmd = [gnu_time, "-f", "FATOU_BENCH maxrss_kb=%M wall=%e rc=%x", *cmd]
    samples = []
    peaks = []
    for _ in range(iterations):
        elapsed, proc = run_once(cmd, stdin_path)
        if proc.returncode == 0:
            _, proc = run_once(memory_cmd, stdin_path)
        if proc.returncode != 0:
            reason = failure_reason(proc)
            return {
                "tool": name,
                "available": True,
                "version": None,
                "files": [
                    {"path": str(path), "bytes": bytes_, "ok": False, "error": reason}
                ],
            }
        reading = MEASUREMENT.search(proc.stderr)
        if reading is None:
            raise RuntimeError(f"GNU time reported no peak RSS for {cmd!r}")
        samples.append(elapsed)
        peaks.append(int(reading[1]))

    samples.sort()
    record = {
        "path": str(path),
        "bytes": bytes_,
        "ok": True,
        "min_ns": samples[0],
        "median_ns": statistics.median(samples),
        "median_peak_rss_kib": statistics.median(peaks),
        "mean_ns": statistics.fmean(samples),
        "stddev_ns": statistics.pstdev(samples),
    }
    return {"tool": name, "available": True, "version": None, "files": [record]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--file", required=True, help="single .jl file to format")
    ap.add_argument("--iterations", type=int, default=5)
    ap.add_argument("--out", required=True)
    ap.add_argument("--fatou", required=True, help="path to the fatou release binary")
    ap.add_argument("--julia", default="julia")
    ap.add_argument("--julia-project", default=None)
    ap.add_argument(
        "--time", default=os.environ.get("GNU_TIME"), help="path to GNU time"
    )
    args = ap.parse_args()
    if args.iterations < 1:
        ap.error("--iterations must be positive")
    gnu_time = find_gnu_time(args.time)

    path = Path(args.file)
    julia_flags = ["--startup-file=no"]
    if args.julia_project:
        julia_flags.append(f"--project={args.julia_project}")

    # `fatou format` with no path reads stdin and formats to stdout; `--no-config`
    # uses built-in defaults, matching the warm harness's config-free formatter.
    tools = [
        measure(
            gnu_time,
            "fatou",
            [args.fatou, "format", "--no-config"],
            str(path),
            path,
            args.iterations,
        ),
        measure(
            gnu_time,
            "runic",
            [args.julia, *julia_flags, "-e", RUNIC_SCRIPT, str(path)],
            None,
            path,
            args.iterations,
        ),
        measure(
            gnu_time,
            "juliaformatter",
            [args.julia, *julia_flags, "-e", JLFMT_SCRIPT, str(path)],
            None,
            path,
            args.iterations,
        ),
    ]

    report = {
        "cold_start": True,
        "iterations": args.iterations,
        "target": path.name,
        "tools": tools,
    }
    Path(args.out).write_text(json.dumps(report) + "\n")
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()

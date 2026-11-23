#!/usr/bin/env python3
"""Run the shared LSP workload against two existing Fatou release binaries."""

import argparse
import os
import shlex
import shutil
import subprocess
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--sessions", type=int, default=6)
    parser.add_argument("--latency-runs", type=int, default=20)
    parser.add_argument("--latency-warmups", type=int, default=2)
    parser.add_argument("--quiet-seconds", type=float, default=5)
    parser.add_argument("--settle-timeout", type=float, default=300)
    bench = Path(__file__).resolve().parent
    args = parser.parse_args()
    project = bench / "corpus" / "DataFrames"
    if not (project / "Manifest.toml").is_file():
        parser.error("the pinned DataFrames workspace must be instantiated first")
    files = sorted(
        project.glob("src/**/*.jl"), key=lambda path: (-path.stat().st_size, path)
    )
    logs = args.out.with_suffix(".logs")
    logs.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    julia = shutil.which(env.get("BENCH_JULIA", "julia"))
    if julia:
        env["PATH"] = str(Path(julia).resolve().parent) + os.pathsep + env["PATH"]
    env.setdefault("JULIA_DEPOT_PATH", str(bench / "lsenv" / "depot") + os.pathsep)
    subprocess.run(
        [
            sys.executable,
            str(bench / "lsp_memory.py"),
            "--project",
            str(project),
            "--files",
            *map(str, files[:5]),
            "--ab",
            "--out",
            str(args.out),
            "--stderr-dir",
            str(logs),
            "--server",
            f"baseline={shlex.join([str(args.baseline.resolve()), 'lsp'])}",
            "--server",
            f"candidate={shlex.join([str(args.candidate.resolve()), 'lsp'])}",
            "--sessions",
            str(args.sessions),
            "--latency-runs",
            str(args.latency_runs),
            "--latency-warmups",
            str(args.latency_warmups),
            "--quiet-seconds",
            str(args.quiet_seconds),
            "--settle-timeout",
            str(args.settle_timeout),
        ],
        check=True,
        env=env,
    )


if __name__ == "__main__":
    main()

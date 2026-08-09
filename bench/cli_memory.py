#!/usr/bin/env python3
"""Peak resident memory of one-shot `fatou` runs.

The language-server harness next door measures a process that stays resident;
this measures the other way people run Fatou -- once, from a shell or a CI job,
against a file or a tree -- where what matters is the high-water mark of a
process that lives for a few milliseconds.

That short life is also why the measurement is farmed out to GNU `time` instead
of being taken in-process. `ru_maxrss` is a high-water mark over the child's
whole lifetime, and a forked CPython carries ~12 MB of interpreter into that
mark before `exec` replaces it -- more than anything a Fatou run costs, so the
harness would measure only itself. GNU `time` costs about 1 MB, which the
`true` case in the results keeps honest by pinning that floor.

Usage:
  cli_memory.py --fatou <binary> --project <dir> --out <out.json> \
      [--runs 5] [--time <gnu-time-binary>]
"""

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

SCHEMA_VERSION = 1
MEASUREMENT = re.compile(rb"FATOU_BENCH maxrss_kb=(\d+) wall=([\d.]+) rc=(-?\d+)")


def find_gnu_time(explicit):
    """Locate GNU time, which is not the shell builtin of the same name."""
    candidate = explicit or shutil.which("time") or "/usr/bin/time"
    probe = subprocess.run(
        [candidate, "-f", "%M", "true"], capture_output=True, check=False
    )
    if probe.returncode != 0 or not probe.stderr.strip().isdigit():
        sys.exit(
            f"error: {candidate} is not GNU time (it must accept -f '%M').\n"
            "       Reload the devenv shell, which provides it, or pass --time."
        )
    return candidate


def measure(gnu_time, cmd, cwd, runs):
    """Lowest peak RSS in MB over `runs` fresh invocations, with its wall time.

    The minimum, not the median: peak RSS is bounded below by what the run
    genuinely needed, and anything above that is scheduling noise in how far
    the parallel passes happened to run ahead of each other.
    """
    readings = []
    for _ in range(max(1, runs)):
        proc = subprocess.run(
            [gnu_time, "-f", "FATOU_BENCH maxrss_kb=%M wall=%e rc=%x", *cmd],
            cwd=cwd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            check=False,
        )
        found = MEASUREMENT.search(proc.stderr)
        if not found:
            sys.exit(f"error: no measurement from {cmd!r}: {proc.stderr[-400:]!r}")
        readings.append((int(found[1]) / 1024, float(found[2]), int(found[3])))
    return min(readings)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fatou", required=True)
    parser.add_argument("--project", required=True, help="corpus checkout to run over")
    parser.add_argument("--out", required=True)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--time", default=None, help="path to GNU time")
    args = parser.parse_args()

    gnu_time = find_gnu_time(args.time)
    project = Path(args.project).absolute()
    tree = project / "src"
    single = project / "src/abstractdataframe/abstractdataframe.jl"
    fatou = str(Path(args.fatou).absolute())

    tree_bytes = sum(f.stat().st_size for f in tree.rglob("*.jl"))
    tree_files = sum(1 for _ in tree.rglob("*.jl"))
    single_bytes = single.stat().st_size

    # (key, displayed command, what it ran over, argv, input size). The last two
    # cases are floors, not measurements of Fatou doing work: one for the cost of
    # starting the binary at all, one for the cost of GNU `time` starting
    # anything at all.
    cases = [
        ("format_tree", "fatou format --check", "src tree", [fatou, "format", "--check", str(tree)], tree_bytes),
        ("lint_tree", "fatou lint", "src tree", [fatou, "lint", str(tree)], tree_bytes),
        ("format_file", "fatou format --check", "one file", [fatou, "format", "--check", str(single)], single_bytes),
        ("lint_file", "fatou lint", "one file", [fatou, "lint", str(single)], single_bytes),
        ("parse_file", "fatou parse", "one file", [fatou, "parse", str(single)], single_bytes),
        ("version", "fatou --version", "process floor", [fatou, "--version"], 0),
        ("true", "true", "harness floor", [shutil.which("true") or "/bin/true"], 0),
    ]

    results = []
    for key, command, target, cmd, size in cases:
        rss_mb, wall, rc = measure(gnu_time, cmd, str(project), args.runs)
        results.append(
            {
                "key": key,
                "command": command,
                "target": target,
                "peak_rss_mb": round(rss_mb, 1),
                "seconds": round(wall, 3),
                "input_bytes": size,
                # `format --check` and `lint` exit non-zero when they have
                # something to report, which over a real corpus they do.
                "exit_code": rc,
            }
        )
        print(f"    {command} ({target}): {rss_mb:.1f} MB, {wall:.3f}s", flush=True)

    Path(args.out).write_text(
        json.dumps(
            {
                "schema_version": SCHEMA_VERSION,
                "project": str(project),
                "tree_files": tree_files,
                "tree_bytes": tree_bytes,
                "runs": args.runs,
                "cases": results,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()

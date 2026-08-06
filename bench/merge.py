#!/usr/bin/env python3
"""Merge the Fatou and Julia warm-loop harness outputs into results.json, the
artifact the docs `doc-utils` mdBook preprocessor reads to render the benchmark
chart and fallback tables.

Scenarios are supplied by `compare_format.sh`, one `--scenario` group per
measurement, and their command-line order becomes `scenario_order` in the output
so the docs render them in a deliberate sequence rather than a map's iteration
order. Each scenario carries its own display label and target.

Throughput (MB/s) is computed per tool over the files that tool formatted
successfully, so a tool is never credited for files it could not parse, and the
skipped files are reported explicitly. MB/s normalizes for byte count, so the
numbers remain directly comparable even when tools cover different file sets.

A `project_*` scenario is a single whole-directory measurement per tool (one
record covering the entire tree, produced by the harnesses' directory mode),
which `aggregate()` handles as a degenerate one-file case.
"""

import argparse
import json
from pathlib import Path


def load(path):
    if not path or not Path(path).exists():
        return None
    return json.loads(Path(path).read_text())


def fatou_files(report):
    """{tool: [file_records]} from the Fatou harness output."""
    return {} if report is None else {"fatou": report.get("files", [])}


def julia_tools(report):
    """{tool: [file_records]} from the Julia harness output (skips unavailable)."""
    out = {}
    if report is None:
        return out
    for t in report.get("tools", []):
        if t.get("available"):
            out[t["tool"]] = t.get("files", [])
    return out


def _clean_reason(error, limit=140):
    """Collapse a multi-line error into a single readable line."""
    flat = " ".join(error.split())
    return flat[:limit] + ("..." if len(flat) > limit else "")


def aggregate(files):
    ok = [f for f in files if f.get("ok")]
    skipped = [
        {"file": Path(f["path"]).name, "reason": _clean_reason(f.get("error", ""))}
        for f in files
        if not f.get("ok")
    ]
    # A directory measurement can succeed overall while individual files inside
    # it were processed but left unrewritten (the tool's own output failed its
    # parse check). That work happened before the check, so those bytes stay in
    # the throughput denominator -- but the file did not come out formatted, so
    # it does not count towards `files_ok` and is named in `skipped`.
    file_errors = [p for f in ok for p in f.get("file_errors", [])]
    skipped += [
        {
            "file": Path(p).name,
            "reason": "processed, but the tool's own output failed its parse "
            "check, so the file was left unchanged",
        }
        for p in file_errors
    ]
    total_bytes = sum(f["bytes"] for f in ok)
    median_total_ns = sum(f["median_ns"] for f in ok)
    min_total_ns = sum(f["min_ns"] for f in ok)
    mbps = (total_bytes / (median_total_ns * 1e-9) / 1e6) if median_total_ns else 0.0
    # A directory measurement is one record covering many files; it reports its
    # own count via `n_files`. Per-file records omit it and count as one.
    files_ok = sum(f.get("n_files", 1) for f in ok) - len(file_errors)
    return {
        "files_ok": files_ok,
        "total_bytes": total_bytes,
        "median_total_ns": median_total_ns,
        "min_total_ns": min_total_ns,
        "throughput_mbps": round(mbps, 3),
        "skipped": skipped,
    }


def scenario(label, target, fatou_report, julia_report):
    tools = {}
    tools.update(fatou_files(fatou_report))
    tools.update(julia_tools(julia_report))
    # Deterministic order: fatou first, then the Julia tools.
    order = ["fatou", "runic", "juliaformatter"]
    return {
        "label": label,
        "target": target,
        "tools": {t: aggregate(tools[t]) for t in order if t in tools},
    }


def cold_scenario(report):
    """Build the cold-start scenario from bench/cold_start.py's report, which
    carries every tool (Fatou included) in one `tools` list. `julia_tools` keys
    the available ones by name, so the same aggregation and ordering apply."""
    if report is None:
        return None
    tools = julia_tools(report)
    order = ["fatou", "runic", "juliaformatter"]
    return {
        "label": "Cold start",
        "target": report.get("target", ""),
        "tools": {t: aggregate(tools[t]) for t in order if t in tools},
    }


def version_of(fatou_report, julia_report):
    versions = {}
    if fatou_report:
        versions["fatou"] = fatou_report.get("version")
    if julia_report:
        versions["julia"] = julia_report.get("julia_version")
        for t in julia_report.get("tools", []):
            versions[t["tool"]] = t.get("version") if t.get("available") else None
    return versions


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--scenario",
        nargs=5,
        action="append",
        required=True,
        metavar=("KEY", "LABEL", "TARGET", "FATOU_JSON", "JULIA_JSON"),
        help="one measured scenario; repeat per scenario, in display order",
    )
    ap.add_argument("--cold", help="path to bench/cold_start.py's report (optional)")
    ap.add_argument("--meta", required=True, help="path to a JSON meta file")
    ap.add_argument("--out", required=True, help="results.json output path")
    args = ap.parse_args()

    meta = json.loads(Path(args.meta).read_text())

    scenarios = {}
    order = []
    # Versions come from whichever harness outputs are present; every scenario
    # runs the same binaries, so the first readable pair settles it.
    fatou_v, julia_v = None, None
    for key, label, target, fatou_json, julia_json in args.scenario:
        fr, jr = load(fatou_json), load(julia_json)
        fatou_v = fatou_v or fr
        julia_v = julia_v or jr
        scenarios[key] = scenario(label, target, fr, jr)
        order.append(key)

    meta["versions"] = version_of(fatou_v, julia_v)

    cold_sc = cold_scenario(load(args.cold) if args.cold else None)
    if cold_sc is not None:
        scenarios["cold_start"] = cold_sc

    results = {
        "schema_version": 2,
        "meta": meta,
        # Explicit render order; `cold_start` is deliberately absent, since the
        # docs render it under its own marker.
        "scenario_order": order,
        "scenarios": scenarios,
    }

    Path(args.out).write_text(json.dumps(results, indent=2) + "\n")
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()

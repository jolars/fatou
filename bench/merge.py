#!/usr/bin/env python3
"""Merge the Fatou and Julia warm-loop harness outputs into results.json, the
artifact the docs `doc-utils` mdBook preprocessor reads to render the benchmark
chart and fallback tables.

Throughput (MB/s) is computed per tool over the files that tool formatted
successfully, so a tool is never credited for files it could not parse, and the
skipped files are reported explicitly. MB/s normalizes for byte count, so the
numbers remain directly comparable even when tools cover different file sets.

The `project` scenario is a single whole-directory measurement per tool (one
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
    total_bytes = sum(f["bytes"] for f in ok)
    median_total_ns = sum(f["median_ns"] for f in ok)
    min_total_ns = sum(f["min_ns"] for f in ok)
    mbps = (total_bytes / (median_total_ns * 1e-9) / 1e6) if median_total_ns else 0.0
    # A directory measurement is one record covering many files; it reports its
    # own count via `n_files`. Per-file records omit it and count as one.
    files_ok = sum(f.get("n_files", 1) for f in ok)
    return {
        "files_ok": files_ok,
        "total_bytes": total_bytes,
        "median_total_ns": median_total_ns,
        "min_total_ns": min_total_ns,
        "throughput_mbps": round(mbps, 3),
        "skipped": skipped,
    }


def scenario(target, fatou_report, julia_report):
    tools = {}
    tools.update(fatou_files(fatou_report))
    tools.update(julia_tools(julia_report))
    # Deterministic order: fatou first, then the Julia tools.
    order = ["fatou", "runic", "juliaformatter"]
    return {
        "target": target,
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
    ap.add_argument("--fatou-single", required=True)
    ap.add_argument("--julia-single", required=True)
    ap.add_argument("--fatou-project", required=True)
    ap.add_argument("--julia-project", required=True)
    ap.add_argument("--meta", required=True, help="path to a JSON meta file")
    ap.add_argument("--out", required=True, help="results.json output path")
    args = ap.parse_args()

    fs, js = load(args.fatou_single), load(args.julia_single)
    fp, jp = load(args.fatou_project), load(args.julia_project)
    meta = json.loads(Path(args.meta).read_text())
    meta["versions"] = version_of(fs or fp, js or jp)

    results = {
        "schema_version": 1,
        "meta": meta,
        "scenarios": {
            "single_file": scenario(meta.get("single_target", ""), fs, js),
            "project": scenario(meta.get("project_target", ""), fp, jp),
        },
    }

    Path(args.out).write_text(json.dumps(results, indent=2) + "\n")
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()

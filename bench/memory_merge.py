#!/usr/bin/env python3
"""Merge the language-server and CLI outputs into memory.json, the artifact the
docs `doc-utils` mdBook preprocessor reads to render speed and memory results.

`lsp_memory.py` reports per-server readiness, request latency, returned work,
and memory milestones over a scripted editing session; `cli_memory.py` reports
peak RSS for one-shot runs. This flattens both into the shape the docs want,
attaches the run metadata, and computes each server's settled memory relative
to Fatou's -- the ratio is what survives a change of machine, even though for
memory the absolute megabytes are worth printing too.

Display labels are assigned here rather than in the preprocessor so the artifact
stays self-describing, the same way scenario labels work in results.json.
"""

import argparse
import json
from pathlib import Path

SCHEMA_VERSION = 2

# Server key -> display label and a one-line note on what that server is doing
# for its memory, which is the context the numbers are meaningless without.
SERVERS = {
    "fatou": ("Fatou", "static analysis, no Julia runtime"),
    "languageserver": (
        "LanguageServer.jl",
        "Julia runtime plus a SymbolServer pass over the environment",
    ),
    "jetls": ("JETLS", "Julia runtime plus type inference through JET"),
}


def load(path):
    return json.loads(Path(path).read_text())


def flatten_servers(lsp):
    """One flat record per server, ordered as the harness ran them."""
    records = []
    for entry in lsp.get("servers", []):
        key = entry["server"]
        label, doing = SERVERS.get(key, (key, ""))
        milestones = entry.get("milestones", {})
        baseline = milestones.get("baseline", {})
        settled = milestones.get("settled", {})
        peak = milestones.get("peak", {})
        records.append(
            {
                "key": key,
                "label": label,
                "doing": doing,
                "baseline_rss_mb": baseline.get("rss_mb"),
                "settled_rss_mb": settled.get("rss_mb"),
                "settled_pss_mb": settled.get("pss_mb"),
                "peak_rss_mb": peak.get("rss_mb"),
                "processes_at_settle": settled.get("processes"),
                "initialize_seconds": entry.get("initialize_seconds"),
                "workspace_ready_seconds": entry.get("workspace_ready_seconds"),
                "documents_ready_seconds": entry.get("documents_ready_seconds"),
                "request_latencies": entry.get("request_latencies", []),
                "settled_seconds": entry.get("settled_seconds"),
                "diagnostics_published": entry.get("diagnostics_published"),
                "diagnostic_requests": entry.get("diagnostic_requests"),
                "notes": entry.get("notes", []),
            }
        )
    return records


def add_ratios(records):
    """Settled memory relative to Fatou's, left absent when Fatou has no figure."""
    baseline = next(
        (
            r["settled_rss_mb"]
            for r in records
            if r["key"] == "fatou" and r["settled_rss_mb"]
        ),
        None,
    )
    for record in records:
        settled = record["settled_rss_mb"]
        record["relative_to_fatou"] = (
            round(settled / baseline, 1) if baseline and settled else None
        )
    return records


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--lsp", required=True)
    parser.add_argument("--cli", required=True)
    parser.add_argument("--meta", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    lsp = load(args.lsp)
    cli = load(args.cli)

    payload = {
        "schema_version": SCHEMA_VERSION,
        "meta": load(args.meta),
        "session": {
            "project": Path(lsp["project"]).name,
            "files": [Path(f).name for f in lsp["files"]],
            "file_count": len(lsp["files"]),
            "total_bytes": lsp["total_bytes"],
            "navigation_target": lsp.get("navigation_target"),
        },
        "servers": add_ratios(flatten_servers(lsp)),
        "cli": {
            "tree_files": cli["tree_files"],
            "tree_bytes": cli["tree_bytes"],
            "runs": cli["runs"],
            "cases": cli["cases"],
        },
    }

    Path(args.out).write_text(json.dumps(payload, indent=2) + "\n")
    print(f"==> wrote {args.out}")


if __name__ == "__main__":
    main()

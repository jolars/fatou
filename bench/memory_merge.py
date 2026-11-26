#!/usr/bin/env python3
"""Merge language-server measurements into the documentation's memory.json.

The artifact contains per-server readiness, request latency, returned work,
memory milestones, and settled memory relative to Fatou.

Display labels are assigned here rather than in the preprocessor so the artifact
stays self-describing, the same way scenario labels work in results.json.
"""

import argparse
import json
from pathlib import Path
from tempfile import NamedTemporaryFile

SCHEMA_VERSION = 3

# Server key -> display label and a one-line note on what that server is doing
# for its memory, which is the context the numbers are meaningless without.
SERVERS = {
    "fatou": ("Fatou", "static analysis, no Julia runtime"),
    "languageserver": (
        "LanguageServer.jl",
        "Julia runtime plus JuliaWorkspaces analysis",
    ),
    "jetls": ("JETLS", "Julia runtime plus type inference through JET"),
}


def load(path):
    return json.loads(Path(path).read_text())


def request_summaries(entry):
    return [
        {key: value for key, value in latency.items() if key != "raw"}
        for latency in entry.get("request_latencies", [])
    ]


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
                "command": entry["command"],
                "session_count": entry["session_count"],
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
                "request_latencies": request_summaries(entry),
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
    parser.add_argument("--meta", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    lsp = load(args.lsp)
    failed = {
        name: sum(session.get("initialize_status") != "ok" for session in sessions)
        for name, sessions in lsp["sessions"].items()
    }
    if any(failed.values()):
        parser.error(f"benchmark artifact has failed initialization sessions: {failed}")

    payload = {
        "schema_version": SCHEMA_VERSION,
        "meta": {
            **load(args.meta),
            "workspace_manifest_sha256": lsp["workspace_manifest_sha256"],
        },
        "session_order": lsp["session_order"],
        "sessions": {
            name: [
                {**session, "request_latencies": request_summaries(session)}
                for session in sessions
            ]
            for name, sessions in lsp["sessions"].items()
        },
        "session": {
            "project": Path(lsp["project"]).name,
            "files": [Path(f).name for f in lsp["files"]],
            "file_count": len(lsp["files"]),
            "total_bytes": lsp["total_bytes"],
            "navigation_target": lsp.get("navigation_target"),
        },
        "servers": add_ratios(flatten_servers(lsp)),
    }

    output = Path(args.out)
    with NamedTemporaryFile(mode="w", dir=output.parent, delete=False) as temporary:
        temporary_path = Path(temporary.name)
        try:
            temporary.write(json.dumps(payload, indent=2) + "\n")
            temporary.close()
            temporary_path.replace(output)
        finally:
            temporary_path.unlink(missing_ok=True)
    print(f"==> wrote {args.out}")


if __name__ == "__main__":
    main()

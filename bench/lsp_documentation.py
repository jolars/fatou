#!/usr/bin/env python3
"""Measure warm Markdown requests over LSP stdio, including dispatch overhead.

Run each binary on the same pinned CPU, then compare the JSON medians and minima:
  python3 bench/lsp_documentation.py --server 'taskset -c 2 /absolute/path/to/fatou lsp' \
      --out bench/documentation.tmp.json
Use --before to alternate before/after sessions and --source for a real Julia
file. Otherwise, generate docstring-heavy fixtures. A loose-file session avoids
machine-dependent package harvesting. Warmups exclude initial analysis and cache
construction; timings include transport, scheduling, and response serialization.
"""

import argparse
import hashlib
import json
import platform
import shlex
import statistics
import tempfile
import time
from pathlib import Path

from lsp_memory import Client


def fixture(count):
    return "".join(
        f'"""\n# Section {i}\n\n'
        + "A paragraph with **emphasis**, `code`, and [a link](https://example.org).\n"
        * 8
        + f'\n- First item\n- Second item\n\n"""\nf{i}(x) = x\n'
        for i in range(count)
    )


PROBES = '''
"""
See [target](@ref benchmark-target).
See [missing](#benchmark-missing).
See this.[^benchmarknote].
"""
benchmark_reference() = 1
"""
# [Benchmark target](@id benchmark-target)

[^benchmarknote]: Benchmark footnote.
"""
benchmark_target() = 2
'''


def position(text, marker):
    prefix = text[: text.index(marker) + len(marker)]
    return {
        "line": prefix.count("\n"),
        "character": len(prefix.rsplit("\n", 1)[-1].encode("utf-16-le")) // 2,
    }


def request(client, method, params):
    reply = client.request(method, params, timeout=60)
    if reply is None or "error" in reply:
        raise RuntimeError(f"{method}: {reply}")
    return reply["result"]


def measure(command, text, runs, warmups):
    with tempfile.TemporaryDirectory(prefix="fatou-doc-bench-") as directory:
        path = Path(directory) / "docs.jl"
        path.write_text(text)
        client = Client(shlex.split(command), directory)
        try:
            request(
                client,
                "initialize",
                {
                    "processId": None,
                    "rootUri": None,
                    "capabilities": {},
                },
            )
            client.notify("initialized", {})
            client.notify(
                "textDocument/didOpen",
                {
                    "textDocument": {
                        "uri": path.as_uri(),
                        "languageId": "julia",
                        "version": 1,
                        "text": text,
                    }
                },
            )
            # Diagnostics prove the analysis thread has consumed didOpen.
            with client.state:
                ready = client.state.wait_for(
                    lambda: any(
                        n.get("method") == "textDocument/publishDiagnostics"
                        and n["params"]["uri"] == path.as_uri()
                        for n in client.notifications
                    ),
                    timeout=60,
                )
            if not ready:
                raise RuntimeError("didOpen analysis never completed")
            results = {}
            for name, method, marker in [
                ("completion", "completion", "@ref benchmark-tar"),
                ("anchor_definition", "definition", "@ref benchmark-tar"),
                ("missing_definition", "definition", "#benchmark-miss"),
                ("footnote_definition", "definition", "[^benchmarkno"),
            ]:
                samples = []
                params = {
                    "textDocument": {"uri": path.as_uri()},
                    "position": position(text, marker),
                }
                expected = None
                for iteration in range(warmups + runs):
                    start = time.perf_counter_ns()
                    result = request(client, "textDocument/" + method, params)
                    elapsed = (time.perf_counter_ns() - start) / 1e6
                    if iteration == 0:
                        expected = result
                        if name == "completion":
                            items = (
                                result if isinstance(result, list) else result["items"]
                            )
                            assert any(i["label"] == "benchmark-target" for i in items)
                        else:
                            assert bool(result) == (name != "missing_definition"), (
                                name,
                                result,
                            )
                    assert result == expected, "warm requests changed their answer"
                    if iteration >= warmups:
                        samples.append(elapsed)
                results[name] = {
                    "median_ms": statistics.median(samples),
                    "min_ms": min(samples),
                    "samples_ms": samples,
                    "result_sha256": hashlib.sha256(
                        json.dumps(expected, sort_keys=True)
                        .replace(path.as_uri(), "file:///docs.jl")
                        .encode()
                    ).hexdigest(),
                }
            request(client, "shutdown", None)
            client.notify("exit", None)
            client.stdin.close()
            return results
        finally:
            client.kill()
            client.proc.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True)
    parser.add_argument("--before", help="Baseline server command to compare")
    parser.add_argument("--rounds", type=int, default=1)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--counts", type=int, nargs="+", default=[1, 100, 500])
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=3)
    args = parser.parse_args()
    if args.runs < 1 or args.warmups < 1 or args.rounds < 1:
        parser.error("runs, warmups, and rounds must be positive")
    sources = (
        [(str(args.source), args.source.read_text())]
        if args.source
        else [(f"synthetic-{n}-docstrings", fixture(n)) for n in args.counts]
    )
    commands = {"after": args.server}
    if args.before:
        commands = {"before": args.before, **commands}
    output = {
        "servers": commands,
        "platform": platform.platform(),
        "runs_per_round": args.runs,
        "rounds": args.rounds,
        "warmups_per_round": args.warmups,
        "cases": [],
    }
    for label, source in sources:
        text = source + PROBES
        result = {
            "label": label,
            "bytes": len(text.encode()),
            "sha256": hashlib.sha256(text.encode()).hexdigest(),
            "variants": {},
        }
        for round_number in range(args.rounds):
            order = list(commands.items())
            if round_number % 2:
                order.reverse()
            for variant, command in order:
                measured = measure(command, text, args.runs, args.warmups)
                requests = result["variants"].setdefault(variant, {})
                for name, values in measured.items():
                    if name not in requests:
                        requests[name] = values
                    else:
                        previous = requests[name]
                        assert previous["result_sha256"] == values["result_sha256"]
                        previous["samples_ms"].extend(values["samples_ms"])
                        previous["median_ms"] = statistics.median(
                            previous["samples_ms"]
                        )
                        previous["min_ms"] = min(previous["samples_ms"])
                print(
                    label,
                    round_number + 1,
                    variant,
                    {k: round(v["median_ms"], 3) for k, v in measured.items()},
                    flush=True,
                )
        if args.before:
            for name in result["variants"]["before"]:
                assert (
                    result["variants"]["before"][name]["result_sha256"]
                    == result["variants"]["after"][name]["result_sha256"]
                ), name
        output["cases"].append(result)
    args.out.write_text(json.dumps(output, indent=2) + "\n")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Speed and resident-memory harness for language servers, driven over stdio.

Each server is spawned, put through the same scripted editing session against
the same real Julia project, and sampled from `/proc` throughout. The session is
deliberately the boring one an editor produces on open:

    initialize -> initialized -> wait for the server to fall quiet
      -> didOpen N files -> diagnostics -> documentSymbol + hover on a few
      -> wait for it to fall quiet again -> definition + references + rename

Sampling covers the **whole process tree**, not just the server process, because
LanguageServer.jl fans its environment indexing out to a SymbolServer child and
would otherwise get that memory for free.

Two figures per sample, both summed over the tree:

    rss  VmRSS, the number `top` shows. Pages shared between parent and child
         are counted once per process.
    pss  Pss from smaps_rollup, which divides each shared page by the number of
         processes mapping it. This is the honest "how much of this machine's
         RAM is this session holding" figure.

Milestones recorded per server: `baseline` (handshake done, nothing opened),
`settled` (diagnostics in, tree quiet again), and `peak` (max over every sample,
which for a server with a short-lived indexing child is the only place that
child shows up at all). The harness also records initialization and readiness
times, then measures warm document-symbol, hover, definition, references, and
rename request latency.

Quiescence, not a fixed sleep, is what ends each phase: the tree is quiet once
its aggregate CPU stays under 5% of one core for `--quiet-seconds`, capped by
`--settle-timeout`. Servers here differ by two orders of magnitude in how long
they take to finish thinking, and any fixed wait would be unfair to one end.

Usage:
  lsp_memory.py --project <dir> --files <f.jl>... --out <out.json> \
      --server 'name=<command line>' [--server ...] \
      [--settle-timeout 300] [--quiet-seconds 5] [--stderr-dir <dir>]
      [--latency-runs 20] [--latency-warmups 2]
"""

import argparse
import json
import os
import re
import shlex
import signal
import statistics
import subprocess
import threading
import time
from pathlib import Path

CLK_TCK = os.sysconf("SC_CLK_TCK")
SCHEMA_VERSION = 2

# Fraction of one core the tree must stay under to count as quiet.
IDLE_CPU_FRACTION = 0.05
SYMBOL_DEFINITION = re.compile(
    r"^\s*(?:(?:mutable\s+)?struct|function|macro|const)\s+"
    r"([@A-Za-z_][A-Za-z0-9_!?]*(?:\.[@A-Za-z_][A-Za-z0-9_!?]*)*)"
)
NAVIGATION_FILE = "abstractdataframe/selection.jl"
NAVIGATION_LINE = "return c => identity => _names(idx)[c]"
NAVIGATION_SYMBOL = "_names"


# --- /proc sampling -----------------------------------------------------------


def process_tree(root_pid):
    """Every live pid in the tree rooted at `root_pid`, root included."""
    pids, frontier = {root_pid}, [root_pid]
    while frontier:
        pid = frontier.pop()
        try:
            tasks = list(Path(f"/proc/{pid}/task").iterdir())
        except OSError:
            continue  # exited between listing and reading; nothing to charge it
        for task in tasks:
            try:
                children = (task / "children").read_text().split()
            except OSError:
                continue
            for child in map(int, children):
                if child not in pids:
                    pids.add(child)
                    frontier.append(child)
    return pids


def read_process(pid):
    """(rss_kb, pss_kb, cpu_ticks) for one pid, or None if it is already gone."""
    try:
        status = Path(f"/proc/{pid}/status").read_text()
        stat = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None

    rss = 0
    for line in status.splitlines():
        if line.startswith("VmRSS:"):
            rss = int(line.split()[1])
            break

    try:
        pss = 0
        for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
            if line.startswith("Pss:"):
                pss = int(line.split()[1])
                break
    except OSError:
        pss = rss  # no rollup available: fall back to the pessimistic figure

    # utime and stime are fields 14 and 15, counted after the (comm) field --
    # which can itself contain spaces and parentheses, so split after the last
    # closing paren rather than on whitespace from the start.
    fields = stat[stat.rindex(")") + 2 :].split()
    return rss, pss, int(fields[11]) + int(fields[12])


def sample_tree(root_pid):
    """(rss_kb, pss_kb, cpu_ticks, process_count) summed over the live tree."""
    rss = pss = cpu = count = 0
    for pid in process_tree(root_pid):
        reading = read_process(pid)
        if reading is None:
            continue
        rss += reading[0]
        pss += reading[1]
        cpu += reading[2]
        count += 1
    return rss, pss, cpu, count


class Sampler(threading.Thread):
    """Poll the process tree in the background, keeping the running peak."""

    def __init__(self, pid, interval=0.15):
        super().__init__(daemon=True)
        self.pid = pid
        self.interval = interval
        self.stop_flag = threading.Event()
        self.samples = []  # (elapsed_s, rss_kb, pss_kb, cpu_ticks, process_count)
        self.peak_rss = 0
        self.peak_pss = 0
        self.started_at = time.monotonic()

    def run(self):
        while not self.stop_flag.is_set():
            rss, pss, cpu, count = sample_tree(self.pid)
            if count:
                elapsed = time.monotonic() - self.started_at
                self.samples.append((elapsed, rss, pss, cpu, count))
                self.peak_rss = max(self.peak_rss, rss)
                self.peak_pss = max(self.peak_pss, pss)
            self.stop_flag.wait(self.interval)

    def milestone(self):
        """Take a reading now, folding it into the peak the poll loop tracks."""
        rss, pss, _, count = sample_tree(self.pid)
        self.peak_rss = max(self.peak_rss, rss)
        self.peak_pss = max(self.peak_pss, pss)
        return {
            "rss_mb": round(rss / 1024, 1),
            "pss_mb": round(pss / 1024, 1),
            "processes": count,
        }

    def quiet_since(self, seconds, not_before=0.0):
        """Start of the current quiet window, or None when it is not quiet."""
        if not self.samples:
            return None
        cutoff = max(self.samples[-1][0] - seconds, not_before)
        window = [s for s in self.samples if s[0] >= cutoff]
        span = window[-1][0] - window[0][0] if len(window) >= 3 else 0.0
        if span < seconds * 0.8:
            return None  # not enough history yet to call it
        cpu_seconds = (window[-1][3] - window[0][3]) / CLK_TCK
        return window[0][0] if cpu_seconds / span < IDLE_CPU_FRACTION else None


# --- a minimal LSP client -----------------------------------------------------


class Client:
    """Just enough of the protocol to drive a server and keep it happy."""

    def __init__(self, cmd, cwd, stderr_path=None):
        # Servers report their own failures on stderr, and that is the only way
        # to tell "quiet because idle" from "quiet because dead".
        # Not a context manager: the handle has to outlive this call, for as
        # long as the server it is attached to. `kill` closes it.
        self.stderr_file = open(stderr_path, "wb") if stderr_path else None  # noqa: SIM115
        self.proc = subprocess.Popen(
            cmd,
            cwd=cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr_file or subprocess.DEVNULL,
            start_new_session=True,
        )
        assert self.proc.stdin is not None and self.proc.stdout is not None
        self.stdin = self.proc.stdin
        self.stdout = self.proc.stdout
        self.next_id = 1
        self.write_lock = threading.Lock()
        self.responses = {}
        self.notifications = []
        self.state = threading.Condition()
        self.alive = True
        threading.Thread(target=self._read_loop, daemon=True).start()

    def _read_loop(self):
        stream = self.stdout
        while True:
            length = None
            while True:
                line = stream.readline()
                if not line:
                    with self.state:
                        self.alive = False
                        self.state.notify_all()
                    return
                line = line.strip()
                if not line:
                    break  # end of headers
                if line.lower().startswith(b"content-length:"):
                    length = int(line.split(b":")[1])
            if length is None:
                continue
            try:
                message = json.loads(stream.read(length))
            except json.JSONDecodeError:
                continue
            with self.state:
                if "id" in message and ("result" in message or "error" in message):
                    self.responses[message["id"]] = message
                elif "method" in message:
                    self.notifications.append(message)
                    if "id" in message:
                        self._answer(message)
                self.state.notify_all()

    def _answer(self, request):
        """Reply to a server->client request, or the server blocks on us.

        The reply also has to typecheck on the server's side: LanguageServer.jl
        declares its `workspace/configuration` response as a Vector and throws
        on a bare null, which takes the whole server down mid-handshake.
        """
        if request["method"] == "workspace/configuration":
            items = (request.get("params") or {}).get("items") or []
            result = [None] * max(1, len(items))
        else:
            result = None
        self._send({"jsonrpc": "2.0", "id": request["id"], "result": result})

    def _send(self, message):
        payload = json.dumps(message).encode()
        with self.write_lock:
            try:
                self.stdin.write(b"Content-Length: %d\r\n\r\n" % len(payload))
                self.stdin.write(payload)
                self.stdin.flush()
            except (BrokenPipeError, ValueError, OSError):
                pass  # the server is gone; the caller finds out by timing out

    def notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params, timeout):
        """Send a request and wait for its response, or None on timeout/death."""
        with self.state:
            request_id = self.next_id
            self.next_id += 1
        self._send(
            {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        )
        deadline = time.monotonic() + timeout
        with self.state:
            while request_id not in self.responses:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not self.alive:
                    return None
                self.state.wait(min(1.0, remaining))
            return self.responses.pop(request_id)

    def count_published_diagnostics(self):
        with self.state:
            return sum(
                1
                for n in self.notifications
                if n.get("method") == "textDocument/publishDiagnostics"
            )

    def kill(self):
        try:
            os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError, OSError):
            pass
        if self.stderr_file is not None:
            self.stderr_file.close()


# Announce a broadly capable client, so no server withholds work on the grounds
# that we could not render it.
CAPABILITIES = {
    "workspace": {
        "workspaceFolders": True,
        "configuration": True,
        "didChangeConfiguration": {"dynamicRegistration": True},
        "symbol": {"dynamicRegistration": True},
        "workspaceEdit": {
            "documentChanges": True,
            "resourceOperations": ["create", "rename", "delete"],
        },
    },
    "textDocument": {
        "synchronization": {"dynamicRegistration": True, "didSave": True},
        "publishDiagnostics": {"relatedInformation": True, "versionSupport": True},
        "diagnostic": {"dynamicRegistration": True, "relatedDocumentSupport": True},
        "hover": {"contentFormat": ["markdown", "plaintext"]},
        "completion": {"completionItem": {"snippetSupport": True}},
        "definition": {"dynamicRegistration": True, "linkSupport": True},
        "references": {"dynamicRegistration": True},
        "rename": {"dynamicRegistration": True, "prepareSupport": True},
        "documentSymbol": {"hierarchicalDocumentSymbolSupport": True},
    },
}


def wait_until_quiet(sampler, quiet_seconds, timeout, not_before):
    """Return the quiet window's start, or None after `timeout` seconds."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        quiet_since = sampler.quiet_since(quiet_seconds, not_before)
        if quiet_since is not None:
            return quiet_since
        time.sleep(0.5)
    return None


def percentile(values, percentile_value):
    """Nearest-rank percentile for a non-empty latency sample."""
    ordered = sorted(values)
    rank = max(1, int(len(ordered) * percentile_value + 0.999999999))
    return ordered[min(rank - 1, len(ordered) - 1)]


def hover_position(text):
    """Choose the first source-defined identifier, in LSP line/column units."""
    for line_number, line in enumerate(text.splitlines()):
        match = SYMBOL_DEFINITION.match(line)
        if match is None:
            continue
        name = match.group(1)
        return {
            "line": line_number,
            "character": match.start(1) + name.rfind(".") + 1,
        }
    return {"line": 0, "character": 0}


def navigation_target(project, files):
    """Resolve the pinned cross-file symbol use in the benchmark corpus."""
    for file in map(Path, files):
        if not file.as_posix().endswith(NAVIGATION_FILE):
            continue
        for line_number, line in enumerate(
            file.read_text(errors="replace").splitlines()
        ):
            line_start = line.find(NAVIGATION_LINE)
            if line_start < 0:
                continue
            character = line.find(NAVIGATION_SYMBOL, line_start)
            if character < 0:
                break
            return {
                "uri": file.as_uri(),
                "file": str(file.relative_to(project)),
                "symbol": NAVIGATION_SYMBOL,
                "position": {"line": line_number, "character": character},
            }
        break
    raise RuntimeError(
        f"navigation target {NAVIGATION_SYMBOL} in {NAVIGATION_FILE} is missing"
    )


def location_summary(result):
    """Number of definition/reference locations and distinct target files."""
    locations = result if isinstance(result, list) else [result]
    uris = {
        location.get("uri") or location.get("targetUri")
        for location in locations
        if isinstance(location, dict)
    }
    uris.discard(None)
    return len(locations), len(uris)


def document_symbol_summary(result):
    """Number of symbols, including nested DocumentSymbol children."""

    def count(symbol):
        return 1 + sum(count(child) for child in symbol.get("children") or [])

    symbols = result if isinstance(result, list) else []
    return sum(count(symbol) for symbol in symbols), None


def singleton_summary(result):
    """A successful, nonempty singleton response such as hover."""
    return 1, None


def workspace_edit_summary(result):
    """Number of text edits and distinct files in a WorkspaceEdit."""
    edits = 0
    uris = set()
    for uri, file_edits in (result.get("changes") or {}).items():
        uris.add(uri)
        edits += len(file_edits or [])
    for change in result.get("documentChanges") or []:
        document = change.get("textDocument") if isinstance(change, dict) else None
        if document is None:
            continue
        uri = document.get("uri")
        if uri is not None:
            uris.add(uri)
        edits += len(change.get("edits") or [])
    return edits, len(uris)


def add_distribution(record, prefix, values):
    """Attach min/median/max fields when at least one response supplied them."""
    if not values:
        return
    record[f"{prefix}_min"] = min(values)
    record[f"{prefix}_median"] = statistics.median(values)
    record[f"{prefix}_max"] = max(values)


def benchmark_requests(
    client,
    key,
    label,
    method,
    params,
    runs,
    warmups,
    timeout,
    result_unit,
    summarize,
):
    """Measure serial stdio round trips for one warm LSP request kind."""
    for _ in range(warmups):
        for request_params in params:
            client.request(method, request_params, timeout=timeout)

    latencies = []
    failures = 0
    empty_results = 0
    result_counts = []
    result_files = []
    payload_bytes = []
    for _ in range(runs):
        for request_params in params:
            started = time.perf_counter_ns()
            response = client.request(method, request_params, timeout=timeout)
            elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
            if response is None or "error" in response:
                failures += 1
                if not client.alive:
                    break
                continue
            result = response.get("result")
            if result in (None, [], {}):
                empty_results += 1
            else:
                count, files = summarize(result)
                result_counts.append(count)
                if files is not None:
                    result_files.append(files)
                payload_bytes.append(
                    len(
                        json.dumps(
                            result, separators=(",", ":"), ensure_ascii=False
                        ).encode()
                    )
                )
            latencies.append(elapsed_ms)
        if not client.alive:
            break

    record = {
        "key": key,
        "label": label,
        "median_ms": round(statistics.median(latencies), 3) if latencies else None,
        "p95_ms": round(percentile(latencies, 0.95), 3) if latencies else None,
        "samples": len(latencies),
        "failures": failures,
        "empty_results": empty_results,
        "targets": len(params),
        "result_unit": result_unit,
    }
    add_distribution(record, "result_count", result_counts)
    add_distribution(record, "result_files", result_files)
    if payload_bytes:
        record["payload_bytes_median"] = round(statistics.median(payload_bytes))
    return record


def run_session(
    name,
    cmd,
    project,
    files,
    settle_timeout,
    quiet_seconds,
    latency_runs,
    latency_warmups,
    navigation,
    stderr_dir,
):
    print(f"==> language server: {name}", flush=True)
    stderr_path = str(Path(stderr_dir) / f"{name}.stderr.log") if stderr_dir else None
    client = Client(cmd, cwd=str(project), stderr_path=stderr_path)
    sampler = Sampler(client.proc.pid)
    sampler.start()

    result = {"server": name, "command": cmd, "milestones": {}, "notes": []}
    start = time.monotonic()

    initialized = client.request(
        "initialize",
        {
            "processId": os.getpid(),
            "clientInfo": {"name": "fatou-lsp-bench", "version": "2"},
            "rootUri": project.as_uri(),
            "rootPath": str(project),
            "capabilities": CAPABILITIES,
            "workspaceFolders": [{"uri": project.as_uri(), "name": project.name}],
            "initializationOptions": {},
        },
        timeout=settle_timeout,
    )
    if initialized is None:
        result["notes"].append("initialize never answered; server timed out or died")
        sampler.stop_flag.set()
        client.kill()
        return result

    result["initialize_seconds"] = round(time.monotonic() - start, 3)
    capabilities = (initialized.get("result") or {}).get("capabilities", {})
    result["pull_diagnostics"] = bool(capabilities.get("diagnosticProvider"))
    client.notify("initialized", {})
    baseline_phase = time.monotonic() - sampler.started_at

    # `initialized` is where servers kick off background work -- LanguageServer.jl
    # starts SymbolServer here -- so the baseline is only meaningful once that
    # has run its course.
    baseline_ready = wait_until_quiet(
        sampler, quiet_seconds, settle_timeout, not_before=baseline_phase
    )
    if baseline_ready is None:
        result["notes"].append("still busy at the settle timeout before opening files")
    if client.proc.poll() is not None:
        result["notes"].append(
            f"server exited before baseline (rc={client.proc.returncode})"
        )
    result["milestones"]["baseline"] = sampler.milestone()
    result["baseline_seconds"] = round(time.monotonic() - start, 2)
    result["workspace_ready_seconds"] = (
        round(baseline_ready + sampler.started_at - start, 3)
        if baseline_ready is not None
        else None
    )
    print(
        f"    baseline {result['milestones']['baseline']['rss_mb']} MB RSS", flush=True
    )

    uris = []
    hover_positions = {}
    documents_started = time.monotonic()
    documents_phase = documents_started - sampler.started_at
    for path in files:
        uri = Path(path).as_uri()
        text = Path(path).read_text(errors="replace")
        uris.append(uri)
        hover_positions[uri] = hover_position(text)
        client.notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": uri,
                    "languageId": "julia",
                    "version": 1,
                    "text": text,
                }
            },
        )

    if result["pull_diagnostics"]:
        for uri in uris:
            client.request(
                "textDocument/diagnostic",
                {"textDocument": {"uri": uri}},
                timeout=settle_timeout,
            )
        result["diagnostic_requests"] = len(uris)
    else:
        # Push-model servers publish on their own schedule; the quiescence wait
        # below is what actually bounds them.
        result["diagnostic_requests"] = 0

    # Prime the same request paths the timed phase exercises. This also ensures
    # that the settled memory milestone describes a server that has answered
    # editor queries, rather than one that merely swallowed some text.
    for uri in uris[:3]:
        client.request(
            "textDocument/documentSymbol", {"textDocument": {"uri": uri}}, timeout=60
        )
        client.request(
            "textDocument/hover",
            {"textDocument": {"uri": uri}, "position": hover_positions[uri]},
            timeout=60,
        )

    documents_ready = wait_until_quiet(
        sampler, quiet_seconds, settle_timeout, not_before=documents_phase
    )
    if documents_ready is None:
        result["notes"].append("still busy at the settle timeout after opening files")
    if client.proc.poll() is not None:
        result["notes"].append(
            f"server exited before settling (rc={client.proc.returncode})"
        )
    result["milestones"]["settled"] = sampler.milestone()
    result["settled_seconds"] = round(time.monotonic() - start, 2)
    result["documents_ready_seconds"] = (
        round(documents_ready + sampler.started_at - documents_started, 3)
        if documents_ready is not None
        else None
    )
    result["diagnostics_published"] = client.count_published_diagnostics()

    latency_targets = uris[:3]
    navigation_params = {
        "textDocument": {"uri": navigation["uri"]},
        "position": navigation["position"],
    }
    result["request_latencies"] = [
        benchmark_requests(
            client,
            "document_symbol",
            "Document symbols",
            "textDocument/documentSymbol",
            [{"textDocument": {"uri": uri}} for uri in latency_targets],
            latency_runs,
            latency_warmups,
            timeout=60,
            result_unit="symbol",
            summarize=document_symbol_summary,
        ),
        benchmark_requests(
            client,
            "hover",
            "Hover",
            "textDocument/hover",
            [
                {"textDocument": {"uri": uri}, "position": hover_positions[uri]}
                for uri in latency_targets
            ],
            latency_runs,
            latency_warmups,
            timeout=60,
            result_unit="result",
            summarize=singleton_summary,
        ),
        benchmark_requests(
            client,
            "definition",
            "Go to definition",
            "textDocument/definition",
            [navigation_params],
            latency_runs,
            latency_warmups,
            timeout=60,
            result_unit="location",
            summarize=location_summary,
        ),
        benchmark_requests(
            client,
            "references",
            "Find references",
            "textDocument/references",
            [{**navigation_params, "context": {"includeDeclaration": True}}],
            latency_runs,
            latency_warmups,
            timeout=60,
            result_unit="location",
            summarize=location_summary,
        ),
        benchmark_requests(
            client,
            "rename",
            "Rename",
            "textDocument/rename",
            [{**navigation_params, "newName": "fatou_benchmark_names"}],
            latency_runs,
            latency_warmups,
            timeout=60,
            result_unit="edit",
            summarize=workspace_edit_summary,
        ),
    ]
    for latency in result["request_latencies"]:
        median = latency["median_ms"]
        if median is not None:
            print(
                f"    {latency['label'].lower():<17} {median:.3f} ms median"
                f" ({latency['p95_ms']:.3f} ms p95)",
                flush=True,
            )

    sampler.stop_flag.set()
    sampler.join(timeout=2)
    result["milestones"]["peak"] = {
        "rss_mb": round(sampler.peak_rss / 1024, 1),
        "pss_mb": round(sampler.peak_pss / 1024, 1),
    }
    result["samples"] = len(sampler.samples)
    print(
        f"    settled  {result['milestones']['settled']['rss_mb']} MB RSS"
        f"  (peak {result['milestones']['peak']['rss_mb']} MB,"
        f" {result['settled_seconds']}s)",
        flush=True,
    )

    client.request("shutdown", None, timeout=15)
    client.notify("exit", None)
    time.sleep(0.5)
    client.kill()
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--project", required=True, help="workspace root opened by every server"
    )
    parser.add_argument(
        "--files", nargs="+", required=True, help="files to open, in order"
    )
    parser.add_argument("--out", required=True)
    parser.add_argument(
        "--server", action="append", default=[], metavar="NAME=COMMAND", required=True
    )
    parser.add_argument("--settle-timeout", type=float, default=300)
    parser.add_argument("--quiet-seconds", type=float, default=5)
    parser.add_argument("--latency-runs", type=int, default=20)
    parser.add_argument("--latency-warmups", type=int, default=2)
    parser.add_argument("--stderr-dir", default=None)
    args = parser.parse_args()

    project = Path(args.project).absolute()
    files = [str(Path(f).absolute()) for f in args.files]
    navigation = navigation_target(project, files)

    results = []
    for spec in args.server:
        name, _, command = spec.partition("=")
        results.append(
            run_session(
                name,
                shlex.split(command),
                project,
                files,
                args.settle_timeout,
                args.quiet_seconds,
                args.latency_runs,
                args.latency_warmups,
                navigation,
                args.stderr_dir,
            )
        )
        time.sleep(2)  # let the previous server's pages actually go back

    Path(args.out).write_text(
        json.dumps(
            {
                "schema_version": SCHEMA_VERSION,
                "project": str(project),
                "files": files,
                "total_bytes": sum(Path(f).stat().st_size for f in files),
                "quiet_seconds": args.quiet_seconds,
                "settle_timeout": args.settle_timeout,
                "latency_runs": args.latency_runs,
                "latency_warmups": args.latency_warmups,
                "latency_files": min(3, len(files)),
                "navigation_target": {
                    key: value for key, value in navigation.items() if key != "uri"
                },
                "servers": results,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Resident-memory harness for language servers, driven over stdio.

Each server is spawned, put through the same scripted editing session against
the same real Julia project, and sampled from `/proc` throughout. The session is
deliberately the boring one an editor produces on open:

    initialize -> initialized -> wait for the server to fall quiet
      -> didOpen N files -> diagnostics -> documentSymbol + hover on a few
      -> wait for it to fall quiet again

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
child shows up at all).

Quiescence, not a fixed sleep, is what ends each phase: the tree is quiet once
its aggregate CPU stays under 5% of one core for `--quiet-seconds`, capped by
`--settle-timeout`. Servers here differ by two orders of magnitude in how long
they take to finish thinking, and any fixed wait would be unfair to one end.

Usage:
  lsp_memory.py --project <dir> --files <f.jl>... --out <out.json> \
      --server 'name=<command line>' [--server ...] \
      [--settle-timeout 300] [--quiet-seconds 5] [--stderr-dir <dir>]
"""

import argparse
import json
import os
import shlex
import signal
import subprocess
import threading
import time
from pathlib import Path

CLK_TCK = os.sysconf("SC_CLK_TCK")
SCHEMA_VERSION = 1

# Fraction of one core the tree must stay under to count as quiet.
IDLE_CPU_FRACTION = 0.05


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

    def is_quiet(self, seconds):
        """Has aggregate tree CPU stayed under the idle threshold that long?"""
        if not self.samples:
            return False
        cutoff = self.samples[-1][0] - seconds
        window = [s for s in self.samples if s[0] >= cutoff]
        span = window[-1][0] - window[0][0] if len(window) >= 3 else 0.0
        if span < seconds * 0.8:
            return False  # not enough history yet to call it
        cpu_seconds = (window[-1][3] - window[0][3]) / CLK_TCK
        return cpu_seconds / span < IDLE_CPU_FRACTION


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
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
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
    },
    "textDocument": {
        "synchronization": {"dynamicRegistration": True, "didSave": True},
        "publishDiagnostics": {"relatedInformation": True, "versionSupport": True},
        "diagnostic": {"dynamicRegistration": True, "relatedDocumentSupport": True},
        "hover": {"contentFormat": ["markdown", "plaintext"]},
        "completion": {"completionItem": {"snippetSupport": True}},
        "definition": {"dynamicRegistration": True},
        "documentSymbol": {"hierarchicalDocumentSymbolSupport": True},
    },
}


def wait_until_quiet(sampler, quiet_seconds, timeout):
    """Block until the tree stops working, or `timeout` seconds elapse."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if sampler.is_quiet(quiet_seconds):
            return True
        time.sleep(0.5)
    return False


def run_session(name, cmd, project, files, settle_timeout, quiet_seconds, stderr_dir):
    print(f"==> memory: {name}", flush=True)
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
            "clientInfo": {"name": "fatou-memory-bench", "version": "1"},
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

    result["init_seconds"] = round(time.monotonic() - start, 2)
    capabilities = (initialized.get("result") or {}).get("capabilities", {})
    result["pull_diagnostics"] = bool(capabilities.get("diagnosticProvider"))
    client.notify("initialized", {})

    # `initialized` is where servers kick off background work -- LanguageServer.jl
    # starts SymbolServer here -- so the baseline is only meaningful once that
    # has run its course.
    if not wait_until_quiet(sampler, quiet_seconds, settle_timeout):
        result["notes"].append("still busy at the settle timeout before opening files")
    if client.proc.poll() is not None:
        result["notes"].append(f"server exited before baseline (rc={client.proc.returncode})")
    result["milestones"]["baseline"] = sampler.milestone()
    result["baseline_seconds"] = round(time.monotonic() - start, 2)
    print(f"    baseline {result['milestones']['baseline']['rss_mb']} MB RSS", flush=True)

    uris = []
    for path in files:
        uri = Path(path).as_uri()
        uris.append(uri)
        client.notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": uri,
                    "languageId": "julia",
                    "version": 1,
                    "text": Path(path).read_text(errors="replace"),
                }
            },
        )
        time.sleep(0.2)

    if result["pull_diagnostics"]:
        for uri in uris:
            client.request(
                "textDocument/diagnostic", {"textDocument": {"uri": uri}}, timeout=settle_timeout
            )
        result["diagnostic_requests"] = len(uris)
    else:
        # Push-model servers publish on their own schedule; the quiescence wait
        # below is what actually bounds them.
        result["diagnostic_requests"] = 0

    # A few real queries, so this measures a server that has answered questions
    # rather than one that merely swallowed some text.
    for uri in uris[:3]:
        client.request("textDocument/documentSymbol", {"textDocument": {"uri": uri}}, timeout=60)
        client.request(
            "textDocument/hover",
            {"textDocument": {"uri": uri}, "position": {"line": 20, "character": 5}},
            timeout=60,
        )

    if not wait_until_quiet(sampler, quiet_seconds, settle_timeout):
        result["notes"].append("still busy at the settle timeout after opening files")
    if client.proc.poll() is not None:
        result["notes"].append(f"server exited before settling (rc={client.proc.returncode})")
    result["milestones"]["settled"] = sampler.milestone()
    result["settled_seconds"] = round(time.monotonic() - start, 2)
    result["diagnostics_published"] = client.count_published_diagnostics()

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
    parser.add_argument("--project", required=True, help="workspace root opened by every server")
    parser.add_argument("--files", nargs="+", required=True, help="files to open, in order")
    parser.add_argument("--out", required=True)
    parser.add_argument(
        "--server", action="append", default=[], metavar="NAME=COMMAND", required=True
    )
    parser.add_argument("--settle-timeout", type=float, default=300)
    parser.add_argument("--quiet-seconds", type=float, default=5)
    parser.add_argument("--stderr-dir", default=None)
    args = parser.parse_args()

    project = Path(args.project).absolute()
    files = [str(Path(f).absolute()) for f in args.files]

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
                "servers": results,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()

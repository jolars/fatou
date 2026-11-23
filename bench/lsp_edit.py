"""Versioned editing workload shared by the LSP benchmark runs."""

import time


def change_params(uri, version, text, start, end, replacement, sync):
    changed = text[:start] + replacement + text[end:]
    content = {"text": changed}
    if sync == 2:
        content = {
            "range": {"start": position(text, start), "end": position(text, end)},
            "text": replacement,
        }
    return {
        "textDocument": {"uri": uri, "version": version},
        "contentChanges": [content],
    }


def position(text, offset):
    prefix = text[:offset]
    return {
        "line": prefix.count("\n"),
        "character": len(prefix.rsplit("\n", 1)[-1].encode("utf-16-le")) // 2,
    }


def matching_diagnostics(message, uri, version):
    params = message.get("params")
    if not isinstance(params, dict):
        return None
    if (
        message.get("method") == "textDocument/publishDiagnostics"
        and params.get("uri") == uri
        and params.get("version") in (None, version)
        and isinstance(params.get("diagnostics"), list)
    ):
        return params["diagnostics"]
    return None


def completion_count(reply, expected):
    if isinstance(reply, dict):
        reply = reply.get("items")
    if isinstance(reply, list) and any(item.get("label") == expected for item in reply):
        return len(reply)
    return 0


def edit_workload(client, capabilities, navigation, text, runs, warmups, timeout):
    uri = navigation["uri"]
    document = {"uri": uri}
    sync = capabilities.get("textDocumentSync", 0)
    if isinstance(sync, dict):
        sync = sync.get("change", 0)
    body = "c = idx[sel]\n    return c => identity => _names(idx)[c]"
    start = text.index(body) + len("c = ")
    end = start + len("idx[sel]")
    suffix = "\n)\n"
    error_line = text.count("\n") + 1
    version = 1
    samples = {
        key: [] for key in ("edit_completion", "edit_diagnostic", "repair_diagnostic")
    }

    def change(original, begin, finish, replacement):
        nonlocal version
        version += 1
        client.notify(
            "textDocument/didChange",
            change_params(uri, version, original, begin, finish, replacement, sync),
        )

    def diagnostics(cursor, present):
        def matches(items):
            return (
                any(item["range"]["start"]["line"] == error_line for item in items)
                == present
            )

        if capabilities.get("diagnosticProvider"):
            reply = client.request(
                "textDocument/diagnostic", {"textDocument": document}, timeout
            )
            if reply is None:
                return "timeout" if client.alive else "exit", None
            if "error" in reply:
                return "error", None
            items = (reply.get("result") or {}).get("items")
            return (
                ("ok", len(items))
                if isinstance(items, list) and matches(items)
                else ("invalid", None)
            )
        with client.state:

            def received():
                for message in client.notifications[cursor:]:
                    items = matching_diagnostics(message, uri, version)
                    if items is not None and matches(items):
                        return items
                return None

            client.state.wait_for(
                lambda: received() is not None or not client.alive, timeout
            )
            items = received()
        if items is not None:
            return "ok", len(items)
        return "timeout" if client.alive else "exit", None

    def record(key, started, status, count):
        if iteration >= warmups:
            samples[key].append(
                {
                    "status": status,
                    "ms": (time.perf_counter_ns() - started) / 1e6
                    if status == "ok"
                    else None,
                    "count": count,
                }
            )

    if sync not in (1, 2):
        return {
            key: [{"status": "unsupported", "ms": None, "count": None}]
            for key in samples
        }

    for iteration in range(warmups + runs):
        if not client.alive:
            break
        prefix, expected = (
            ("renamecol", "renamecols") if iteration % 2 == 0 else ("se", "sel")
        )
        changed = text[:start] + prefix + text[end:]
        started = time.perf_counter_ns()
        if capabilities.get("completionProvider") is None:
            record("edit_completion", started, "unsupported", None)
        else:
            change(text, start, end, prefix)
            reply = client.request(
                "textDocument/completion",
                {
                    "textDocument": document,
                    "position": position(changed, start + len(prefix)),
                    "context": {"triggerKind": 1},
                },
                timeout,
            )
            if reply is None:
                status, count = ("timeout" if client.alive else "exit"), None
            elif "error" in reply:
                status, count = "error", None
            else:
                count = completion_count(reply.get("result"), expected)
                status = "ok" if count else "invalid"
            record("edit_completion", started, status, count)
            change(changed, start, start + len(prefix), text[start:end])

        with client.state:
            cursor = len(client.notifications)
        started = time.perf_counter_ns()
        change(text, len(text), len(text), suffix)
        status, count = diagnostics(cursor, True)
        record("edit_diagnostic", started, status, count)

        with client.state:
            cursor = len(client.notifications)
        started = time.perf_counter_ns()
        change(text + suffix, len(text), len(text) + len(suffix), "")
        if status == "ok":
            repair_status, count = diagnostics(cursor, False)
        else:
            repair_status, count = "invalid", None
        record("repair_diagnostic", started, repair_status, count)
        if status != "ok" or repair_status != "ok":
            break
    return samples

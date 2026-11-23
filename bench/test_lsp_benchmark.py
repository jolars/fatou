import sys
import tempfile
import threading
import unittest

from lsp_edit import (
    change_params,
    completion_count,
    edit_workload,
    matching_diagnostics,
)
from lsp_memory import Client, aggregate_sessions


class BenchmarkTests(unittest.TestCase):
    def test_full_and_incremental_changes(self):
        text = "😀old"
        full = change_params("file:///test.jl", 2, text, 1, 4, "new", 1)
        step = change_params("file:///test.jl", 2, text, 1, 4, "new", 2)
        self.assertEqual(full["contentChanges"], [{"text": "😀new"}])
        self.assertEqual(
            step["contentChanges"][0]["range"],
            {
                "start": {"line": 0, "character": 2},
                "end": {"line": 0, "character": 5},
            },
        )
        self.assertEqual(step["textDocument"]["version"], 2)

    def test_diagnostics_reject_stale_versions_and_other_files(self):
        message = {
            "method": "textDocument/publishDiagnostics",
            "params": {"uri": "file:///test.jl", "version": 2, "diagnostics": []},
        }
        self.assertIsNone(matching_diagnostics(message, "file:///test.jl", 3))
        self.assertIsNone(matching_diagnostics(message, "file:///other.jl", 2))
        self.assertEqual(matching_diagnostics(message, "file:///test.jl", 2), [])

    def test_completion_requires_expected_binding(self):
        self.assertEqual(
            completion_count({"items": [{"label": "other"}]}, "expected"), 0
        )
        self.assertEqual(completion_count([{"label": "expected"}], "expected"), 1)

    def test_edit_workload_ignores_stale_push_and_rejects_wrong_completion(self):
        uri = "file:///test.jl"
        text = "c = idx[sel]\n    return c => identity => _names(idx)[c]"

        class Server:
            alive = True
            state = threading.Condition()

            def __init__(self):
                self.notifications = []

            def notify(self, method, params):
                self.assert_change(method, params)
                content = params["contentChanges"][0]["text"]
                version = params["textDocument"]["version"]
                diagnostic = {"range": {"start": {"line": 2}}}
                for diagnostics, revision in (
                    ([], version - 1),
                    ([diagnostic] if content.endswith("\n)\n") else [], version),
                ):
                    self.notifications.append(
                        {
                            "method": "textDocument/publishDiagnostics",
                            "params": {
                                "uri": uri,
                                "version": revision,
                                "diagnostics": diagnostics,
                            },
                        }
                    )

            def request(self, method, params, timeout):
                if method == "textDocument/completion":
                    return {"result": [{"label": "unrelated"}]}
                raise AssertionError(method)

            def assert_change(self, method, params):
                if (
                    method != "textDocument/didChange"
                    or "range" in params["contentChanges"][0]
                ):
                    raise AssertionError((method, params))

        result = edit_workload(
            Server(),
            {"textDocumentSync": 1, "completionProvider": {}},
            {"uri": uri},
            text,
            runs=1,
            warmups=0,
            timeout=0.05,
        )
        self.assertEqual(result["edit_completion"][0]["status"], "invalid")
        self.assertEqual(result["edit_diagnostic"][0]["status"], "ok")
        self.assertEqual(result["edit_diagnostic"][0]["count"], 1)
        self.assertEqual(result["repair_diagnostic"][0]["status"], "ok")
        self.assertEqual(result["repair_diagnostic"][0]["count"], 0)

    def test_invalid_samples_do_not_enter_median(self):
        latency = {
            "key": "completion",
            "raw": [{"status": "invalid", "ms": None}, {"status": "ok", "ms": 40}],
        }
        result = aggregate_sessions([{"notes": [], "request_latencies": [latency]}])
        self.assertEqual(result["request_latencies"][0]["median_ms"], 40)
        self.assertEqual(result["request_latencies"][0]["samples"], 1)

    def test_silent_server_times_out_and_is_reaped(self):
        with tempfile.TemporaryDirectory() as directory:
            client = Client(
                [sys.executable, "-c", "import time; time.sleep(60)"], directory
            )
            try:
                self.assertIsNone(client.request("initialize", {}, timeout=0.05))
            finally:
                client.kill()
            self.assertIsNotNone(client.proc.poll())


if __name__ == "__main__":
    unittest.main()

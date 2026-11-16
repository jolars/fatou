import hashlib
import importlib.util
import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "julia_oracle", Path(__file__).resolve().parents[1] / "julia-oracle.py"
)
oracle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(oracle)


class JuliaOracleTests(unittest.TestCase):
    def test_checked_in_lock_is_valid(self):
        oracle.validate_lock(json.loads(oracle.LOCK_PATH.read_text()))

    def test_rejects_moving_download_urls(self):
        lock = self.lock()
        lock["url"] = oracle.LATEST_URL
        with self.assertRaisesRegex(ValueError, "commit"):
            oracle.validate_lock(lock)

    def test_rejects_changed_archive_before_extracting_or_executing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "corrupt.tar.gz"
            archive.write_bytes(b"changed upstream archive")
            with (
                patch.object(oracle, "download", return_value=archive),
                patch.object(oracle, "extract_archive") as extract,
                patch.object(oracle, "runtime_identity") as identity,
            ):
                with self.assertRaisesRegex(ValueError, "SHA-256"):
                    oracle.install(self.lock(), root / "cache")
                extract.assert_not_called()
                identity.assert_not_called()

    def test_rejects_wrong_runtime_commit(self):
        with (
            patch.object(
                oracle, "runtime_identity", return_value=("1.15.0-DEV.1", "b" * 40)
            ),
            self.assertRaisesRegex(ValueError, "commit"),
        ):
            oracle.verify_runtime(Path("julia"), self.lock())

    def test_rejects_wrong_runtime_version(self):
        with (
            patch.object(oracle, "runtime_identity", return_value=("1.14.0", "a" * 40)),
            self.assertRaisesRegex(ValueError, "version"),
        ):
            oracle.verify_runtime(Path("julia"), self.lock())

    def test_rechecks_cached_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            (cache / ("0" * 64 + ".tar.gz")).write_bytes(b"corrupt cache")
            with patch.object(oracle, "extract_archive") as extract:
                with self.assertRaisesRegex(ValueError, "SHA-256"):
                    oracle.install(self.lock(), cache)
                extract.assert_not_called()

    def test_rejects_archive_path_traversal(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "bad.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                member = tarfile.TarInfo("../escape")
                member.size = 1
                stream.addfile(member, io.BytesIO(b"x"))
            with self.assertRaises((tarfile.FilterError, ValueError)):
                oracle.extract_archive(archive, root / "install")
            self.assertFalse((root / "escape").exists())

    def test_failed_discovery_preserves_existing_lock(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            lock_path = root / "lock.json"
            original = json.dumps(self.lock())
            lock_path.write_text(original)
            with (
                patch.object(oracle, "discover", side_effect=ValueError("bad runtime")),
                self.assertRaisesRegex(ValueError, "bad runtime"),
            ):
                oracle.update_lock(lock_path, root / "cache")
            self.assertEqual(lock_path.read_text(), original)

    def test_installs_verified_archive_without_network(self):
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            archive = cache / "binary.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                member = tarfile.TarInfo("julia-test/bin/julia")
                member.mode = 0o755
                content = b"#!/bin/sh\nexit 0\n"
                member.size = len(content)
                stream.addfile(member, io.BytesIO(content))
            lock = self.lock()
            lock["sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()
            archive.rename(cache / (lock["sha256"] + ".tar.gz"))
            with (
                patch.object(oracle, "download") as download,
                patch.object(
                    oracle,
                    "runtime_identity",
                    return_value=(lock["version"], lock["commit"]),
                ),
            ):
                bindir = oracle.install(lock, cache)
                self.assertTrue((bindir / "julia").is_file())
                download.assert_not_called()

    @staticmethod
    def lock():
        return {
            "version": "1.15.0-DEV.1",
            "commit": "a" * 40,
            "platform": "x86_64-linux-gnu",
            "url": oracle.ARCHIVE_BASE + "/1.15/julia-aaaaaaaaaa-linux-x86_64.tar.gz",
            "sha256": "0" * 64,
        }


if __name__ == "__main__":
    unittest.main()

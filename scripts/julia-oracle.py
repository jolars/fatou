#!/usr/bin/env python3
"""Resolve and install the oracle's pinned Linux x86-64 Julia build."""

import argparse
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
LOCK_PATH = REPO_ROOT / "scripts/julia-oracle.lock.json"
ARCHIVE_BASE = "https://julialangnightlies-s3.julialang.org/bin/linux/x86_64"
LATEST_URL = ARCHIVE_BASE + "/julia-latest-linux-x86_64.tar.gz"
SOURCE_URL = "https://raw.githubusercontent.com/JuliaLang/julia"


def validate_lock(lock):
    if lock["platform"] != "x86_64-linux-gnu":
        raise ValueError("The oracle binary must target x86_64-linux-gnu")
    if not re.fullmatch(r"[0-9a-f]{40}", lock["commit"]):
        raise ValueError("Invalid Julia commit")
    if not re.fullmatch(r"[0-9a-f]{64}", lock["sha256"]):
        raise ValueError("Invalid Julia archive SHA-256")
    match = re.fullmatch(r"(\d+\.\d+)\.\d+-DEV(?:\.\d+)?", lock["version"])
    if not match:
        raise ValueError("Expected an exact Julia development version")
    expected_url = archive_url(lock["commit"], match[1])
    if lock["url"] != expected_url:
        raise ValueError("Julia archive URL must name the pinned commit")


def archive_url(commit, minor):
    return f"{ARCHIVE_BASE}/{minor}/julia-{commit[:10]}-linux-x86_64.tar.gz"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def download(url, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as stream:
        partial = Path(stream.name)
        try:
            with urllib.request.urlopen(url, timeout=120) as response:
                shutil.copyfileobj(response, stream, length=1024 * 1024)
            stream.close()
            partial.replace(destination)
        finally:
            partial.unlink(missing_ok=True)
    return destination


def cached_archive(lock, cache, archive=None):
    cache.mkdir(parents=True, exist_ok=True)
    target = cache / (lock["sha256"] + ".tar.gz")
    if archive is not None:
        candidate = Path(archive)
    elif target.exists():
        candidate = target
    else:
        candidate = download(lock["url"], target)
    if digest(candidate) != lock["sha256"]:
        raise ValueError(f"Julia archive SHA-256 mismatch: {candidate}")
    if candidate.resolve() != target.resolve():
        shutil.copyfile(candidate, target)
    return target


def extract_archive(archive, destination):
    destination.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive) as stream:
        stream.extractall(destination, filter="data")
    roots = list(destination.iterdir())
    if len(roots) != 1 or not (roots[0] / "bin/julia").is_file():
        raise ValueError("Julia archive must contain one installation directory")
    return roots[0]


def julia_command(binary):
    # NixOS's stub loader cannot run the upstream ELF interpreter directly.
    # The project shell already provides the actual loader through NIX_CC.
    if platform.system() == "Linux" and os.environ.get("NIX_CC"):
        loader_file = Path(os.environ["NIX_CC"]) / "nix-support/dynamic-linker"
        if loader_file.is_file():
            return [
                loader_file.read_text().strip(),
                str(binary),
                "--home",
                str(binary.parent),
            ]
    return [str(binary)]


def runtime_identity(binary):
    env = os.environ.copy()
    env.pop("JULIA_PROJECT", None)
    env["JULIA_LOAD_PATH"] = "@stdlib"
    result = subprocess.run(
        julia_command(binary)
        + [
            "--startup-file=no",
            "--history-file=no",
            "-e",
            "println(VERSION); println(Base.GIT_VERSION_INFO.commit)",
        ],
        env=env,
        text=True,
        capture_output=True,
        check=True,
    )
    version, commit = result.stdout.strip().splitlines()
    return version, commit


def verify_runtime(binary, lock):
    version, commit = runtime_identity(binary)
    if commit != lock["commit"]:
        raise ValueError(
            f"Julia runtime commit mismatch: expected {lock['commit']}, got {commit}"
        )
    if version != lock["version"]:
        raise ValueError(
            f"Julia runtime version mismatch: expected {lock['version']}, got {version}"
        )


def install(lock, cache, archive=None, binary=None):
    validate_lock(lock)
    if binary is None:
        archive = cached_archive(lock, cache, archive)
        destination = cache / lock["sha256"]
        if not destination.exists():
            with tempfile.TemporaryDirectory(dir=cache) as temporary:
                root = extract_archive(archive, Path(temporary) / "unpacked")
                root.rename(destination)
        binary = destination / "bin/julia"
        launcher_dir = cache / (lock["sha256"] + "-launcher")
    else:
        binary = Path(binary).resolve()
        launcher_dir = cache / (lock["commit"] + "-source-launcher")
    verify_runtime(binary, lock)
    launcher_dir.mkdir(parents=True, exist_ok=True)
    launcher = launcher_dir / "julia"
    launcher.write_text(
        "#!/bin/sh\nexec " + shlex.join(julia_command(binary)) + ' "$@"\n'
    )
    launcher.chmod(0o755)
    return launcher_dir


def discover(cache):
    with urllib.request.urlopen(
        urllib.request.Request(LATEST_URL, method="HEAD"), timeout=30
    ) as response:
        commit = response.headers.get("x-amz-meta-build-commit", "")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Nightly discovery did not return a full Julia commit")
    with urllib.request.urlopen(
        f"{SOURCE_URL}/{commit}/VERSION", timeout=30
    ) as response:
        source_version = response.read().decode().strip()
    minor = re.match(r"\d+\.\d+", source_version)[0]
    url = archive_url(commit, minor)
    cache.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=cache) as temporary:
        archive = download(url, Path(temporary) / "julia.tar.gz")
        checksum = digest(archive)
        root = extract_archive(archive, Path(temporary) / "unpacked")
        version, runtime_commit = runtime_identity(root / "bin/julia")
        if runtime_commit != commit:
            raise ValueError(
                "Discovered archive does not match its Julia source commit"
            )
        lock = {
            "version": version,
            "commit": commit,
            "platform": "x86_64-linux-gnu",
            "url": url,
            "sha256": checksum,
        }
        validate_lock(lock)
        target = cache / (checksum + ".tar.gz")
        archive.replace(target)
        destination = cache / checksum
        if not destination.exists():
            root.rename(destination)
    return lock


def update_lock(path, cache):
    lock = discover(cache)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            stream.write(json.dumps(lock, indent=2) + "\n")
            stream.close()
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)
    return lock


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["update", "install", "archive"])
    parser.add_argument("--lock", type=Path, default=LOCK_PATH)
    parser.add_argument(
        "--archive", type=Path, help="Use a retained copy of the locked archive"
    )
    parser.add_argument(
        "--julia",
        type=Path,
        help="Explicitly use a source build of the locked Julia revision",
    )
    args = parser.parse_args()
    if args.julia and args.command != "install":
        parser.error("--julia is only supported by install")
    if platform.system() != "Linux" or platform.machine() not in ("x86_64", "AMD64"):
        parser.error("The pinned oracle binary requires Linux x86-64")
    cache = Path(
        os.environ.get(
            "FATOU_JULIA_ORACLE_CACHE",
            str(
                Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
                / "fatou/julia-oracle"
            ),
        )
    ).resolve()
    if args.command == "update":
        if args.archive or args.julia:
            parser.error(
                "update discovers an upstream build and cannot take local overrides"
            )
        lock = update_lock(args.lock, cache)
        print(f"Pinned Julia {lock['version']} at {lock['commit']}", file=sys.stderr)
    else:
        lock = json.loads(args.lock.read_text())
        validate_lock(lock)
        if args.command == "archive":
            print(cached_archive(lock, cache, args.archive))
        else:
            if args.archive and args.julia:
                parser.error("--archive and --julia cannot be combined")
            print(install(lock, cache, args.archive, args.julia))


if __name__ == "__main__":
    try:
        main()
    except (
        ValueError,
        OSError,
        subprocess.CalledProcessError,
        tarfile.TarError,
    ) as error:
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        sys.exit(f"error: {error}")

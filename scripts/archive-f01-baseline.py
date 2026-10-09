#!/usr/bin/env python3
"""Preserve the immutable F01 legacy CLI and generated v1 template materials."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
BASELINE_REVISION = "6d091b661d7ef82115e88cf142c7ff252b593864"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def material_hashes(output: Path) -> dict[str, str]:
    hashes = {}
    for path in sorted(output.rglob("*")):
        if path.is_symlink():
            raise ValueError("baseline materials must not contain symlinks")
        if path.is_file() and path != output / "manifest.json":
            hashes[path.relative_to(output).as_posix()] = digest(path)
    return hashes


def verify(output: Path, expected_producer: str, expected_dirty: bool) -> dict[str, Any]:
    manifest = json.loads((output / "manifest.json").read_text())
    if manifest.get("schema_version") != 1 or manifest.get("status") != "pass":
        raise ValueError("baseline archive did not complete")
    if manifest.get("baseline_revision") != BASELINE_REVISION:
        raise ValueError("legacy baseline revision differs from the immutable pin")
    if manifest.get("producer_revision") != expected_producer or manifest.get("producer_dirty") is not expected_dirty:
        raise ValueError("archive producer identity differs from the expected checkout")
    if manifest.get("driver_sha256") != digest(Path(__file__)):
        raise ValueError("archive driver hash differs from the checked out source")
    if manifest.get("files") != material_hashes(output):
        raise ValueError("baseline material inventory or hashes differ from the manifest")
    expected_source = subprocess.check_output(["git", "archive", BASELINE_REVISION], cwd=ROOT)
    if (output / "source.tar").read_bytes() != expected_source:
        raise ValueError("archived source differs from the immutable Git revision")
    with tarfile.open(output / "source.tar") as archive:
        original_runtime = archive.extractfile("templates/app/src/live.rs").read()
        original_lock = archive.extractfile("Cargo.lock").read()
    if (output / "project/crates/app/src/live.rs").read_bytes() != original_runtime:
        raise ValueError("generated v1 runtime differs from the original template")
    if b"const PROTO_VERSION: u32 = 1;" not in original_runtime:
        raise ValueError("legacy runtime does not declare protocol v1")
    if (output / "legacy-Cargo.lock").read_bytes() != original_lock:
        raise ValueError("legacy CLI lockfile differs from the original source")
    if not (output / "bin/gpui").is_file():
        raise ValueError("legacy CLI binary is missing")
    for path in ("project/crates/desktop/Cargo.toml", "project/mobile/ios/project.yml", "project/mobile/android/gradle/app/build.gradle.kts"):
        if not (output / path).is_file():
            raise ValueError(f"legacy scaffold is missing {path}")
    return {
        "status": "pass", "baseline_revision": BASELINE_REVISION,
        "producer_revision": expected_producer, "producer_dirty": expected_dirty,
        "cli_sha256": digest(output / "bin/gpui"), "source_sha256": digest(output / "source.tar"),
        "files": len(manifest["files"]), "runtime_protocol": 1,
        "application_build_or_launch": "not_run", "online_compatibility": "not_run",
    }


def archive_baseline(output: Path, cargo_target_dir: Path) -> None:
    output.mkdir(parents=True, exist_ok=False)
    commands = []

    def record(label: str, argv: list[str], cwd: Path = ROOT, env: dict[str, str] | None = None, stdout_name: str | None = None) -> bytes:
        entry = {"label": label, "argv": argv, "cwd": str(cwd)}
        commands.append(entry)
        started = time.monotonic()
        try:
            result = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, timeout=600)
            entry.update(exit_code=result.returncode, elapsed_seconds=time.monotonic() - started)
            (output / (stdout_name or f"{label}.stdout")).write_bytes(result.stdout)
            (output / f"{label}.stderr").write_bytes(result.stderr)
            if result.returncode != 0:
                raise RuntimeError(f"{label} failed with exit {result.returncode}; see retained logs")
            return result.stdout
        except subprocess.TimeoutExpired as error:
            entry.update(timeout_seconds=600, elapsed_seconds=time.monotonic() - started)
            (output / f"{label}.stdout").write_bytes(error.stdout or b"")
            (output / f"{label}.stderr").write_bytes(error.stderr or b"")
            raise
        finally:
            (output / "commands.json").write_text(json.dumps(commands, indent=2, sort_keys=True) + "\n")

    manifest = {"schema_version": 1, "baseline_revision": BASELINE_REVISION, "status": "fail"}
    try:
        manifest.update(
            producer_revision=record("producer-revision", ["git", "rev-parse", "HEAD"]).decode().strip(),
            producer_dirty=bool(record("producer-dirty", ["git", "status", "--porcelain"]).strip()),
            driver_sha256=digest(Path(__file__)),
            host={"platform": platform.platform(), "machine": platform.machine()},
            cargo=record("cargo-version", ["cargo", "--version"]).decode().strip(),
            cargo_target_dir=str(cargo_target_dir),
            rustc=record("rustc-version", ["rustc", "--version", "--verbose"]).decode().strip(),
            interpretation={"historical_source_rebuilt_with_recorded_toolchain": True, "historic_release_binary": False, "online_compatibility": "not_run", "application_build_or_launch": "not_run"},
        )
        resolved = record("baseline-revision", ["git", "rev-parse", f"{BASELINE_REVISION}^{{commit}}"]).decode().strip()
        if resolved != BASELINE_REVISION:
            raise ValueError("baseline revision did not resolve to the immutable pin")
        record("source-archive", ["git", "archive", BASELINE_REVISION], stdout_name="source.tar")
        with tempfile.TemporaryDirectory(prefix="gpui-f01-legacy-source-") as temporary:
            source = Path(temporary)
            with tarfile.open(output / "source.tar") as archive:
                archive.extractall(source, filter="data")
            shutil.copy2(source / "Cargo.lock", output / "legacy-Cargo.lock")
            env = os.environ.copy()
            env["CARGO_TARGET_DIR"] = str(cargo_target_dir)
            record("cargo-build", ["cargo", "build", "--locked", "--bin", "gpui"], cwd=source, env=env)
            record("protocol-tests", ["cargo", "test", "--locked", "devserver::protocol::tests", "--", "--nocapture"], cwd=source, env=env)
            (output / "bin").mkdir()
            binary = output / "bin/gpui"
            shutil.copy2(cargo_target_dir / "debug/gpui", binary)
            manifest["cli_version"] = record("cli-version", [str(binary), "--version"]).decode().strip()
            record("scaffold", [str(binary), "init", "f01-legacy-baseline", "--path", str(output / "project"), "--targets", "macos,ios,android", "--bundle-id", "com.example.f01legacybaseline"])
        manifest["status"] = "pass"
        manifest["files"] = material_hashes(output)
        (output / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        verify(output, manifest["producer_revision"], manifest["producer_dirty"])
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, tarfile.TarError) as error:
        manifest["status"] = "fail"
        manifest["error"] = str(error)
        raise
    finally:
        (output / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cargo-target-dir", type=Path, default=ROOT / "target/f01-legacy-baseline")
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--expected-producer", required=True)
    parser.add_argument("--expected-dirty", choices=("true", "false"), default="false")
    args = parser.parse_args()
    try:
        if not args.verify_only:
            archive_baseline(args.output.resolve(), args.cargo_target_dir.resolve())
        print(json.dumps(verify(args.output.resolve(), args.expected_producer, args.expected_dirty == "true"), indent=2, sort_keys=True))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, tarfile.TarError) as error:
        print(f"F01 baseline archive failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error


if __name__ == "__main__":
    main()

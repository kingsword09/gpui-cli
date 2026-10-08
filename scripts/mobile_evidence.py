"""Shared, bounded command recording for mobile CI evidence."""

from __future__ import annotations

import hashlib
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import signal
import struct
import subprocess
import tempfile
import time
import zlib


CANARY = "gpui-mobile-ci-secret-canary"
BUNDLE_ID = "com.example.gpuimobileci"
PROJECT_NAME = "mobile-ci-probe"


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def scrub(value: str) -> str:
    return value.replace(CANARY, "[redacted-canary]")


class Evidence:
    def __init__(self, output: Path):
        self.output = output.resolve()
        self.output.mkdir(parents=True, exist_ok=False)
        self.commands = []
        self.started = time.monotonic()
        self.write("summary.json", {"status": "running"})
        self.write("environment.json", {
            "system": platform.system(),
            "machine": platform.machine(),
            "github_sha": os.environ.get("GITHUB_SHA"),
            "github_run_id": os.environ.get("GITHUB_RUN_ID"),
            "github_run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
            "developer_dir": os.environ.get("DEVELOPER_DIR"),
            "android_home": os.environ.get("ANDROID_HOME"),
            "android_ndk_home": os.environ.get("ANDROID_NDK_HOME"),
            "android_avd_home": os.environ.get("ANDROID_AVD_HOME"),
            "android_user_home": os.environ.get("ANDROID_USER_HOME"),
            "java_home": os.environ.get("JAVA_HOME"),
            "recorded_at_utc": datetime.now(timezone.utc).isoformat(),
        })

    def write(self, name: str, value: object) -> None:
        serialized = json.dumps(value)
        if CANARY in serialized:
            raise RuntimeError("secret canary leaked into evidence")
        write_json(self.output / name, value)

    def run(self, label: str, argv: list[str], *, cwd: Path | None = None,
            env: dict[str, str] | None = None, timeout: int = 180,
            check: bool = True, binary: bool = False) -> dict:
        started = time.monotonic()
        stdout = b""
        stderr = b""
        returncode = None
        error = None
        try:
            process = subprocess.Popen(
                argv, cwd=cwd, env=env, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, start_new_session=True,
            )
            try:
                stdout, stderr = process.communicate(timeout=timeout)
                returncode = process.returncode
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                stdout, stderr = process.communicate()
                error = f"command exceeded {timeout}s deadline"
        except OSError as failure:
            error = str(failure)
        secret_leak = CANARY.encode() in stdout or CANARY.encode() in stderr
        stdout = stdout.replace(CANARY.encode(), b"[redacted-canary]")
        stderr = stderr.replace(CANARY.encode(), b"[redacted-canary]")
        index = len(self.commands)
        prefix = f"{index:03d}-{label}"
        stdout_name = f"{prefix}.{'bin' if binary else 'stdout'}"
        stderr_name = f"{prefix}.stderr"
        (self.output / stdout_name).write_bytes(stdout)
        (self.output / stderr_name).write_bytes(stderr)
        command = {
            "label": label, "argv": [scrub(item) for item in argv],
            "cwd": str(cwd or Path.cwd()), "returncode": returncode,
            "elapsed_seconds": time.monotonic() - started, "error": scrub(error or ""),
            "stdout_path": stdout_name, "stderr_path": stderr_name,
            "secret_leak": secret_leak,
        }
        self.commands.append(command)
        self.write("commands.json", self.commands)
        if secret_leak:
            raise RuntimeError(f"{label}: secret canary leaked; recorded output was redacted")
        if check and (returncode != 0 or error):
            raise RuntimeError(f"{label} failed: exit={returncode}, {error or stderr.decode(errors='replace')[-2000:]}")
        return {**command, "stdout": stdout if binary else stdout.decode(errors="replace"),
                "stderr": stderr.decode(errors="replace")}

    def context(self) -> None:
        root = Path(__file__).resolve().parent.parent
        revision = self.run("source-revision", ["git", "rev-parse", "HEAD"], cwd=root)
        dirty = self.run("source-status", ["git", "status", "--porcelain"], cwd=root)
        self.write("source.json", {"revision": revision["stdout"].strip(),
                                   "dirty": bool(dirty["stdout"].strip()),
                                   "drivers_sha256": {
                                       path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                                       for path in sorted((root / "scripts").glob("*.py"))
                                   },
                                   "workflow_sha256": hashlib.sha256(
                                       (root / ".github/workflows/ci.yml").read_bytes()).hexdigest()})
        self.run("rust-version", ["rustc", "-Vv"])

    def finish(self, status: str, **details: object) -> None:
        self.write("summary.json", {
            "schema_version": 1, "status": status,
            "elapsed_seconds": time.monotonic() - self.started,
            "verified_present": False, "gui_acceptance": "not_run", **details,
        })


def selected_check(report: dict, target: str) -> dict:
    checks = report.get("checks")
    if not isinstance(checks, list) or not checks:
        raise RuntimeError("doctor report has no checks")
    selected = [check for check in checks if check.get("id") == f"{target}.selected_device"]
    if len(selected) != 1 or selected[0].get("required") is not True:
        raise RuntimeError("doctor must contain exactly one required selected-device check")
    return selected[0]


def validate_doctor(report: dict, target: str, expected: str, phase: str,
                    device: str, abi: str | None = None) -> None:
    if report.get("schema_version") != 2 or report.get("target") != {
        "id": target, "source": "cli", "explicit": True,
    }:
        raise RuntimeError("doctor schema/explicit target mismatch")
    selected = selected_check(report, target)
    failures = [check["id"] for check in report["checks"]
                if check.get("required") is True and check.get("status") != "pass"]
    if selected.get("status") != expected:
        raise RuntimeError(f"selected-device status mismatch: {selected!r}")
    if expected == "pass":
        if failures or report.get("overall") not in {"pass", "warning"}:
            raise RuntimeError(f"unexpected required failures: {failures!r}")
    elif failures != [f"{target}.selected_device"] or report.get("overall") != "fail":
        raise RuntimeError(f"negative case failed for unrelated requirements: {failures!r}")
    if expected == "pass" or target == "android":
        actual = selected.get("actual", {})
        identities = {actual.get("id"), actual.get("state", {}).get("serial")}
        if device not in identities or actual.get("kind") != "emulator" or actual.get("platform") != target:
            raise RuntimeError(f"doctor selected an unexpected device: {actual!r}")
        state = "stopped" if phase == "cold" else "running"
        if actual.get("state", {}).get("kind") != state:
            raise RuntimeError(f"doctor device is not {state}: {actual!r}")
        if target == "android" and actual.get("arch") != abi:
            raise RuntimeError(f"device ABI was not {abi}: {actual!r}")
        if target == "android":
            build_abi = abi if expected == "pass" else ("arm64-v8a" if abi == "x86_64" else "x86_64")
            if selected.get("expected", {}).get("build_abis") != [build_abi]:
                raise RuntimeError("doctor did not check the configured build ABI set")


def retryable_ios_doctor_timeouts(report: dict, device: str, expected: str) -> bool:
    if report.get("schema_version") != 2 or report.get("target") != {
        "id": "ios", "source": "cli", "explicit": True,
    } or report.get("overall") != ("unknown" if expected == "pass" else "fail"):
        return False
    selected = selected_check(report, "ios")
    if selected.get("status") != expected:
        return False
    actual = selected.get("actual", {})
    if expected == "pass":
        if (actual.get("id") != device or actual.get("kind") != "emulator"
                or actual.get("platform") != "ios" or actual.get("state", {}).get("kind") != "running"):
            return False
    elif actual.get("available") is not False:
        return False
    failures = [check for check in report["checks"] if check.get("required") is True
                and check.get("id") != "ios.selected_device" and check.get("status") != "pass"]
    timeout_reasons = {"probe timed out", "total probe deadline exceeded before this check ran"}
    return bool(failures) and all(check.get("status") == "unknown" and check.get("reason") in timeout_reasons
                                  for check in failures)


def doctor(evidence: Evidence, gpui: Path, target: str, device: str,
           phase: str, abi: str = "x86_64", *, timeout_retries: int = 0) -> dict:
    if timeout_retries not in (0, 1, 2) or (timeout_retries and (target != "ios" or phase != "live")):
        raise ValueError("bounded doctor timeout retries are only supported for live iOS")
    env = {**os.environ, "GPUI_DOCTOR_SECRET_CANARY": CANARY}
    retries = []
    reports = {}
    with tempfile.TemporaryDirectory(prefix="gpui-mobile-doctor-") as scratch:
        root = Path(scratch)
        project = root / PROJECT_NAME
        evidence.run("generate", [str(gpui), "init", PROJECT_NAME, "--path", str(project),
                                  "--targets", target, "--bundle-id", BUNDLE_ID], env=env)
        flag = "--avd" if target == "android" and phase == "cold" else "--device"
        cases = [("match", device, "pass", 0)]
        negative_device = device if target == "android" else "00000000-0000-0000-0000-000000000000"
        cases.append(("mismatch" if target == "android" else "missing-device", negative_device, "fail", 1))
        for label, selector, expected, expected_exit in cases:
            case_env = dict(env)
            if target == "android":
                case_env["GPUI_ANDROID_ABIS"] = abi if label == "match" else (
                    "arm64-v8a" if abi == "x86_64" else "x86_64")
            for attempt in range(timeout_retries + 1):
                attempt_label = label if attempt == 0 else f"{label}-retry-{attempt}"
                command = evidence.run(attempt_label, [str(gpui), "doctor", "--json", "--target", target,
                                                       flag, selector], cwd=project, env=case_env, check=False)
                report = json.loads(command["stdout"])
                report_name = f"{attempt_label}-doctor.json"
                evidence.write(report_name, report)
                try:
                    if command["returncode"] != expected_exit:
                        failures = [(check.get("id"), check.get("status"), check.get("reason"))
                                    for check in report.get("checks", [])
                                    if check.get("required") is True and check.get("status") != "pass"]
                        raise RuntimeError(f"doctor {attempt_label} exit was {command['returncode']}, "
                                           f"expected {expected_exit}; required failures: {failures!r}")
                    validate_doctor(report, target, expected, phase, device, abi)
                except RuntimeError:
                    if (attempt >= timeout_retries or command["returncode"] != 1
                            or not retryable_ios_doctor_timeouts(report, device, expected)):
                        raise
                    retries.append({"failed_report": report_name, "next_attempt": attempt + 1,
                                    "delay_seconds": 15, "reason": "required_probe_timeouts_only"})
                    evidence.write("doctor-retries.json", retries)
                    time.sleep(15)
                else:
                    reports[label] = report_name
                    break
    result = {"timeout_retries_used": len(retries), "reports": reports}
    evidence.write("doctor-result.json", result)
    return result


def validate_png(data: bytes) -> dict:
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise RuntimeError("capture is not a PNG")
    offset = 8
    dimensions = None
    image_data = []
    row_size = None
    while offset + 12 <= len(data):
        length = struct.unpack_from(">I", data, offset)[0]
        kind = data[offset + 4:offset + 8]
        end = offset + 12 + length
        if end > len(data):
            raise RuntimeError("truncated PNG chunk")
        payload = data[offset + 8:offset + 8 + length]
        checksum = struct.unpack_from(">I", data, offset + 8 + length)[0]
        if zlib.crc32(kind + payload) != checksum:
            raise RuntimeError("PNG chunk checksum mismatch")
        if offset == 8:
            if kind != b"IHDR" or length != 13:
                raise RuntimeError("PNG missing IHDR")
            dimensions = struct.unpack_from(">II", payload)
            if not all(dimensions):
                raise RuntimeError("PNG has empty dimensions")
            depth, color, compression, filtering, interlace = payload[8:]
            channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}.get(color)
            depths = {0: {1, 2, 4, 8, 16}, 2: {8, 16}, 3: {1, 2, 4, 8}, 4: {8, 16}, 6: {8, 16}}
            if not channels or depth not in depths[color] or compression or filtering or interlace:
                raise RuntimeError("unsupported capture PNG format")
            row_size = (dimensions[0] * channels * depth + 7) // 8 + 1
            if row_size * dimensions[1] > 256 * 1024 * 1024:
                raise RuntimeError("capture PNG exceeds decoded size limit")
        if kind == b"IDAT":
            image_data.append(payload)
        if kind == b"IEND":
            if length or end != len(data) or not image_data:
                raise RuntimeError("invalid PNG end")
            decoder = zlib.decompressobj()
            try:
                pixels = decoder.decompress(b"".join(image_data), row_size * dimensions[1] + 1)
            except zlib.error as error:
                raise RuntimeError("invalid PNG compressed pixels") from error
            if not decoder.eof or decoder.unused_data or len(pixels) != row_size * dimensions[1]:
                raise RuntimeError("PNG pixel data does not match dimensions")
            if any(pixels[index] > 4 for index in range(0, len(pixels), row_size)):
                raise RuntimeError("invalid PNG row filter")
            return {"width": dimensions[0], "height": dimensions[1],
                    "sha256": hashlib.sha256(data).hexdigest()}
        offset = end
    raise RuntimeError("PNG missing IEND")


def build_artifact(evidence: Evidence, project: Path, target: str, suffix: str) -> Path:
    root = (project / ".gpui/builds" / target).resolve()
    manifests = list(root.glob("*/artifact-manifest.json"))
    if len(manifests) > 1 or (target == "ios" and not manifests):
        raise RuntimeError(f"ambiguous or missing {target} build manifest")
    manifest = None
    if manifests:
        manifest = json.loads(manifests[0].read_text())
        if manifest.get("schema_version") != 1 or manifest.get("platform") != target:
            raise RuntimeError("build manifest schema/platform mismatch")
        evidence.write("build-artifact-manifest.json", manifest)
    if target == "android":
        candidates = list(root.glob("*/gradle-build/outputs/apk/debug/*.apk"))
    else:
        candidates = [manifests[0].parent / entry for entry in manifest["roots"] if entry.endswith(suffix)]
    if len(candidates) != 1 or not candidates[0].resolve().is_relative_to(root) or not candidates[0].exists():
        raise RuntimeError("build did not publish exactly one contained application artifact")
    if target == "android" and manifest is not None:
        relative = candidates[0].relative_to(manifests[0].parent).as_posix()
        entries = [entry for entry in manifest["files"] if entry["path"] == relative]
        if len(entries) != 1 or entries[0]["sha256"] != hashlib.sha256(candidates[0].read_bytes()).hexdigest():
            raise RuntimeError("APK is not the artifact recorded in the build manifest")
    evidence.write("artifact-selection.json", {"path": str(candidates[0].relative_to(root)),
                                              "cache_manifest_available": manifest is not None})
    return candidates[0]

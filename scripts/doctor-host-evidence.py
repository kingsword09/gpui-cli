#!/usr/bin/env python3
"""Run target-aware doctor on a real host and persist reviewable evidence.

The script is intentionally limited to host and generated-project doctor
checks.  It does not install SDKs, boot devices, or alter signing state.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
from typing import Any


CANARY = "gpui-doctor-host-secret-canary"


def run_command(argv: list[str], cwd: Path, env: dict[str, str]) -> dict[str, Any]:
    try:
        completed = subprocess.run(
            argv,
            cwd=cwd,
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=180,
        )
        result: dict[str, Any] = {
            "argv": argv,
            "cwd": str(cwd),
            "returncode": completed.returncode,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
        }
    except (OSError, subprocess.SubprocessError) as error:
        result = {
            "argv": argv,
            "cwd": str(cwd),
            "returncode": None,
            "stdout": "",
            "stderr": str(error),
        }
    return result


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def parse_report(command: dict[str, Any], label: str) -> dict[str, Any]:
    stdout = command["stdout"]
    try:
        report = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{label} did not emit JSON: {error}") from error
    if not isinstance(report, dict):
        raise RuntimeError(f"{label} JSON root is not an object")
    return report


def assert_report(report: dict[str, Any], source: str, label: str) -> None:
    if report.get("target", {}).get("id") != "desktop":
        raise RuntimeError(f"{label} selected the wrong target: {report.get('target')!r}")
    if report.get("target", {}).get("source") != source:
        raise RuntimeError(f"{label} selected the wrong source: {report.get('target')!r}")
    if report.get("overall") not in {"pass", "warning"}:
        raise RuntimeError(f"{label} has unexpected overall status: {report.get('overall')!r}")
    checks = report.get("checks")
    if not isinstance(checks, list) or not checks:
        raise RuntimeError(f"{label} has no checks")
    failed_required = [
        check.get("id")
        for check in checks
        if check.get("required") is True and check.get("status") != "pass"
    ]
    if failed_required:
        raise RuntimeError(f"{label} required checks failed: {failed_required!r}")
    mobile_checks = [
        check.get("id")
        for check in checks
        if str(check.get("id", "")).startswith(("android.", "ios."))
    ]
    if mobile_checks:
        raise RuntimeError(f"{label} unexpectedly contains mobile checks: {mobile_checks!r}")


def tool_version(argv: list[str], cwd: Path, env: dict[str, str]) -> dict[str, Any]:
    executable = shutil.which(argv[0], path=env.get("PATH"))
    if executable is None:
        return {"argv": argv, "available": False}
    result = run_command(argv, cwd, env)
    return {
        "argv": argv,
        "available": result["returncode"] == 0,
        "returncode": result["returncode"],
        "stdout": result["stdout"],
        "stderr": result["stderr"],
    }


def host_target() -> str:
    system = platform.system().lower()
    if system == "darwin":
        return "macos"
    if system == "windows":
        return "windows"
    if system == "linux":
        return "linux"
    raise RuntimeError(f"unsupported host system {system!r}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gpui", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    gpui = args.gpui
    if not gpui.exists() and os.name == "nt" and gpui.suffix.lower() != ".exe":
        gpui = gpui.with_suffix(".exe")
    gpui = gpui.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["GPUI_DOCTOR_SECRET_CANARY"] = CANARY
    commands: list[dict[str, Any]] = []

    with tempfile.TemporaryDirectory(prefix="gpui-doctor-host-") as scratch:
        scratch_root = Path(scratch)
        project = scratch_root / "doctor-host-project"
        init = run_command(
            [
                str(gpui),
                "init",
                "doctor-host-project",
                "--path",
                str(project),
                "--targets",
                host_target(),
                "--bundle-id",
                "com.example.doctorhost",
            ],
            scratch_root,
            env,
        )
        commands.append({"label": "init", **init})
        if init["returncode"] != 0:
            raise RuntimeError(f"gpui init failed: {init['stderr']}")

        explicit = run_command(
            [str(gpui), "doctor", "--json", "--target", "desktop"],
            scratch_root,
            env,
        )
        commands.append({"label": "doctor-explicit", **explicit})
        explicit_report = parse_report(explicit, "explicit doctor")
        assert_report(explicit_report, "cli", "explicit doctor")

        project_default = run_command(
            [str(gpui), "doctor", "--json"],
            project,
            env,
        )
        commands.append({"label": "doctor-project-default", **project_default})
        project_report = parse_report(project_default, "project-default doctor")
        assert_report(project_report, "project", "project-default doctor")

        environment = {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "platform": platform.platform(),
            "python": platform.python_version(),
            "tools": {
                "rustc": tool_version(["rustc", "-Vv"], scratch_root, env),
                "cargo": tool_version(["cargo", "-V"], scratch_root, env),
                "cc": tool_version(["cc", "--version"], scratch_root, env),
            },
        }

    evidence_text = json.dumps({"commands": commands, "environment": environment}, sort_keys=True)
    reports_text = json.dumps({"explicit": explicit_report, "project": project_report}, sort_keys=True)
    if CANARY in evidence_text or CANARY in reports_text:
        raise RuntimeError("doctor secret canary leaked into evidence")

    write_json(output / "environment.json", environment)
    write_json(output / "explicit-doctor.json", explicit_report)
    write_json(output / "project-default-doctor.json", project_report)
    write_json(output / "commands.json", commands)
    write_json(
        output / "summary.json",
        {
            "schema_version": 1,
            "target": "desktop",
            "host_target": host_target(),
            "explicit_overall": explicit_report["overall"],
            "project_default_overall": project_report["overall"],
            "required_checks": "pass",
            "secret_canary": "absent",
        },
    )
    print(json.dumps({"output": str(output), "host_target": host_target()}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

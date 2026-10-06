#!/usr/bin/env python3
"""Run Android doctor against a live emulator for ABI match/mismatch evidence."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
from typing import Any


CANARY = "gpui-doctor-android-secret-canary"


def run_command(argv: list[str], cwd: Path, env: dict[str, str]) -> dict[str, Any]:
    try:
        completed = subprocess.run(
            argv,
            cwd=cwd,
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=240,
        )
    except (OSError, subprocess.SubprocessError) as error:
        return {"argv": argv, "cwd": str(cwd), "returncode": None, "stdout": "", "stderr": str(error)}
    return {
        "argv": argv,
        "cwd": str(cwd),
        "returncode": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    }


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def report_from(command: dict[str, Any], label: str) -> dict[str, Any]:
    try:
        report = json.loads(command["stdout"])
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{label} did not emit JSON: {error}") from error
    if not isinstance(report, dict):
        raise RuntimeError(f"{label} JSON root is not an object")
    return report


def selected_check(report: dict[str, Any]) -> dict[str, Any]:
    for check in report.get("checks", []):
        if check.get("id") == "android.selected_device":
            return check
    raise RuntimeError("doctor report did not contain android.selected_device")


def validate_report(report: dict[str, Any], expected_status: str, label: str) -> None:
    if report.get("target", {}).get("id") != "android":
        raise RuntimeError(f"{label} selected the wrong target: {report.get('target')!r}")
    check = selected_check(report)
    if check.get("status") != expected_status:
        raise RuntimeError(f"{label} selected-device status was {check.get('status')!r}: {check!r}")
    required = [item for item in report.get("checks", []) if item.get("required") is True]
    if expected_status == "pass" and any(item.get("status") != "pass" for item in required):
        raise RuntimeError(f"{label} has an unexpected required failure: {required!r}")
    if expected_status == "fail" and report.get("overall") != "fail":
        raise RuntimeError(f"{label} mismatch did not fail overall: {report.get('overall')!r}")


def adb_probe(device: str, env: dict[str, str]) -> dict[str, Any]:
    return {
        "devices": run_command(["adb", "devices", "-l"], Path.cwd(), env),
        "abi": run_command(
            ["adb", "-s", device, "shell", "getprop", "ro.product.cpu.abi"],
            Path.cwd(),
            env,
        ),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gpui", type=Path, required=True)
    parser.add_argument("--device", required=True)
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
    reports: dict[str, dict[str, Any]] = {}

    with tempfile.TemporaryDirectory(prefix="gpui-doctor-android-") as scratch:
        root = Path(scratch)
        project = root / "doctor-android-project"
        init = run_command(
            [
                str(gpui),
                "init",
                "doctor-android-project",
                "--path",
                str(project),
                "--targets",
                "android",
                "--bundle-id",
                "com.example.doctorandroid",
            ],
            root,
            env,
        )
        commands.append({"label": "init", **init})
        if init["returncode"] != 0:
            raise RuntimeError(f"gpui init failed: {init['stderr']}")

        for label, abis, expected_status, expected_exit in (
            ("match", "x86_64", "pass", 0),
            ("mismatch", "arm64-v8a", "fail", 1),
        ):
            case_env = {**env, "GPUI_ANDROID_ABIS": abis}
            command = run_command(
                [
                    str(gpui),
                    "doctor",
                    "--json",
                    "--target",
                    "android",
                    "--device",
                    args.device,
                ],
                project,
                case_env,
            )
            commands.append({"label": f"doctor-{label}", "abis": abis, **command})
            if command["returncode"] != expected_exit:
                raise RuntimeError(
                    f"doctor {label} exit was {command['returncode']!r}, expected {expected_exit}"
                )
            report = report_from(command, f"doctor {label}")
            write_json(output / f"{label}-raw-doctor.json", report)
            print(
                json.dumps(
                    {
                        "label": label,
                        "exit": command["returncode"],
                        "overall": report.get("overall"),
                        "checks": [
                            {
                                "id": item.get("id"),
                                "status": item.get("status"),
                                "required": item.get("required"),
                                "reason": item.get("reason"),
                            }
                            for item in report.get("checks", [])
                        ],
                    },
                    sort_keys=True,
                ),
                flush=True,
            )
            validate_report(report, expected_status, f"doctor {label}")
            reports[label] = report

        adb = adb_probe(args.device, env)

    serialized = json.dumps({"commands": commands, "reports": reports, "adb": adb}, sort_keys=True)
    if CANARY in serialized:
        raise RuntimeError("doctor secret canary leaked into evidence")
    write_json(output / "commands.json", commands)
    write_json(output / "match-doctor.json", reports["match"])
    write_json(output / "mismatch-doctor.json", reports["mismatch"])
    write_json(output / "adb.json", adb)
    write_json(
        output / "environment.json",
        {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "device": args.device,
            "match_abis": "x86_64",
            "mismatch_abis": "arm64-v8a",
            "secret_canary": "absent",
        },
    )
    write_json(
        output / "summary.json",
        {
            "schema_version": 1,
            "device": args.device,
            "match": {"abi_set": "x86_64", "status": "pass", "exit": 0},
            "mismatch": {"abi_set": "arm64-v8a", "status": "fail", "exit": 1},
            "secret_canary": "absent",
        },
    )
    print(json.dumps({"output": str(output), "device": args.device}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

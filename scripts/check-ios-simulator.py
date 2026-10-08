#!/usr/bin/env python3
"""Own one simulator for cold doctor or full-app process/capture smoke, not GUI acceptance."""

import argparse
import json
from pathlib import Path
import plistlib
import re
import tempfile
import time
import uuid

from mobile_evidence import BUNDLE_ID, PROJECT_NAME, Evidence, build_artifact, doctor, scrub, validate_png


def select_runtime(catalog: dict, version: str) -> str:
    runtimes = [runtime for runtime in catalog["runtimes"]
                if runtime.get("isAvailable") and runtime.get("version") == version
                and runtime["identifier"].startswith("com.apple.CoreSimulator.SimRuntime.iOS-")]
    if len(runtimes) != 1:
        raise RuntimeError(f"expected exactly one available iOS {version} runtime")
    return runtimes[0]["identifier"]


def build(evidence: Evidence, gpui: Path, project: Path, device: str) -> Path:
    evidence.run("generate-app", [str(gpui), "init", PROJECT_NAME, "--path", str(project),
                                  "--targets", "ios", "--bundle-id", BUNDLE_ID])
    evidence.run("lock-app", ["cargo", "generate-lockfile"], cwd=project, timeout=300)
    (evidence.output / "Cargo.lock").write_bytes((project / "Cargo.lock").read_bytes())
    evidence.run("build-app", [str(gpui), "build", "ios", "--device", device], cwd=project, timeout=1200)
    return build_artifact(evidence, project, "ios", ".app")


def smoke(evidence: Evidence, app: Path, device: str) -> dict:
    probe = 'import Metal; import CoreGraphics; import Darwin; guard let device = MTLCreateSystemDefaultDevice() else { print("Metal unavailable"); exit(1) }; print(device.name)'
    evidence.run("host-metal", ["xcrun", "swift", "-framework", "CoreGraphics", "-e", probe], timeout=120)
    with (app / "Info.plist").open("rb") as source:
        executable = plistlib.load(source)["CFBundleExecutable"]
    evidence.run("install", ["xcrun", "simctl", "install", device, str(app)])
    installed = evidence.run("installed-app", ["xcrun", "simctl", "get_app_container", device, BUNDLE_ID, "app"])
    expected_executable = str(Path(installed["stdout"].strip()) / executable)
    launch = evidence.run("launch", ["xcrun", "simctl", "launch", "--terminate-running-process",
                          f"--stdout={evidence.output / 'app.stdout'}",
                          f"--stderr={evidence.output / 'app.stderr'}", device, BUNDLE_ID])
    matched = re.fullmatch(rf"{re.escape(BUNDLE_ID)}:\s*(\d+)\s*", launch["stdout"].strip())
    if not matched:
        raise RuntimeError("simctl launch did not return the selected application's PID")
    pid = matched[1]
    for sample in range(6):
        identity = evidence.run(f"process-{sample}", ["ps", "-ww", "-p", pid, "-o", "comm="])
        if identity["stdout"].strip() != expected_executable:
            raise RuntimeError("launched PID exited or changed executable")
        if sample < 5:
            time.sleep(1)
    capture = evidence.output / "capture.png"
    evidence.run("capture", ["xcrun", "simctl", "io", device, "screenshot", str(capture)])
    png = validate_png(capture.read_bytes())
    evidence.write("capture.json", {**png, "scope": "device", "foreground_verified": False,
                                    "device": device, "application_pid": int(pid)})
    evidence.run("native-logs", ["xcrun", "simctl", "spawn", device, "log", "show", "--style", "json",
                 "--last", "2m", "--predicate", f"processID == {pid}"], timeout=60)
    identity = evidence.run("process-after-capture", ["ps", "-ww", "-p", pid, "-o", "comm="])
    if identity["stdout"].strip() != expected_executable:
        raise RuntimeError("application did not survive capture/log collection")
    return {"pid": int(pid), "identity_confidence": "pid_and_executable", "capture": png,
            "application_ready": "not_instrumented", "simulator_metal": "not_instrumented"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gpui", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--mode", choices=("doctor", "build", "smoke"), required=True)
    parser.add_argument("--runtime", default="26.2")
    parser.add_argument("--device-type", default="com.apple.CoreSimulator.SimDeviceType.iPhone-16")
    args = parser.parse_args()
    evidence = Evidence(args.output)
    device = None
    failure = None
    details = {}
    cleanup = {"status": "not_needed"}
    try:
        evidence.context()
        gpui = args.gpui.resolve(strict=True)
        evidence.run("xcode-version", ["xcodebuild", "-version"])
        evidence.run("xcode-sdks", ["xcodebuild", "-showsdks"])
        catalog = evidence.run("runtimes", ["xcrun", "simctl", "list", "runtimes", "--json"])
        runtime = select_runtime(json.loads(catalog["stdout"]), args.runtime)
        creation = evidence.run("create", ["xcrun", "simctl", "create", f"gpui-ci-{uuid.uuid4().hex[:12]}",
                                          args.device_type, runtime])
        device = str(uuid.UUID(creation["stdout"].strip())).upper()
        evidence.write("device.json", {"udid": device, "runtime": runtime, "owned": True})
        if args.mode == "doctor":
            doctor(evidence, gpui, "ios", device, "cold")
        else:
            with tempfile.TemporaryDirectory(prefix="gpui-ios-build-") as scratch:
                app = build(evidence, gpui, Path(scratch) / PROJECT_NAME, device)
                details["build_status"] = "pass"
                if args.mode == "smoke":
                    evidence.run("boot", ["xcrun", "simctl", "bootstatus", device, "-b"], timeout=300)
                    doctor(evidence, gpui, "ios", device, "live")
                    details.update(smoke(evidence, app, device))
    except Exception as error:
        failure = scrub(str(error))
    finally:
        if device is not None:
            try:
                evidence.run("diagnostics", ["xcrun", "simctl", "spawn", device, "log", "show",
                             "--style", "json", "--last", "2m"], check=False, timeout=45)
                evidence.run("shutdown", ["xcrun", "simctl", "shutdown", device], check=False, timeout=60)
                evidence.run("delete", ["xcrun", "simctl", "delete", device], timeout=60)
                remaining = evidence.run("devices-after-cleanup", ["xcrun", "simctl", "list", "devices", "--json"])
                if any(item["udid"].upper() == device for devices in json.loads(remaining["stdout"])["devices"].values()
                       for item in devices):
                    raise RuntimeError("owned simulator still exists after cleanup")
                cleanup = {"status": "pass", "deleted_udid": device}
            except Exception as error:
                cleanup = {"status": "fail", "error": scrub(str(error))}
                failure = failure or cleanup["error"]
        evidence.finish("fail" if failure else "pass", platform="ios", mode=args.mode,
                        device=device, error=failure, cleanup=cleanup, **details)
    if failure:
        print(failure)
    return int(failure is not None)


if __name__ == "__main__":
    raise SystemExit(main())

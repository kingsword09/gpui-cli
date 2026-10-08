#!/usr/bin/env python3
"""Build an unmodified GPUI APK and record process/capture smoke on a booted emulator."""

import argparse
import hashlib
import os
from pathlib import Path
import tempfile
import time
import uuid
import zipfile

from mobile_evidence import BUNDLE_ID, PROJECT_NAME, Evidence, build_artifact, scrub, validate_png


def smoke(evidence: Evidence, adb: list[str], apk: Path, bundle_id: str, device: str) -> dict:
    evidence.run("install", [*adb, "install", str(apk)])
    evidence.run("launch", [*adb, "shell", "am", "start", "-W", "-n",
                             f"{bundle_id}/dev.gpui.mobile.GpuiActivity"])
    pid = None
    for sample in range(6):
        identity = evidence.run(f"process-{sample}", [*adb, "shell", "pidof", bundle_id])
        found = identity["stdout"].strip()
        if not found.isdecimal() or (pid is not None and found != pid):
            raise RuntimeError("application exited, restarted or has ambiguous PID")
        pid = found
        if sample < 5:
            time.sleep(1)
    screenshot = evidence.run("capture", [*adb, "exec-out", "screencap", "-p"], binary=True)
    png = validate_png(screenshot["stdout"])
    (evidence.output / "capture.png").write_bytes(screenshot["stdout"])
    evidence.write("capture.json", {**png, "device": device, "application_pid": int(pid),
                                    "scope": "device", "foreground_verified": False})
    evidence.run("app-logcat", [*adb, "logcat", "-d", "-v", "threadtime", f"--pid={pid}"])
    final_pid = evidence.run("process-after-capture", [*adb, "shell", "pidof", bundle_id])
    if final_pid["stdout"].strip() != pid:
        raise RuntimeError("application did not survive capture/log collection")
    return {"pid": int(pid), "identity_confidence": "package_and_pid", "capture": png,
            "application_ready": "not_instrumented"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gpui", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--device")
    parser.add_argument("--build-only", action="store_true")
    parser.add_argument("--abi", choices=("x86_64", "arm64-v8a"), default="x86_64")
    args = parser.parse_args()
    if not args.build_only and args.device is None:
        parser.error("--device is required for runtime smoke")
    bundle_id = f"{BUNDLE_ID}.r{uuid.uuid4().hex}"
    evidence = Evidence(args.output)
    install_attempted = False
    failure = None
    details = {}
    cleanup = {"status": "not_needed"}
    adb = ["adb", "-s", args.device] if args.device else []
    try:
        evidence.context()
        if not args.build_only and not args.device.startswith("emulator-"):
            raise RuntimeError("runtime smoke only owns an emulator application, not a physical device")
        gpui = args.gpui.resolve(strict=True)
        if not args.build_only:
            boot = evidence.run("boot-completed", [*adb, "shell", "getprop", "sys.boot_completed"])
            abi = evidence.run("device-abi", [*adb, "shell", "getprop", "ro.product.cpu.abi"])
            if boot["stdout"].strip() != "1" or abi["stdout"].strip() != args.abi:
                raise RuntimeError("selected emulator is not booted with the expected ABI")
            existing = evidence.run("existing-package", [*adb, "shell", "pm", "list", "packages", bundle_id])
            if existing["stdout"].strip():
                raise RuntimeError("refusing to replace an existing application")
        with tempfile.TemporaryDirectory(prefix="gpui-android-smoke-") as scratch:
            project = Path(scratch) / PROJECT_NAME
            env = {**os.environ, "GPUI_ANDROID_ABIS": args.abi}
            evidence.run("generate-app", [str(gpui), "init", PROJECT_NAME, "--path", str(project),
                                          "--targets", "android", "--bundle-id", bundle_id], env=env)
            evidence.run("lock-app", ["cargo", "generate-lockfile"], cwd=project, env=env, timeout=300)
            (evidence.output / "Cargo.lock").write_bytes((project / "Cargo.lock").read_bytes())
            evidence.run("build-app", [str(gpui), "build", "android"], cwd=project, env=env, timeout=1200)
            apk = build_artifact(evidence, project, "android", ".apk")
            with zipfile.ZipFile(apk) as archive:
                libraries = [name for name in archive.namelist() if name.startswith("lib/") and name.endswith(".so")]
                library = f"lib/{args.abi}/libmobile_ci_probe_app.so"
                if library not in libraries or {name.split("/")[1] for name in libraries} != {args.abi}:
                    raise RuntimeError("APK does not contain the requested native GPUI ABI")
            evidence.write("apk.json", {"sha256": hashlib.sha256(apk.read_bytes()).hexdigest(),
                                      "abi": args.abi, "libraries": libraries, "stub": False, "bundle_id": bundle_id})
            details["build_status"] = "pass"
            if not args.build_only:
                install_attempted = True
                details.update(smoke(evidence, adb, apk, bundle_id, args.device))
    except Exception as error:
        failure = scrub(str(error))
    finally:
        try:
            if args.device and args.device.startswith("emulator-"):
                evidence.run("crash-logcat", [*adb, "logcat", "-d", "-b", "crash", "-v", "threadtime"], check=False, timeout=30)
            if install_attempted:
                package = evidence.run("package-before-cleanup", [*adb, "shell", "pm", "list", "packages", bundle_id])
                if package["stdout"].strip():
                    evidence.run("stop", [*adb, "shell", "am", "force-stop", bundle_id])
                    evidence.run("uninstall", [*adb, "uninstall", bundle_id])
                remaining = evidence.run("package-after-cleanup", [*adb, "shell", "pm", "list", "packages", bundle_id])
                if remaining["stdout"].strip():
                    raise RuntimeError("owned application remains installed")
                cleanup = {"status": "pass", "uninstalled_bundle_id": bundle_id}
        except Exception as error:
            cleanup = {"status": "fail", "error": scrub(str(error))}
            failure = failure or cleanup["error"]
        evidence.finish("fail" if failure else "pass", platform="android", device=args.device,
                        mode="build" if args.build_only else "smoke", error=failure, cleanup=cleanup, **details)
    if failure:
        print(failure)
    return int(failure is not None)


if __name__ == "__main__":
    raise SystemExit(main())

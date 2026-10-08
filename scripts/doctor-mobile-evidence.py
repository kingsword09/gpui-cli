#!/usr/bin/env python3
"""Record cold inventory or live mobile doctor evidence without booting devices."""

import argparse
from pathlib import Path

from mobile_evidence import Evidence, doctor, scrub


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gpui", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--platform", choices=("android", "ios"), required=True)
    parser.add_argument("--phase", choices=("cold", "live"), required=True)
    parser.add_argument("--device", required=True)
    parser.add_argument("--abi", choices=("x86_64", "arm64-v8a"), default="x86_64")
    args = parser.parse_args()
    evidence = Evidence(args.output)
    try:
        evidence.context()
        if args.platform == "android" and args.phase == "live":
            evidence.run("adb-devices", ["adb", "devices", "-l"])
            boot = evidence.run("adb-boot", ["adb", "-s", args.device, "shell", "getprop", "sys.boot_completed"])
            actual = evidence.run("adb-abi", ["adb", "-s", args.device, "shell", "getprop", "ro.product.cpu.abi"])
            if boot["stdout"].strip() != "1" or actual["stdout"].strip() != args.abi:
                raise RuntimeError("selected emulator is not booted with the expected ABI")
        doctor(evidence, args.gpui.resolve(strict=True), args.platform, args.device, args.phase, args.abi)
        evidence.finish("pass", platform=args.platform, phase=args.phase, device=args.device)
        return 0
    except Exception as error:
        evidence.finish("fail", platform=args.platform, phase=args.phase, device=args.device, error=scrub(str(error)))
        print(scrub(str(error)))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

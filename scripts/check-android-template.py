#!/usr/bin/env python3
"""Build generated Android hosts and inspect their APKs; no device required."""

import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import zipfile


def run(*args, cwd=None, env=None):
    subprocess.run(args, cwd=cwd, env=env, check=True)


def check_apk(gradle, variant, expected_abis):
    directory = gradle / "app/build/outputs/apk" / variant
    metadata = json.loads((directory / "output-metadata.json").read_text())
    outputs = metadata["elements"]
    assert len(outputs) == 1, outputs
    apk = directory / outputs[0]["outputFile"]
    if variant == "release":
        assert apk.name.endswith("-unsigned.apk"), apk
    with zipfile.ZipFile(apk) as archive:
        actual = {
            name.split("/")[1]
            for name in archive.namelist()
            if name.startswith("lib/") and name.endswith("/libtemplate_probe_app.so")
        }
    assert actual == expected_abis, (variant, actual, expected_abis)
    print(f"Verified {variant}: {apk.name}, ABIs {sorted(actual)}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gpui", type=Path, required=True)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    gpui = args.gpui.resolve(strict=True)
    ndk = Path(os.environ["ANDROID_NDK_HOME"])
    host = {"Darwin": "darwin-x86_64", "Linux": "linux-x86_64"}[platform.system()]
    compilers = ndk / "toolchains/llvm/prebuilt" / host / "bin"
    source = Path(__file__).resolve().parent.parent / "tests/fixtures/native_probe.c"
    env = {**os.environ, "GPUI_ANDROID_ABIS": "arm64-v8a,x86_64"}
    with tempfile.TemporaryDirectory(prefix="gpui-android-template-") as scratch:
        project = Path(scratch) / "app"
        run(str(gpui), "init", "template-probe", "--path", str(project),
            "--targets", "android", "--title", 'R&D "Desk" <工具> \\path {{APP_CRATE}}',
            "--bundle-id", "com.example.templateprobe")
        gradle = project / "mobile/android/gradle"
        # Tiny real ELF libraries keep this check about resource generation,
        # Gradle and ABI packaging rather than the GPUI dependency build.
        for abi, target in [("arm64-v8a", "aarch64-linux-android"), ("x86_64", "x86_64-linux-android")]:
            output = gradle / "app/src/main/jniLibs" / abi / "libtemplate_probe_app.so"
            output.parent.mkdir(parents=True, exist_ok=True)
            run(str(compilers / f"{target}31-clang"), "-shared", "-fPIC", str(source), "-o", str(output))
        wrapper = [str(gradle / "gradlew"), "--no-daemon"]
        if args.offline:
            wrapper.append("--offline")
        run(*wrapper, "assembleDebug", cwd=gradle, env=env)
        check_apk(gradle, "debug", {"arm64-v8a", "x86_64"})
        # A property must override the environment and exclude stale libraries
        # from the earlier build, even though both .so files remain on disk.
        run(*wrapper, "assembleRelease", "-Pgpui.abis=x86_64", cwd=gradle, env=env)
        check_apk(gradle, "release", {"x86_64"})


if __name__ == "__main__":
    main()

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


def run_capture(*args, cwd=None, env=None):
    result = subprocess.run(args, cwd=cwd, env=env, check=False, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if result.returncode:
        print(result.stdout, flush=True)
        result.check_returncode()
    return result.stdout


def prepare_minimal_android_library(project):
    """Avoid compiling GPUI here; exercise the real CLI/cargo-ndk/Gradle path."""
    (project / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/app"]\nresolver = "2"\n'
    )
    app = project / "crates/app"
    (app / "Cargo.toml").write_text(
        '[package]\n'
        'name = "template-probe-app"\n'
        'version = "0.1.0"\n'
        'edition = "2021"\n\n'
        '[lib]\n'
        'name = "template_probe_app"\n'
        'crate-type = ["cdylib"]\n'
    )
    (app / "src/lib.rs").write_text(
        '#[no_mangle]\npub extern "C" fn gpui_cache_probe() -> u32 { 1 }\n'
    )


def check_android_build_cache(gpui, project, expected_abis, env):
    prepare_minimal_android_library(project)
    run("cargo", "generate-lockfile", cwd=project, env=env)

    with tempfile.TemporaryDirectory(prefix="gpui-gradle-cache-probe-") as temporary:
        gradle_home = Path(temporary) / "gradle-home"
        gradle_home.mkdir()
        cached_home = Path(env["GRADLE_USER_HOME"]).resolve()
        # Reuse downloaded distributions and dependencies, but exclude global
        # gradle.properties/init scripts from this cache-reuse probe.
        for name in ("caches", "wrapper"):
            cached_entry = cached_home / name
            if cached_entry.exists():
                (gradle_home / name).symlink_to(
                    cached_entry.resolve(), target_is_directory=True
                )
        cache_env = {**env, "GRADLE_USER_HOME": str(gradle_home)}
        for name in (
            "GRADLE_HOME",
            "GRADLE_OPTS",
            "JAVA_OPTS",
            "JAVA_TOOL_OPTIONS",
            "JAVACMD",
            "JDK_JAVA_OPTIONS",
            "_JAVA_OPTIONS",
        ):
            cache_env.pop(name, None)
        for name in list(cache_env):
            if name.upper().startswith("ORG_GRADLE_PROJECT_"):
                cache_env.pop(name)

        first = run_capture(str(gpui), "build", "android", cwd=project, env=cache_env)
        assert "Android cache miss:" in first, first
        second = run_capture(str(gpui), "build", "android", cwd=project, env=cache_env)
        assert "Android BuildKey cache hit:" in second, second

    outputs = list(
        (project / ".gpui/builds/android").glob(
            "*/gradle-build/outputs/apk/debug/*.apk"
        )
    )
    assert len(outputs) == 1, outputs
    check_apk_file(outputs[0], expected_abis)


def check_apk_file(apk, expected_abis):
    with zipfile.ZipFile(apk) as archive:
        actual = {
            name.split("/")[1]
            for name in archive.namelist()
            if name.startswith("lib/") and name.endswith("/libtemplate_probe_app.so")
        }
    assert actual == expected_abis, (apk, actual, expected_abis)
    print(f"Verified cached CLI APK: {apk.name}, ABIs {sorted(actual)}", flush=True)


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
    abis = {"arm64-v8a", "x86_64"}
    with tempfile.TemporaryDirectory(prefix="gpui-android-template-") as scratch:
        env = {**os.environ, "GPUI_ANDROID_ABIS": ",".join(sorted(abis))}
        env["HOME"] = str(Path(scratch) / "home")
        Path(env["HOME"]).mkdir()
        env["ANDROID_USER_HOME"] = str(Path(env["HOME"]) / ".android")
        env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
        env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
        env.setdefault("GRADLE_USER_HOME", str(Path.home() / ".gradle"))
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
        check_android_build_cache(gpui, project, abis, env)


if __name__ == "__main__":
    main()

import importlib.util
import hashlib
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile
import zlib


sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from mobile_evidence import CANARY, Evidence, build_artifact, doctor, validate_doctor, validate_png


def report(target="android", status="pass", phase="cold", device="ci-avd"):
    return {
        "schema_version": 2,
        "target": {"id": target, "explicit": True, "source": "cli"},
        "overall": "pass" if status == "pass" else "fail",
        "checks": [
            {"id": "rustc", "required": True, "status": "pass"},
            {"id": f"{target}.selected_device", "required": True, "status": status,
             "expected": {"build_abis": ["x86_64" if status == "pass" else "arm64-v8a"]},
             "actual": {"id": device, "kind": "emulator", "platform": target, "arch": "x86_64",
                        "state": {"kind": "stopped" if phase == "cold" else "running"}}},
        ],
    }


def png_chunk(kind, payload):
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", zlib.crc32(kind + payload))


def tiny_png():
    return (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
            + png_chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00")) + png_chunk(b"IEND", b""))


class DoctorTests(unittest.TestCase):
    def test_cold_match_and_mismatch(self):
        for expected in ("pass", "fail"):
            validate_doctor(report(status=expected), "android", expected, "cold", "ci-avd", "x86_64")

    def test_live_serial_can_resolve_to_named_avd(self):
        actual = report(phase="live")
        actual["checks"][1]["actual"]["state"]["serial"] = "emulator-5554"
        validate_doctor(actual, "android", "pass", "live", "emulator-5554", "x86_64")

    def test_phase_and_device_and_abi_cannot_be_substituted(self):
        for phase, device, abi in (("live", "ci-avd", "x86_64"), ("cold", "other-avd", "x86_64"),
                                   ("cold", "ci-avd", "arm64-v8a")):
            with self.assertRaises(RuntimeError):
                validate_doctor(report(), "android", "pass", phase, device, abi)

    def test_missing_or_duplicate_selected_check_is_rejected(self):
        for checks in ([], [report()["checks"][0]], report()["checks"] + [report()["checks"][1]]):
            actual = report()
            actual["checks"] = checks
            with self.assertRaises(RuntimeError):
                validate_doctor(actual, "android", "pass", "cold", "ci-avd", "x86_64")

    def test_unrelated_required_failure_does_not_count_as_expected_failure(self):
        actual = report(status="fail")
        actual["checks"][0]["status"] = "fail"
        with self.assertRaises(RuntimeError):
            validate_doctor(actual, "android", "fail", "cold", "ci-avd", "x86_64")

    def test_optional_warning_does_not_fail_match(self):
        actual = report()
        actual["overall"] = "warning"
        actual["checks"].append({"id": "optional", "required": False, "status": "warning"})
        validate_doctor(actual, "android", "pass", "cold", "ci-avd", "x86_64")

    def test_wrong_build_abi_set_is_rejected(self):
        actual = report()
        actual["checks"][1]["expected"]["build_abis"] = ["arm64-v8a"]
        with self.assertRaises(RuntimeError):
            validate_doctor(actual, "android", "pass", "cold", "ci-avd", "x86_64")

    def test_ios_missing_device_is_a_selected_device_only_failure(self):
        actual = report(target="ios", status="fail")
        actual["checks"][1]["actual"] = {"available": False}
        validate_doctor(actual, "ios", "fail", "cold", "some-udid")

    def test_raw_report_is_saved_before_exit_validation(self):
        class Recorder:
            def __init__(self):
                self.saved = {}

            def run(self, label, argv, **kwargs):
                return {"returncode": 1 if label == "match" else 0,
                        "stdout": json.dumps(report(status="fail"))}

            def write(self, name, value):
                self.saved[name] = value

        recorder = Recorder()
        with self.assertRaises(RuntimeError):
            doctor(recorder, Path("/gpui"), "android", "ci-avd", "cold")
        self.assertIn("match-doctor.json", recorder.saved)


class CommandTests(unittest.TestCase):
    def test_nonzero_output_is_persisted_before_raising(self):
        with tempfile.TemporaryDirectory() as root:
            evidence = Evidence(Path(root) / "evidence")
            with self.assertRaises(RuntimeError):
                evidence.run("failure", [sys.executable, "-c", "print('diagnostic'); raise SystemExit(7)"])
            self.assertEqual(evidence.commands[0]["returncode"], 7)
            self.assertIn("diagnostic", (evidence.output / evidence.commands[0]["stdout_path"]).read_text())

    def test_timeout_records_failure(self):
        with tempfile.TemporaryDirectory() as root:
            evidence = Evidence(Path(root) / "evidence")
            result = evidence.run("timeout", [sys.executable, "-c", "import time; time.sleep(20)"], timeout=1, check=False)
            self.assertIsNone(result["returncode"])
            self.assertIn("deadline", result["error"])

    def test_secret_leak_is_redacted_and_fails(self):
        with tempfile.TemporaryDirectory() as root:
            evidence = Evidence(Path(root) / "evidence")
            with self.assertRaises(RuntimeError):
                evidence.run("leak", [sys.executable, "-c", f"print({CANARY!r})"])
            for artifact in evidence.output.iterdir():
                self.assertNotIn(CANARY.encode(), artifact.read_bytes())
            self.assertTrue(evidence.commands[0]["secret_leak"])

    def test_existing_output_is_rejected(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(FileExistsError):
                Evidence(Path(root))


class PngTests(unittest.TestCase):
    def test_dimensions_and_hash(self):
        metadata = validate_png(tiny_png())
        self.assertEqual((metadata["width"], metadata["height"]), (1, 1))
        self.assertEqual(len(metadata["sha256"]), 64)

    def test_invalid_and_truncated_captures_are_rejected(self):
        corrupted = bytearray(tiny_png())
        corrupted[30] ^= 1
        for data in (b"not png", tiny_png()[:-4], tiny_png() + b"extra", bytes(corrupted)):
            with self.assertRaises(RuntimeError):
                validate_png(data)

    def test_valid_crc_does_not_hide_invalid_compressed_pixels(self):
        header = tiny_png()[:33]
        for pixels in (b"not deflate", zlib.compress(b"\x00"), zlib.compress(b"\x05\xff\x00\x00")):
            invalid = header + png_chunk(b"IDAT", pixels) + png_chunk(b"IEND", b"")
            with self.assertRaises(RuntimeError):
                validate_png(invalid)


class RuntimeTests(unittest.TestCase):
    def test_ios_runtime_selection_is_exact_and_available(self):
        path = Path(__file__).resolve().parent.parent / "check-ios-simulator.py"
        spec = importlib.util.spec_from_file_location("ios_evidence", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        catalog = {"runtimes": [{"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-2",
                                  "version": "26.2", "isAvailable": True}]}
        self.assertTrue(module.select_runtime(catalog, "26.2").endswith("iOS-26-2"))
        for invalid in ("18.4", "26.1"):
            with self.assertRaises(RuntimeError):
                module.select_runtime(catalog, invalid)
        catalog["runtimes"][0]["isAvailable"] = False
        with self.assertRaises(RuntimeError):
            module.select_runtime(catalog, "26.2")

    def load_driver(self, filename):
        source = Path(__file__).resolve().parent.parent / filename
        spec = importlib.util.spec_from_file_location(filename, source)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_android_build_only_never_calls_adb(self):
        module = self.load_driver("check-android-runtime.py")
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            gpui = root / "gpui"
            gpui.touch()
            apk = root / "app.apk"
            with zipfile.ZipFile(apk, "w") as archive:
                archive.writestr("lib/x86_64/libmobile_ci_probe_app.so", b"unit-test-only")
            recorder = FakeEvidence(root)
            argv = ["driver", "--gpui", str(gpui), "--output", str(root / "evidence"), "--build-only"]
            with patch.object(module, "Evidence", return_value=recorder), \
                    patch.object(module, "build_artifact", return_value=apk), patch.object(sys, "argv", argv):
                self.assertEqual(module.main(), 0)
            self.assertFalse(any(command[0] == "adb" for _, command in recorder.commands))
            labels = [label for label, _ in recorder.commands]
            self.assertLess(labels.index("lock-app"), labels.index("build-app"))
            self.assertEqual(recorder.summary["status"], "pass")
            self.assertEqual(recorder.summary["build_status"], "pass")

    def test_android_failed_install_still_cleans_up_owned_package(self):
        module = self.load_driver("check-android-runtime.py")
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            gpui = root / "gpui"
            gpui.touch()
            apk = root / "app.apk"
            with zipfile.ZipFile(apk, "w") as archive:
                archive.writestr("lib/x86_64/libmobile_ci_probe_app.so", b"unit-test-only")
            recorder = FakeEvidence(root, responses={"boot-completed": "1", "device-abi": "x86_64",
                                                     "package-before-cleanup": "package:owned"}, failures={"install"})
            argv = ["driver", "--gpui", str(gpui), "--output", str(root / "evidence"), "--device", "emulator-5554"]
            with patch.object(module, "Evidence", return_value=recorder), \
                    patch.object(module, "build_artifact", return_value=apk), patch.object(sys, "argv", argv):
                self.assertEqual(module.main(), 1)
            labels = [label for label, _ in recorder.commands]
            self.assertIn("uninstall", labels)
            self.assertEqual(recorder.summary["cleanup"]["status"], "pass")
            self.assertEqual(recorder.summary["status"], "fail")
            uninstall = next(command for label, command in recorder.commands if label == "uninstall")
            self.assertTrue(uninstall[-1].startswith("com.example.gpuimobileci.r"))

    def test_ios_build_is_independent_of_boot_and_always_deletes_owned_device(self):
        module = self.load_driver("check-ios-simulator.py")
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            gpui = root / "gpui"
            gpui.touch()
            catalog = {"runtimes": [{"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-2",
                                      "version": "26.2", "isAvailable": True}]}
            recorder = FakeEvidence(root, responses={"runtimes": json.dumps(catalog),
                                                     "create": "11111111-1111-1111-1111-111111111111",
                                                     "devices-after-cleanup": '{"devices": {}}'})
            argv = ["driver", "--gpui", str(gpui), "--output", str(root / "evidence"), "--mode", "build"]
            with patch.object(module, "Evidence", return_value=recorder), \
                    patch.object(module, "build", return_value=root / "app.app"), patch.object(sys, "argv", argv):
                self.assertEqual(module.main(), 0)
            labels = [label for label, _ in recorder.commands]
            self.assertNotIn("boot", labels)
            self.assertIn("delete", labels)
            self.assertEqual(recorder.summary["cleanup"]["status"], "pass")

    def test_ios_build_prepares_lockfile_before_locked_cli_build(self):
        module = self.load_driver("check-ios-simulator.py")
        with tempfile.TemporaryDirectory() as root:
            recorder = FakeEvidence(Path(root))
            with patch.object(module, "build_artifact", return_value=Path(root) / "app.app"):
                module.build(recorder, Path("/gpui"), Path(root) / "project", "owned-udid")
            labels = [label for label, _ in recorder.commands]
            self.assertLess(labels.index("lock-app"), labels.index("build-app"))
            command = next(command for label, command in recorder.commands if label == "build-app")
            self.assertEqual(command[-2:], ["--device", "owned-udid"])

    def test_android_manifest_roots_are_directories_not_apk_names(self):
        with tempfile.TemporaryDirectory() as root:
            project = Path(root) / "project"
            key_root = project / ".gpui/builds/android" / ("0" * 64)
            relative = "gradle-build/outputs/apk/debug/app-debug.apk"
            apk = key_root / relative
            apk.parent.mkdir(parents=True)
            apk.write_bytes(b"unit-test-only")
            manifest = {"schema_version": 1, "platform": "android",
                        "roots": ["native-staging/android/jni-libs/x86_64", "gradle-build/outputs/apk/debug"],
                        "files": [{"path": relative, "sha256": hashlib.sha256(apk.read_bytes()).hexdigest()}]}
            (key_root / "artifact-manifest.json").write_text(json.dumps(manifest))
            evidence = Evidence(Path(root) / "evidence")
            self.assertEqual(build_artifact(evidence, project, "android", ".apk"), apk.resolve())
            apk.write_bytes(b"corrupted")
            with self.assertRaises(RuntimeError):
                build_artifact(evidence, project, "android", ".apk")

    def test_android_cache_bypass_does_not_hide_a_real_build_output(self):
        with tempfile.TemporaryDirectory() as root:
            project = Path(root) / "project"
            apk = project / ".gpui/builds/android" / ("0" * 64) / "gradle-build/outputs/apk/debug/app-debug.apk"
            apk.parent.mkdir(parents=True)
            apk.write_bytes(b"unit-test-only")
            evidence = Evidence(Path(root) / "evidence")
            self.assertEqual(build_artifact(evidence, project, "android", ".apk"), apk.resolve())
            selection = json.loads((evidence.output / "artifact-selection.json").read_text())
            self.assertFalse(selection["cache_manifest_available"])


class FakeEvidence:
    def __init__(self, output, responses=None, failures=None):
        self.output = output
        self.responses = responses or {}
        self.failures = failures or set()
        self.commands = []
        self.summary = {}

    def context(self):
        pass

    def run(self, label, argv, **kwargs):
        self.commands.append((label, argv))
        if label in self.failures:
            raise RuntimeError(f"injected {label} failure")
        if label == "lock-app":
            kwargs["cwd"].mkdir(parents=True, exist_ok=True)
            (kwargs["cwd"] / "Cargo.lock").write_text("unit-test-only\n")
        return {"returncode": 0, "stdout": self.responses.get(label, ""), "stderr": ""}

    def write(self, name, value):
        pass

    def finish(self, status, **details):
        self.summary = {"status": status, **details}


if __name__ == "__main__":
    unittest.main()

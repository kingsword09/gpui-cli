import importlib.util
import hashlib
import json
import copy
from pathlib import Path
import re
import struct
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
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


def ios_live_report(*, missing=False, timeout=False):
    actual = report(target="ios", status="fail" if missing else "pass", phase="live", device="ci-sim")
    if missing:
        actual["checks"][1]["actual"] = {"available": False}
    if timeout:
        actual["overall"] = "fail" if missing else "unknown"
        actual["checks"][0].update(status="unknown", reason="probe timed out")
    return actual


def ios_rust_target_timeout_report(*, missing=False, kind="simulator"):
    actual = ios_live_report(missing=missing, timeout=True)
    target = "aarch64-apple-ios-sim" if kind == "simulator" else "aarch64-apple-ios"
    actual["checks"].append({
        "id": f"ios.rust_target.{kind}", "required": True, "status": "unknown",
        "reason": "Rust target probe timed out", "expected": {"target": target},
        # A timeout reports installed=false even though it cannot establish absence.
        "actual": {"target": target, "installed": False},
    })
    return actual


class DoctorRecorder:
    def __init__(self, responses):
        self.responses = responses
        self.saved = {}
        self.commands = []

    def run(self, label, argv, **kwargs):
        self.commands.append(label)
        if label == "generate":
            return {"returncode": 0, "stdout": ""}
        actual = self.responses.get(label, ios_live_report(missing=label.startswith("missing-device")))
        return {"returncode": int(actual["overall"] in ("fail", "unknown")), "stdout": json.dumps(actual)}

    def write(self, name, value):
        self.saved[name] = copy.deepcopy(value)


class DoctorTests(unittest.TestCase):
    def test_ios_live_rust_target_timeouts_recover_for_both_selector_cases(self):
        for missing in (False, True):
            for kind in ("simulator", "device"):
                failed = ios_rust_target_timeout_report(missing=missing, kind=kind)
                recovered = copy.deepcopy(failed)
                recovered["overall"] = "fail" if missing else "pass"
                for check in recovered["checks"]:
                    if check["status"] == "unknown":
                        check.update(status="pass", reason="probe completed")
                recovered["checks"][-1]["actual"]["installed"] = True
                label = "missing-device" if missing else "match"
                recorder = DoctorRecorder({label: failed, f"{label}-retry-1": recovered})
                with self.subTest(missing=missing, kind=kind), patch("mobile_evidence.time.sleep") as sleeper:
                    result = doctor(recorder, Path("/gpui"), "ios", "ci-sim", "live", timeout_retries=2)
                    sleeper.assert_called_once_with(15)
                    self.assertEqual(result["timeout_retries_used"], 1)
                    self.assertEqual(result["reports"][label], f"{label}-retry-1-doctor.json")
                    self.assertEqual(recorder.saved[f"{label}-doctor.json"], failed)
                    self.assertEqual(recorder.saved[f"{label}-retry-1-doctor.json"], recovered)

    def test_ios_live_persistent_rust_target_timeouts_keep_the_retry_limit(self):
        failed = ios_rust_target_timeout_report()
        recorder = DoctorRecorder({label: failed for label in ("match", "match-retry-1", "match-retry-2")})
        with patch("mobile_evidence.time.sleep") as sleeper, self.assertRaises(RuntimeError):
            doctor(recorder, Path("/gpui"), "ios", "ci-sim", "live", timeout_retries=2)
        self.assertEqual(sleeper.call_count, 2)
        self.assertEqual(recorder.commands, ["generate", "match", "match-retry-1", "match-retry-2"])
        self.assertEqual(recorder.saved["match-retry-2-doctor.json"], failed)
        self.assertNotIn("doctor-result.json", recorder.saved)

    def test_ios_live_rust_target_non_timeout_failures_are_not_retried(self):
        failures = (("fail", "Rust target aarch64-apple-ios-sim is not installed"),
                    ("unavailable", "rustup was not found"),
                    ("fail", "rustup exited unsuccessfully (code Some(1))"),
                    ("unknown", "could not start rustup: permission denied"))
        for missing in (False, True):
            for status, reason in failures:
                failed = ios_rust_target_timeout_report(missing=missing)
                failed["checks"][-1].update(status=status, reason=reason)
                if status in ("fail", "unavailable"):
                    failed["overall"] = "fail"
                label = "missing-device" if missing else "match"
                recorder = DoctorRecorder({label: failed})
                with self.subTest(missing=missing, reason=reason), patch("mobile_evidence.time.sleep") as sleeper, \
                        self.assertRaises(RuntimeError):
                    doctor(recorder, Path("/gpui"), "ios", "ci-sim", "live", timeout_retries=2)
                sleeper.assert_not_called()
                self.assertEqual(recorder.commands[-1], label)
                self.assertEqual(recorder.saved[f"{label}-doctor.json"], failed)
                self.assertNotIn("doctor-result.json", recorder.saved)

    def test_ios_live_timeout_retries_preserve_each_failed_report(self):
        recorder = DoctorRecorder({"match": ios_live_report(timeout=True)})
        with patch("mobile_evidence.time.sleep") as sleeper:
            result = doctor(recorder, Path("/gpui"), "ios", "ci-sim", "live", timeout_retries=2)
        sleeper.assert_called_once_with(15)
        self.assertEqual(result["timeout_retries_used"], 1)
        self.assertEqual(result["reports"]["match"], "match-retry-1-doctor.json")
        self.assertEqual(recorder.saved["match-doctor.json"]["overall"], "unknown")
        self.assertEqual(recorder.saved["match-retry-1-doctor.json"]["overall"], "pass")
        self.assertEqual(recorder.saved["doctor-retries.json"][0]["failed_report"], "match-doctor.json")

    def test_ios_live_persistent_timeouts_fail_after_bounded_retries(self):
        recorder = DoctorRecorder({label: ios_live_report(timeout=True)
                                  for label in ("match", "match-retry-1", "match-retry-2")})
        with patch("mobile_evidence.time.sleep") as sleeper, self.assertRaises(RuntimeError):
            doctor(recorder, Path("/gpui"), "ios", "ci-sim", "live", timeout_retries=2)
        self.assertEqual(sleeper.call_count, 2)
        self.assertEqual(recorder.commands, ["generate", "match", "match-retry-1", "match-retry-2"])
        self.assertIn("match-retry-2-doctor.json", recorder.saved)
        self.assertNotIn("doctor-result.json", recorder.saved)

    def test_ios_live_non_timeout_failures_and_wrong_identity_are_not_retried(self):
        invalid_reports = []
        for status in ("fail", "unavailable", "unknown"):
            actual = ios_live_report(timeout=True)
            actual["checks"][0].update(status=status, reason="missing or invalid tool")
            invalid_reports.append(actual)
        for field, value in (("id", "other-device"), ("kind", "physical"), ("platform", "android")):
            actual = ios_live_report(timeout=True)
            actual["checks"][1]["actual"][field] = value
            invalid_reports.append(actual)
        actual = ios_live_report(timeout=True)
        actual["checks"][1]["actual"]["state"]["kind"] = "stopped"
        invalid_reports.append(actual)
        actual = ios_live_report(timeout=True)
        actual["target"]["explicit"] = False
        invalid_reports.append(actual)
        actual = ios_live_report(timeout=True)
        actual["checks"][1]["status"] = "fail"
        invalid_reports.append(actual)
        for actual in invalid_reports:
            recorder = DoctorRecorder({"match": actual})
            with self.subTest(actual=actual), patch("mobile_evidence.time.sleep") as sleeper, \
                    self.assertRaises(RuntimeError):
                doctor(recorder, Path("/gpui"), "ios", "ci-sim", "live", timeout_retries=2)
            sleeper.assert_not_called()
            self.assertEqual(recorder.commands, ["generate", "match"])

    def test_ios_live_negative_timeout_retries_do_not_mask_unrelated_failure(self):
        actual = ios_live_report(missing=True, timeout=True)
        actual["checks"][0]["reason"] = "total probe deadline exceeded before this check ran"
        recorder = DoctorRecorder({"missing-device": actual})
        with patch("mobile_evidence.time.sleep") as sleeper:
            result = doctor(recorder, Path("/gpui"), "ios", "ci-sim", "live", timeout_retries=2)
        sleeper.assert_called_once_with(15)
        self.assertEqual(result["reports"]["missing-device"], "missing-device-retry-1-doctor.json")
        self.assertEqual(recorder.saved["missing-device-doctor.json"]["checks"][0]["status"], "unknown")
        self.assertEqual(recorder.saved["missing-device-retry-1-doctor.json"]["checks"][0]["status"], "pass")

    def test_doctor_timeout_retries_are_opt_in_and_live_ios_only(self):
        recorder = DoctorRecorder({"match": ios_live_report(timeout=True)})
        with patch("mobile_evidence.time.sleep") as sleeper, self.assertRaises(RuntimeError):
            doctor(recorder, Path("/gpui"), "ios", "ci-sim", "live")
        sleeper.assert_not_called()
        for target, phase, retries in (("android", "live", 2), ("ios", "cold", 2), ("ios", "live", 3)):
            with self.assertRaises(ValueError):
                doctor(recorder, Path("/gpui"), target, "ci-sim", phase, timeout_retries=retries)

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
    def test_android_inventory_roots_are_recorded(self):
        roots = {"ANDROID_AVD_HOME": "/isolated/avds", "ANDROID_USER_HOME": "/isolated/user"}
        with tempfile.TemporaryDirectory() as root, patch.dict("os.environ", roots):
            evidence = Evidence(Path(root) / "evidence")
            environment = json.loads((evidence.output / "environment.json").read_text())
            self.assertEqual(environment["android_avd_home"], roots["ANDROID_AVD_HOME"])
            self.assertEqual(environment["android_user_home"], roots["ANDROID_USER_HOME"])

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


class WorkflowTests(unittest.TestCase):
    def job(self, name):
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml").read_text()
        matched = re.search(rf"^  {re.escape(name)}:\n(.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)",
                            workflow, re.MULTILINE | re.DOTALL)
        self.assertIsNotNone(matched)
        return matched[1]

    def test_ios_jobs_prepare_all_required_doctor_tools(self):
        job = self.job("ios-simulator")
        targets = re.search(r"^\s+targets: ([^\n]+)$", job, re.MULTILINE)
        self.assertIsNotNone(targets)
        self.assertTrue({"aarch64-apple-ios", "aarch64-apple-ios-sim"}.issubset(targets[1].split(",")))
        self.assertLess(job.index("brew install xcodegen"), job.index("scripts/check-ios-simulator.py"))

    def test_emulator_host_libraries_precede_preflight(self):
        job = self.job("android-emulator")
        self.assertLess(job.index("apt-get install -y --no-install-recommends libpulse0"),
                        job.index('"$ANDROID_HOME/emulator/emulator" -accel-check'))
        self.assertIn("mobile-preflight/shared-libraries.txt", job)

    def test_cold_avd_has_an_explicit_shared_metadata_root(self):
        job = self.job("doctor-android-cold")
        self.assertIn('export ANDROID_AVD_HOME="$RUNNER_TEMP/gpui-cold-avd"', job)
        self.assertIn('echo "ANDROID_AVD_HOME=$ANDROID_AVD_HOME" >> "$GITHUB_ENV"', job)
        self.assertIn('--path "$ANDROID_AVD_HOME/gpui-ci-x86.avd"', job)
        self.assertIn('cp "$ANDROID_AVD_HOME/gpui-ci-x86.avd/config.ini" mobile-preflight/avd-config.ini', job)
        self.assertIn("doctor-android-cold-evidence/\n            mobile-preflight/", job)


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

    def test_ios_smoke_enables_only_bounded_live_doctor_retries(self):
        module = self.load_driver("check-ios-simulator.py")
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            gpui = root / "gpui"
            gpui.touch()
            device = "11111111-1111-1111-1111-111111111111"
            catalog = {"runtimes": [{"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-2",
                                      "version": "26.2", "isAvailable": True}]}
            recorder = FakeEvidence(root, responses={"runtimes": json.dumps(catalog), "create": device,
                                                     "devices-after-cleanup": '{"devices": {}}'})
            argv = ["driver", "--gpui", str(gpui), "--output", str(root / "evidence"), "--mode", "smoke"]
            with patch.object(module, "Evidence", return_value=recorder), \
                    patch.object(module, "build", return_value=root / "app.app"), \
                    patch.object(module, "doctor", return_value={"timeout_retries_used": 1}) as live_doctor, \
                    patch.object(module, "disable_push_service") as push_service, \
                    patch.object(module, "smoke", return_value={}), patch.object(sys, "argv", argv):
                self.assertEqual(module.main(), 0)
            push_service.assert_not_called()
            live_doctor.assert_called_once_with(recorder, gpui.resolve(), "ios", device, "live", timeout_retries=2)
            self.assertEqual(recorder.summary["live_doctor"]["timeout_retries_used"], 1)
            self.assertEqual(recorder.summary["cleanup"]["status"], "pass")

    def test_ios_push_service_is_disabled_only_inside_the_selected_simulator(self):
        module = self.load_driver("check-ios-simulator.py")
        recorder = self.push_service_recorder()
        result = module.disable_push_service(recorder, "owned-udid")
        for call in recorder.run.call_args_list:
            self.assertEqual(call.args[1][:5], ["xcrun", "simctl", "spawn", "owned-udid", "launchctl"])
        self.assertEqual(result, {"device": "owned-udid", "service": "com.apple.apsd", "domain": "user/502",
                                  "loaded": False, "lifetime": "current_boot", "push_notifications": "excluded_from_smoke"})
        self.assertEqual([call.args[1][5] for call in recorder.run.call_args_list], ["print", "bootout", "print"])
        recorder.write.assert_called_once_with("simulator-services.json", result)

    def test_ios_push_service_requires_confirmed_removal(self):
        module = self.load_driver("check-ios-simulator.py")
        invalid = (
            ("push-service-before", {"stdout": "system/com.apple.apsd = {"}),
            ("push-service-before", {"stdout": "user/502/other-service = {"}),
            ("push-service-after", {"returncode": 0, "stdout": "state = running", "stderr": ""}),
            ("push-service-after", {"returncode": 113, "error": "command exceeded 60s deadline"}),
            ("push-service-after", {"returncode": 1, "stderr": "permission denied"}),
            ("push-service-after", {"returncode": 113, "stderr": 'Could not find service "other-service"'}),
        )
        for label, response in invalid:
            recorder = self.push_service_recorder({label: response})
            with self.subTest(label=label, response=response), self.assertRaises(RuntimeError):
                module.disable_push_service(recorder, "owned-udid")
            recorder.write.assert_not_called()

    def test_ios_push_service_bootout_failure_is_not_reported_as_removal(self):
        module = self.load_driver("check-ios-simulator.py")
        recorder = self.push_service_recorder()
        respond = recorder.run.side_effect
        def fail_bootout(label, argv, **kwargs):
            if label == "push-service-stop":
                raise RuntimeError("bootout failed")
            return respond(label, argv, **kwargs)
        recorder.run.side_effect = fail_bootout
        with self.assertRaisesRegex(RuntimeError, "bootout failed"):
            module.disable_push_service(recorder, "owned-udid")
        recorder.write.assert_not_called()
        self.assertEqual([call.args[0] for call in recorder.run.call_args_list],
                         ["push-service-before", "push-service-stop"])

    @staticmethod
    def push_service_recorder(overrides=None):
        responses = {
            "push-service-before": {"stdout": "user/502/com.apple.apsd = {\n\tstate = running\n}"},
            "push-service-after": {"returncode": 113, "stderr": 'Bad request.\nCould not find service "com.apple.apsd" in domain for user/502\n'},
        }
        for label, response in (overrides or {}).items():
            responses[label] = {**responses.get(label, {}), **response}
        recorder = Mock()
        recorder.run.side_effect = lambda label, argv, **kwargs: {
            "returncode": 0, "stdout": "", "stderr": "", "error": "", **responses.get(label, {})}
        return recorder

    def test_ios_push_service_policy_precedes_doctor_and_failure_still_cleans_up(self):
        module = self.load_driver("check-ios-simulator.py")
        for failed in (False, True):
            with self.subTest(failed=failed), tempfile.TemporaryDirectory() as root:
                root = Path(root)
                gpui = root / "gpui"
                gpui.touch()
                device = "11111111-1111-1111-1111-111111111111"
                catalog = {"runtimes": [{"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-2",
                                          "version": "26.2", "isAvailable": True}]}
                recorder = FakeEvidence(root, responses={"runtimes": json.dumps(catalog), "create": device,
                                                         "devices-after-cleanup": '{"devices": {}}'})
                policy = {"push_notifications": "excluded_from_smoke"}
                calls = []
                def configure(evidence, udid):
                    self.assertEqual(udid, device)
                    self.assertIn("boot", [label for label, _ in evidence.commands])
                    calls.append("service")
                    if failed:
                        raise RuntimeError("APNs service removal was not confirmed")
                    return policy
                argv = ["driver", "--gpui", str(gpui), "--output", str(root / "evidence"),
                        "--mode", "smoke", "--disable-push-service"]
                with patch.object(module, "Evidence", return_value=recorder), \
                        patch.object(module, "build", return_value=root / "app.app"), \
                        patch.object(module, "disable_push_service", side_effect=configure), \
                        patch.object(module, "doctor", side_effect=lambda *a, **k: calls.append("doctor")), \
                        patch.object(module, "smoke", return_value={}) as smoke, patch.object(sys, "argv", argv):
                    self.assertEqual(module.main(), int(failed))
                self.assertEqual(calls, ["service"] if failed else ["service", "doctor"])
                self.assertEqual(recorder.summary["cleanup"]["status"], "pass")
                if failed:
                    smoke.assert_not_called()
                    self.assertEqual(recorder.summary["status"], "fail")
                else:
                    self.assertEqual(recorder.summary["simulator_services"], policy)

    def test_ios_push_service_option_rejects_non_smoke_modes(self):
        module = self.load_driver("check-ios-simulator.py")
        for mode in ("doctor", "build"):
            argv = ["driver", "--gpui", "/gpui", "--output", "/unused", "--mode", mode, "--disable-push-service"]
            with self.subTest(mode=mode), patch.object(module, "Evidence") as recorder, \
                    patch.object(sys, "argv", argv), self.assertRaises(SystemExit) as error:
                module.main()
            self.assertEqual(error.exception.code, 2)
            recorder.assert_not_called()

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

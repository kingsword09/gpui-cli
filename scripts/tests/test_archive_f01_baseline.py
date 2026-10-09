import importlib.util
import io
import json
from pathlib import Path
import re
import tarfile
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("archive_f01_baseline", Path(__file__).resolve().parents[1] / "archive-f01-baseline.py")
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class ArchiveBaselineTests(unittest.TestCase):
    def make_materials(self, output):
        runtime = b"const PROTO_VERSION: u32 = 1;\n"
        with tarfile.open(output / "source.tar", "w") as archive:
            for name, content in (("templates/app/src/live.rs", runtime), ("Cargo.lock", b"locked")):
                member = tarfile.TarInfo(name)
                member.size = len(content)
                archive.addfile(member, io.BytesIO(content))
        files = {
            "legacy-Cargo.lock": b"locked", "bin/gpui": b"binary",
            "project/crates/app/src/live.rs": runtime,
            "project/crates/desktop/Cargo.toml": b"desktop",
            "project/mobile/ios/project.yml": b"ios",
            "project/mobile/android/gradle/app/build.gradle.kts": b"android",
        }
        for name, content in files.items():
            path = output / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        manifest = {
            "schema_version": 1, "status": "pass", "baseline_revision": driver.BASELINE_REVISION,
            "producer_revision": "producer", "producer_dirty": False,
            "driver_sha256": driver.digest(Path(driver.__file__)), "files": driver.material_hashes(output),
        }
        (output / "manifest.json").write_text(json.dumps(manifest))

    def test_revision_is_the_original_immutable_baseline(self):
        self.assertEqual(driver.BASELINE_REVISION, "6d091b661d7ef82115e88cf142c7ff252b593864")

    def test_inventory_includes_hidden_scaffold_files_and_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            (output / "project").mkdir()
            (output / "project/.gitignore").write_text("target/\n")
            (output / "bin").mkdir()
            (output / "bin/gpui").write_bytes(b"binary")
            (output / "manifest.json").write_text("metadata")
            self.assertEqual(set(driver.material_hashes(output)), {"project/.gitignore", "bin/gpui"})

    def test_ci_upload_preserves_hidden_scaffold_files(self):
        workflow = (driver.ROOT / ".github/workflows/ci.yml").read_text()
        job = re.search(r"^  baseline-driver:\n(.*?)(?=^  [a-zA-Z0-9_-]+:|\Z)", workflow, re.MULTILINE | re.DOTALL)
        self.assertIsNotNone(job)
        self.assertRegex(job[1], r"(?m)^          include-hidden-files: true$")

    def test_inventory_rejects_symlinked_materials(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            (output / "link").symlink_to(output / "missing")
            with self.assertRaisesRegex(ValueError, "symlinks"):
                driver.material_hashes(output)

    def test_output_directory_cannot_reuse_stale_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            (output / "sentinel").write_text("old evidence")
            with self.assertRaises(FileExistsError):
                driver.archive_baseline(output, output / "target")
            self.assertEqual((output / "sentinel").read_text(), "old evidence")

    def test_inventory_changes_when_a_scaffold_file_is_modified(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            path = output / "runtime.rs"
            path.write_text("v1")
            original = driver.material_hashes(output)
            path.write_text("modified")
            self.assertNotEqual(original, driver.material_hashes(output))

    def test_complete_archive_verifies_without_claiming_runtime_acceptance(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            self.make_materials(output)
            with patch.object(driver.subprocess, "check_output", return_value=(output / "source.tar").read_bytes()):
                result = driver.verify(output, "producer", False)
            self.assertEqual(result["status"], "pass")
            self.assertEqual(result["runtime_protocol"], 1)
            self.assertEqual(result["online_compatibility"], "not_run")
            self.assertEqual(result["application_build_or_launch"], "not_run")

    def test_verifier_rejects_mutated_materials(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            self.make_materials(output)
            (output / "bin/gpui").write_bytes(b"different binary")
            with self.assertRaisesRegex(ValueError, "inventory or hashes"):
                driver.verify(output, "producer", False)

    def test_verifier_rejects_wrong_source_even_with_self_consistent_hashes(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            self.make_materials(output)
            with patch.object(driver.subprocess, "check_output", return_value=b"different source"):
                with self.assertRaisesRegex(ValueError, "immutable Git revision"):
                    driver.verify(output, "producer", False)

    def test_verifier_rejects_wrong_checkout_identity(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            self.make_materials(output)
            for producer, dirty in (("other-producer", False), ("producer", True)):
                with self.subTest(producer=producer, dirty=dirty), self.assertRaisesRegex(ValueError, "producer identity"):
                    driver.verify(output, producer, dirty)

    def test_failed_command_keeps_logs_and_failed_manifest(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "evidence"
            result = driver.subprocess.CompletedProcess(["git"], 1, b"partial output", b"controlled failure")
            with patch.object(driver.subprocess, "run", return_value=result):
                with self.assertRaisesRegex(RuntimeError, "producer-revision failed"):
                    driver.archive_baseline(output, Path(temporary) / "target")
            manifest = json.loads((output / "manifest.json").read_text())
            commands = json.loads((output / "commands.json").read_text())
            self.assertEqual(manifest["status"], "fail")
            self.assertEqual(commands[0]["exit_code"], 1)
            self.assertEqual((output / "producer-revision.stdout").read_bytes(), b"partial output")
            self.assertEqual((output / "producer-revision.stderr").read_bytes(), b"controlled failure")

    def test_timeout_keeps_partial_logs_and_explicit_deadline(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "evidence"
            error = driver.subprocess.TimeoutExpired(["git"], 600, output=b"partial", stderr=b"timeout")
            with patch.object(driver.subprocess, "run", side_effect=error):
                with self.assertRaises(driver.subprocess.TimeoutExpired):
                    driver.archive_baseline(output, Path(temporary) / "target")
            self.assertEqual(json.loads((output / "manifest.json").read_text())["status"], "fail")
            self.assertEqual(json.loads((output / "commands.json").read_text())[0]["timeout_seconds"], 600)
            self.assertEqual((output / "producer-revision.stdout").read_bytes(), b"partial")
            self.assertEqual((output / "producer-revision.stderr").read_bytes(), b"timeout")


if __name__ == "__main__":
    unittest.main()

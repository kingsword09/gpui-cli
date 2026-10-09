import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]


def load_script(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), ROOT / "scripts" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


driver = load_script("live-baseline")
verifier = load_script("verify-live-baseline")


def setup_spans():
    spans = []
    for case in sorted(verifier.EXPECTED_CASES):
        for ordinal, status in enumerate(("superseded", "ok"), start=1):
            spans.append({
                "case": case, "phase": "setup", "sample_id": f"{case}-setup",
                "name": "build", "build_id": f"b{ordinal}", "span_id": f"sp{ordinal}",
                "started_at_ns": ordinal * 1_000_000, "duration_ns": ordinal * 2_000_000,
                "status": status,
            })
    return spans


class LiveBaselineTests(unittest.TestCase):
    def test_startup_classification_uses_first_build_and_retains_superseded(self):
        spans = setup_spans()
        builds = driver.summarize_startup_builds(list(reversed(spans)))
        self.assertEqual(len(builds), 6)
        for build in builds:
            if build["build_id"] == "b1":
                self.assertEqual(build["cache_kind"], "startup_cold")
                self.assertEqual(build["outcome"], "superseded")
                self.assertEqual(build["build_duration_ms"], 2)
            else:
                self.assertEqual(build["cache_kind"], "incremental_warm")
        self.assertEqual(verifier.verify_startup_builds({"startup_builds": builds}, spans), 6)

    def test_startup_report_rejects_relabelled_or_dropped_first_build(self):
        spans = setup_spans()
        builds = driver.summarize_startup_builds(spans)
        changed = copy.deepcopy(builds)
        changed[0]["cache_kind"] = "incremental_warm"
        for invalid in (changed, builds[1:]):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "classification"):
                verifier.verify_startup_builds({"startup_builds": invalid}, spans)

    def test_startup_report_requires_each_fixture_and_real_timing(self):
        spans = setup_spans()
        builds = driver.summarize_startup_builds(spans)
        for invalid in (spans[2:], [dict(span, started_at_ns=None) for span in spans]):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "timed startup"):
                verifier.verify_startup_builds({"startup_builds": builds}, invalid)

    def test_startup_groups_use_build_duration_not_driver_elapsed(self):
        builds = driver.summarize_startup_builds(setup_spans())
        groups = driver.summarize_groups(builds, "cache_kind", "build_duration_ms")
        self.assertEqual(groups["startup_cold"]["samples"], 3)
        self.assertEqual(groups["startup_cold"]["build_duration_ms"]["p95"], 2)
        self.assertNotIn("driver_elapsed_ms", groups["startup_cold"])

    def test_first_warmup_stays_incremental_after_startup(self):
        sample = {"cache_kind": "incremental_warm"}
        args = type("Args", (), {"timeout": 1, "offline": True, "warmup": 1, "measurements": 0})()
        with tempfile.TemporaryDirectory() as temporary, patch.object(driver, "LiveSession") as session_type:
            session_type.return_value.spans.return_value = []
            with patch.object(driver, "run_sample", return_value=(sample, [])):
                samples, _ = driver.run_case(Path("/gpui"), Path(temporary), "counter", args, [])
        self.assertEqual(samples[0]["cache_kind"], "incremental_warm")

    def test_verifier_rejects_cold_mutation_sample(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            source = {
                "schema_version": 1, "revision": "test", "dirty": False,
                "driver_sha256": hashlib.sha256((ROOT / "scripts/live-baseline.py").read_bytes()).hexdigest(),
                "verifier_sha256": hashlib.sha256((ROOT / "scripts/verify-live-baseline.py").read_bytes()).hexdigest(),
                "workflow_sha256": hashlib.sha256((ROOT / ".github/workflows/ci.yml").read_bytes()).hexdigest(),
            }
            summary = {
                "driver": "scripts/live-baseline.py", "interpretation": {"performance_claim": False},
                "sample_policy": {
                    "failure_samples_retained": True, "superseded_samples_retained": True,
                    "warmup_runs": 10, "measurement_runs": 30,
                },
                "samples": [{"case": "counter", "phase": "warmup", "cache_kind": "startup_cold"}],
            }
            for name, value in (
                ("source", source), ("environment", {"repository": {"commit": "test", "dirty": False}}),
                ("summary", summary),
            ):
                (output / f"{name}.json").write_text(json.dumps(value))
            with self.assertRaisesRegex(ValueError, "must be incremental_warm"):
                verifier.verify(output, "test", False)


if __name__ == "__main__":
    unittest.main()

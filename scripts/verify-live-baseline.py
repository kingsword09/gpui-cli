#!/usr/bin/env python3
"""Verify a complete F01 baseline output directory before CI publishes it."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import sys
from typing import Any


EXPECTED_CASES = {"counter", "login-invalid", "list-scroll"}
SOURCE_MANIFEST_VERSION = 1


def read_json(path: Path) -> Any:
    return json.loads(path.read_text())


def read_ndjson(path: Path) -> list[dict[str, Any]]:
    records = []
    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        value = json.loads(line)
        if not isinstance(value, dict):
            raise ValueError(f"{path.name}:{line_number} is not a JSON object")
        records.append(value)
    return records


def verify_startup_builds(summary: dict[str, Any], spans: list[dict[str, Any]]) -> int:
    startup_builds = summary.get("startup_builds")
    if not isinstance(startup_builds, list):
        raise ValueError("startup build evidence is missing")
    expected = []
    for case in sorted(EXPECTED_CASES):
        records = [
            span for span in spans
            if span.get("case") == case and span.get("phase") == "setup"
            and span.get("name") == "build"
        ]
        if not records or any(not isinstance(span.get("started_at_ns"), int) for span in records):
            raise ValueError(f"{case} is missing timed startup build spans")
        for ordinal, span in enumerate(sorted(records, key=lambda record: record["started_at_ns"])):
            duration = span.get("duration_ns")
            if not isinstance(duration, int) or duration < 0:
                raise ValueError("startup build duration is invalid")
            expected.append({
                "case": case,
                "build_id": span.get("build_id"),
                "span_id": span.get("span_id"),
                "cache_kind": "startup_cold" if ordinal == 0 else "incremental_warm",
                "outcome": span.get("status"),
                "build_duration_ms": duration / 1_000_000,
            })
    if startup_builds != expected:
        raise ValueError("startup build classification differs from the raw setup spans")
    return len(expected)


def verify(output: Path, expected_commit: str, expected_dirty: bool) -> dict[str, Any]:
    source = read_json(output / "source.json")
    environment = read_json(output / "environment.json")
    summary = read_json(output / "summary.json")

    if source.get("schema_version") != SOURCE_MANIFEST_VERSION:
        raise ValueError("unsupported source manifest schema")
    if source.get("revision") != expected_commit:
        raise ValueError("source revision does not match the workflow commit")
    if source.get("dirty") is not expected_dirty:
        raise ValueError("source dirty state does not match the expected value")
    tracked_hashes = {
        "driver_sha256": Path("scripts/live-baseline.py"),
        "verifier_sha256": Path(__file__),
        "workflow_sha256": Path(".github/workflows/ci.yml"),
    }
    for field, path in tracked_hashes.items():
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if source.get(field) != actual:
            raise ValueError(f"{field} does not match the checked out source")

    repository = environment.get("repository", {})
    if (
        repository.get("commit") != expected_commit
        or repository.get("dirty") is not expected_dirty
    ):
        raise ValueError("environment repository identity differs from the source manifest")
    if summary.get("driver") != "scripts/live-baseline.py":
        raise ValueError("unexpected baseline driver")
    if summary.get("sample_policy") != {
        "failure_samples_retained": True,
        "measurement_runs": 30,
        "superseded_samples_retained": True,
        "warmup_runs": 10,
    }:
        raise ValueError("baseline sample policy differs from the required 10+30 runs")
    if summary.get("interpretation", {}).get("performance_claim") is not False:
        raise ValueError("baseline report must not claim cross-machine performance")

    samples = summary.get("samples")
    if not isinstance(samples, list):
        raise ValueError("summary samples are missing")
    per_case: dict[str, dict[str, int]] = {}
    failure_samples = 0
    sample_ids: set[str] = set()
    expected_outcome = {
        "counter": "succeeded",
        "login-invalid": "failed",
        "list-scroll": "succeeded",
    }
    for sample in samples:
        if not isinstance(sample, dict):
            raise ValueError("baseline sample is not a JSON object")
        case = sample.get("case")
        phase = sample.get("phase")
        if case not in EXPECTED_CASES or phase not in {"warmup", "measure"}:
            raise ValueError("baseline contains an unknown case or phase")
        if sample.get("cache_kind") != "incremental_warm":
            raise ValueError("mutation samples after startup must be incremental_warm")
        sample_id = sample.get("sample_id")
        if not isinstance(sample_id, str) or not sample_id or sample_id in sample_ids:
            raise ValueError("baseline sample IDs must be present and unique")
        sample_ids.add(sample_id)
        if sample.get("expected_build_status") != expected_outcome[case]:
            raise ValueError(f"{case} has an unexpected expected build status")
        if sample.get("outcome") != expected_outcome[case]:
            raise ValueError(f"{case} sample did not reach its expected build status")
        recovery = sample.get("recovery")
        if not isinstance(recovery, dict):
            raise ValueError("baseline sample is missing recovery evidence")
        if case == "login-invalid":
            if recovery.get("performed") is not True or recovery.get("build_status") != "succeeded":
                raise ValueError("compile-failure sample is missing successful recovery evidence")
        elif recovery.get("performed") is not False or recovery.get("build_status") is not None:
            raise ValueError("unexpected recovery evidence on a successful fixture")
        counts = per_case.setdefault(case, {"warmup": 0, "measure": 0})
        counts[phase] += 1
        if sample.get("outcome") == "failed":
            failure_samples += 1
    if set(per_case) != EXPECTED_CASES:
        raise ValueError("baseline does not contain all three fixed fixtures")
    if any(counts != {"warmup": 10, "measure": 30} for counts in per_case.values()):
        raise ValueError("each fixed fixture must contain 10 warmups and 30 measurements")
    if failure_samples == 0:
        raise ValueError("expected compile-failure samples were not retained")

    counts = summary.get("counts", {})
    if counts.get("all") != 120 or counts.get("warmup") != 30 or counts.get("measurements") != 90:
        raise ValueError("summary sample totals are inconsistent")

    spans = read_ndjson(output / "spans.ndjson")
    commands = read_ndjson(output / "commands.ndjson")
    if len(spans) != counts.get("spans"):
        raise ValueError("span count does not match summary")
    if not commands:
        raise ValueError("command evidence is empty")
    sample_spans: dict[str, list[dict[str, Any]]] = {}
    for span in spans:
        sample_id = span.get("sample_id")
        case = span.get("case")
        phase = span.get("phase")
        if case not in EXPECTED_CASES or phase not in {"setup", "warmup", "measure"}:
            raise ValueError("span has an unknown fixture or phase")
        if not isinstance(sample_id, str):
            raise ValueError("span is missing its sample ID")
        if phase != "setup" and sample_id not in sample_ids:
            raise ValueError("span refers to an unknown sample")
        sample_spans.setdefault(sample_id, []).append(span)
    startup_build_count = verify_startup_builds(summary, spans)
    for sample in samples:
        sample_id = sample["sample_id"]
        associated = sample_spans.get(sample_id, [])
        if len(associated) != sample.get("span_count") or not associated:
            raise ValueError(f"span records do not match sample {sample_id}")
        if any(
            span.get("case") != sample["case"] or span.get("phase") != sample["phase"]
            for span in associated
        ):
            raise ValueError(f"span identity does not match sample {sample_id}")
        build_statuses = {
            span.get("status")
            for span in associated
            if span.get("name") == "build"
        }
        expected_span_status = "failed" if sample["case"] == "login-invalid" else "ok"
        if expected_span_status not in build_statuses:
            raise ValueError(f"build span is missing for sample {sample_id}")
    if any(
        isinstance(command.get("argv"), list)
        and command["argv"][1:3] == ["run", "--live"]
        for command in commands
    ) is False:
        raise ValueError("live run commands were not retained")
    if not any(
        isinstance(command.get("argv"), list)
        and command["argv"][1:3] == ["dev", "status"]
        for command in commands
    ):
        raise ValueError("live status commands were not retained")

    binary_path = output / "bin" / "gpui"
    binary_hash = hashlib.sha256(binary_path.read_bytes()).hexdigest()
    if source.get("cli_sha256") != binary_hash:
        raise ValueError("CLI binary hash does not match the source manifest")
    if source.get("cli_version") != environment.get("toolchain", {}).get("gpui_version"):
        raise ValueError("CLI version differs between source and environment evidence")

    return {
        "status": "pass",
        "revision": expected_commit,
        "dirty": expected_dirty,
        "samples": counts["all"],
        "warmup": counts["warmup"],
        "measurements": counts["measurements"],
        "spans": len(spans),
        "commands": len(commands),
        "failure_samples": failure_samples,
        "startup_builds": startup_build_count,
        "startup_cold_builds": len(EXPECTED_CASES),
        "cases": {case: counts for case, counts in sorted(per_case.items())},
        "cli_sha256": binary_hash,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--expected-commit", required=True)
    parser.add_argument("--expected-dirty", choices=("true", "false"), default="false")
    args = parser.parse_args()
    try:
        result = verify(args.output, args.expected_commit, args.expected_dirty == "true")
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"F01 baseline verification failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()

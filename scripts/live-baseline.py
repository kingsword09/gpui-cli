#!/usr/bin/env python3
"""Run the deterministic F01 live-loop baseline and write raw evidence.

The driver intentionally uses dependency-free Rust projects. It measures the
CLI/supervisor loop without claiming that a headless Cargo fixture is a GPUI
window test; real UI scenarios are introduced by the later S01/S02 work.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable


ROOT = Path(__file__).resolve().parent.parent
FIXTURE_DIR = ROOT / "tests" / "fixtures" / "agent-native"
CASE_DEFINITIONS = {
    "counter": {
        "fixture": "counter-zero.json",
        "mutation": "source",
        "description": "fixed small source edit",
    },
    "login-invalid": {
        "fixture": "login-invalid.json",
        "mutation": "compile_failure",
        "description": "fixed compiler failure and recovery",
    },
    "list-scroll": {
        "fixture": "list-1000.json",
        "mutation": "asset",
        "description": "fixed asset change",
    },
}


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def nonnegative(value: str) -> int:
    parsed = int(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be non-negative")
    return parsed


def run_capture(*argv: str, cwd: Path | None = None) -> str | None:
    try:
        completed = subprocess.run(
            argv,
            cwd=cwd,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0:
        return None
    output = completed.stdout.strip() or completed.stderr.strip()
    return output or None


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def read_spans(project: Path, session_id: str) -> list[dict[str, Any]]:
    path = project / ".gpui" / "live" / session_id / "spans.ndjson"
    if not path.exists():
        return []
    records: list[dict[str, Any]] = []
    for line in path.read_text().splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            records.append(value)
    return records


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * fraction
    lower = int(position)
    upper = min(lower + 1, len(ordered) - 1)
    weight = position - lower
    return ordered[lower] + (ordered[upper] - ordered[lower]) * weight


def duration_ms(record: dict[str, Any]) -> float | None:
    duration = record.get("duration_ns")
    if not isinstance(duration, int):
        return None
    return duration / 1_000_000


def summarize_groups(samples: list[dict[str, Any]], field: str) -> dict[str, Any]:
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for sample in samples:
        grouped[str(sample.get(field, "unknown"))].append(sample)
    result: dict[str, Any] = {}
    for name, entries in sorted(grouped.items()):
        durations = [
            float(entry["driver_elapsed_ms"])
            for entry in entries
            if isinstance(entry.get("driver_elapsed_ms"), (int, float))
        ]
        result[name] = {
            "samples": len(entries),
            "outcomes": dict(sorted(
                (outcome, sum(1 for entry in entries if entry.get("outcome") == outcome))
                for outcome in {entry.get("outcome", "unknown") for entry in entries}
            )),
            "driver_elapsed_ms": {
                "p50": percentile(durations, 0.50),
                "p95": percentile(durations, 0.95),
                "min": min(durations) if durations else None,
                "max": max(durations) if durations else None,
            },
        }
    return result


def summarize_stages(samples: list[dict[str, Any]]) -> dict[str, Any]:
    values: dict[str, list[float]] = defaultdict(list)
    for sample in samples:
        for name, value in sample.get("stage_duration_ms", {}).items():
            if isinstance(value, (int, float)):
                values[name].append(float(value))
    return {
        name: {
            "samples": len(durations),
            "p50": percentile(durations, 0.50),
            "p95": percentile(durations, 0.95),
            "total": sum(durations),
        }
        for name, durations in sorted(values.items())
    }


def environment_report(args: argparse.Namespace, gpui: Path) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "generated_at": utc_now(),
        "driver": {
            "name": "gpui-cli F01 live baseline",
            "version": "1",
            "argv": sys.argv,
            "gpui": str(gpui),
            "warmup_runs": args.warmup,
            "measurement_runs": args.measurements,
            "timeout_seconds": args.timeout,
            "offline": args.offline,
            "cases": args.cases.split(","),
        },
        "repository": {
            "root": str(ROOT),
            "commit": run_capture("git", "rev-parse", "HEAD", cwd=ROOT),
            "dirty": bool(run_capture("git", "status", "--short", cwd=ROOT)),
        },
        "host": {
            "platform": platform.platform(),
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "processor": platform.processor(),
            "python": platform.python_version(),
        },
        "toolchain": {
            "rustc": run_capture("rustc", "-Vv"),
            "cargo": run_capture("cargo", "-V"),
            "gpui_version": run_capture(str(gpui), "--version"),
        },
        "fixtures": {
            case_id: {
                "path": str(FIXTURE_DIR / definition["fixture"]),
                "sha256": hashlib.sha256(
                    (FIXTURE_DIR / definition["fixture"]).read_bytes()
                ).hexdigest(),
                "mutation": definition["mutation"],
            }
            for case_id, definition in CASE_DEFINITIONS.items()
        },
        "cache_policy": {
            "startup": "startup_cold",
            "subsequent": "incremental_warm",
            "note": "The driver labels the first supervisor build separately; it does not purge a global Cargo cache.",
        },
        "limitations": [
            "headless Cargo fixtures do not prove GPUI scene, frame, or GPU behavior",
            "device clock timestamps are not collected by this desktop-only driver",
            "driver_elapsed_ms is orchestration wall time; stage durations come from supervisor spans",
        ],
    }


def good_source(marker: str) -> str:
    return (
        "fn main() {\n"
        f'    println!("{marker}");\n'
        "    loop { std::thread::sleep(std::time::Duration::from_millis(20)); }\n"
        "}\n"
    )


def bad_source() -> str:
    return 'fn main() { let _: u32 = "baseline type error"; }\n'


def write_project(project: Path, case_id: str, fixture: dict[str, Any]) -> None:
    project.mkdir(parents=True, exist_ok=True)
    (project / "crates" / "desktop" / "src").mkdir(parents=True)
    (project / "assets").mkdir()
    write_json(project / "fixture.json", fixture)
    (project / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/desktop"]\nresolver = "3"\n'
    )
    (project / "gpui.toml").write_text(
        f'[app]\nname = "f01-{case_id}"\ntitle = "F01 {case_id}"\n'
    )
    (project / "crates" / "desktop" / "Cargo.toml").write_text(
        f'[package]\nname = "f01-{case_id}-desktop"\nversion = "0.1.0"\nedition = "2024"\n'
    )
    (project / "crates" / "desktop" / "src" / "main.rs").write_text(
        good_source(f"{case_id}-initial")
    )
    (project / "assets" / "baseline.txt").write_text("initial\n")


class LiveSession:
    def __init__(
        self,
        gpui: Path,
        project: Path,
        timeout: float,
        offline: bool,
        commands: list[dict[str, Any]],
    ) -> None:
        self.gpui = gpui
        self.project = project
        self.timeout = timeout
        self.offline = offline
        self.commands = commands
        self.process: subprocess.Popen[bytes] | None = None
        self.output_file: Any = None
        self.session_id: str | None = None

    def record_command(self, argv: list[str]) -> None:
        self.commands.append({
            "recorded_at": utc_now(),
            "argv": argv,
            "cwd": str(self.project),
        })

    def status(self) -> dict[str, Any] | None:
        argv = [str(self.gpui), "dev", "status", "--json"]
        self.record_command(argv)
        try:
            completed = subprocess.run(
                argv,
                cwd=self.project,
                check=False,
                capture_output=True,
                text=True,
                timeout=10,
            )
        except (OSError, subprocess.SubprocessError):
            return None
        if completed.returncode != 0:
            return None
        try:
            value = json.loads(completed.stdout)
        except json.JSONDecodeError:
            return None
        result = value.get("result")
        return result if isinstance(result, dict) else None

    def start(self) -> dict[str, Any]:
        log_path = self.project.parent / "live.log"
        self.output_file = log_path.open("wb")
        argv = [str(self.gpui), "run", "--live"]
        self.record_command(argv)
        env = os.environ.copy()
        if self.offline:
            env["CARGO_NET_OFFLINE"] = "true"
        self.process = subprocess.Popen(
            argv,
            cwd=self.project,
            env=env,
            stdin=subprocess.PIPE,
            stdout=self.output_file,
            stderr=subprocess.STDOUT,
        )
        initial = self.wait(
            lambda value: value.get("build", {}).get("status") == "succeeded"
            and value.get("running", {}).get("process") == "running"
        )
        self.session_id = str(initial["session_id"])
        return initial

    def wait(self, predicate: Callable[[dict[str, Any]], bool]) -> dict[str, Any]:
        deadline = time.monotonic() + self.timeout
        last: dict[str, Any] | None = None
        while time.monotonic() < deadline:
            if self.process is not None and self.process.poll() is not None:
                trace = (self.project.parent / "live.log").read_text(errors="replace")
                raise RuntimeError(f"live exited with {self.process.returncode}:\n{trace[-4000:]}")
            last = self.status()
            if last is not None and predicate(last):
                return last
            time.sleep(0.1)
        raise TimeoutError(f"live status did not reach target; last={last}")

    def spans(self) -> list[dict[str, Any]]:
        if self.session_id is None:
            return []
        return read_spans(self.project, self.session_id)

    def stop(self) -> None:
        if self.process is None:
            return
        if self.process.poll() is None and self.process.stdin is not None:
            try:
                self.process.stdin.write(b"q\n")
                self.process.stdin.flush()
            except OSError:
                pass
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        if self.output_file is not None:
            self.output_file.close()


def run_sample(
    session: LiveSession,
    case_id: str,
    mutation: str,
    phase: str,
    ordinal: int,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    source = session.project / "crates" / "desktop" / "src" / "main.rs"
    asset = session.project / "assets" / "baseline.txt"
    before = len(session.spans())
    previous = session.status() or {}
    previous_build_id = previous.get("build", {}).get("build_id")
    previous_revision = (
        previous.get("desired", {}).get("source_revision"),
        previous.get("desired", {}).get("asset_revision"),
    )
    started = time.monotonic()
    started_ms = int(time.time() * 1000)
    sample_id = f"{case_id}-{phase}-{ordinal:03d}"
    expected = "succeeded"
    if mutation == "source":
        source.write_text(good_source(f"{case_id}-{sample_id}"))
    elif mutation == "compile_failure":
        expected = "failed"
        source.write_text(bad_source())
    elif mutation == "asset":
        asset.write_text(f"{sample_id}\n")
    else:
        raise ValueError(f"unknown mutation {mutation}")

    def reached_target(value: dict[str, Any]) -> bool:
        revision = (
            value.get("desired", {}).get("source_revision"),
            value.get("desired", {}).get("asset_revision"),
        )
        build_id = value.get("build", {}).get("build_id")
        changed = build_id != previous_build_id or revision != previous_revision
        return (
            changed
            and value.get("build", {}).get("status") == expected
            and (expected == "failed" or value.get("stale") is False)
        )

    status = session.wait(reached_target)
    primary_spans = session.spans()
    new_spans = primary_spans[before:]
    recovery_status: dict[str, Any] | None = None
    if mutation == "compile_failure":
        source.write_text(good_source(f"{case_id}-{sample_id}-recovery"))
        recovery_status = session.wait(
            lambda value: value.get("build", {}).get("status") == "succeeded"
            and value.get("stale") is False
        )
        new_spans = session.spans()[before:]

    builds = [record for record in new_spans if record.get("name") == "build"]
    primary_build = next(
        (record for record in builds if record.get("status") == expected),
        builds[0] if builds else None,
    )
    build_ids = sorted({
        str(record["build_id"])
        for record in new_spans
        if record.get("build_id") is not None
    })
    stage_durations: dict[str, float] = defaultdict(float)
    for record in new_spans:
        value = duration_ms(record)
        if value is not None and record.get("name") not in {"build", "build.queue"}:
            stage_durations[str(record.get("name", "unknown"))] += value

    for record in new_spans:
        record["sample_id"] = sample_id
        record["case"] = case_id
        record["phase"] = phase

    elapsed = (time.monotonic() - started) * 1000
    sample = {
        "sample_id": sample_id,
        "case": case_id,
        "phase": phase,
        "mutation": mutation,
        "cache_kind": "incremental_warm",
        "expected_build_status": expected,
        "outcome": status.get("build", {}).get("status", "unknown"),
        "driver_elapsed_ms": elapsed,
        "build_duration_ms": duration_ms(primary_build) if primary_build else None,
        "stage_duration_ms": dict(sorted(stage_durations.items())),
        "build_ids": build_ids,
        "span_count": len(new_spans),
        "source_revision": status.get("desired", {}).get("source_revision"),
        "asset_revision": status.get("desired", {}).get("asset_revision"),
        "recovery": {
            "performed": recovery_status is not None,
            "build_status": recovery_status.get("build", {}).get("status")
            if recovery_status
            else None,
        },
        "started_at_ms": started_ms,
        "finished_at_ms": int(time.time() * 1000),
    }
    return sample, new_spans


def run_case(
    gpui: Path,
    scratch: Path,
    case_id: str,
    args: argparse.Namespace,
    commands: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    definition = CASE_DEFINITIONS[case_id]
    fixture_path = FIXTURE_DIR / definition["fixture"]
    fixture = json.loads(fixture_path.read_text())
    project = scratch / case_id
    write_project(project, case_id, fixture)
    session = LiveSession(gpui, project, args.timeout, args.offline, commands)
    samples: list[dict[str, Any]] = []
    spans: list[dict[str, Any]] = []
    try:
        session.start()
        initial_spans = session.spans()
        for record in initial_spans:
            record["sample_id"] = f"{case_id}-setup"
            record["case"] = case_id
            record["phase"] = "setup"
        spans.extend(initial_spans)
        for phase, count in (("warmup", args.warmup), ("measure", args.measurements)):
            for ordinal in range(count):
                sample, sample_spans = run_sample(
                    session, case_id, definition["mutation"], phase, ordinal
                )
                if phase == "warmup":
                    sample["cache_kind"] = "startup_cold" if ordinal == 0 else "incremental_warm"
                samples.append(sample)
                spans.extend(sample_spans)
    finally:
        session.stop()
    return samples, spans


def run_baseline(args: argparse.Namespace) -> None:
    gpui = args.gpui.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    commands: list[dict[str, Any]] = []
    samples: list[dict[str, Any]] = []
    spans: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="gpui-f01-baseline-") as temporary:
        scratch = Path(temporary)
        for case_id in args.cases.split(","):
            case_id = case_id.strip()
            if case_id not in CASE_DEFINITIONS:
                raise ValueError(f"unknown case {case_id}")
            case_samples, case_spans = run_case(gpui, scratch, case_id, args, commands)
            samples.extend(case_samples)
            spans.extend(case_spans)

    environment = environment_report(args, gpui)
    summary = {
        "schema_version": 1,
        "generated_at": utc_now(),
        "driver": "scripts/live-baseline.py",
        "sample_policy": {
            "warmup_runs": args.warmup,
            "measurement_runs": args.measurements,
            "failure_samples_retained": True,
            "superseded_samples_retained": True,
        },
        "samples": samples,
        "counts": {
            "all": len(samples),
            "warmup": sum(1 for sample in samples if sample["phase"] == "warmup"),
            "measurements": sum(1 for sample in samples if sample["phase"] == "measure"),
            "spans": len(spans),
        },
        "groups": {
            "measurements": {
                "case": summarize_groups(
                    [sample for sample in samples if sample["phase"] == "measure"], "case"
                ),
                "cache_kind": summarize_groups(
                    [sample for sample in samples if sample["phase"] == "measure"], "cache_kind"
                ),
                "outcome": summarize_groups(
                    [sample for sample in samples if sample["phase"] == "measure"], "outcome"
                ),
                "stage_duration_ms": summarize_stages(
                    [sample for sample in samples if sample["phase"] == "measure"]
                ),
            },
            "warmup": {
                "case": summarize_groups(
                    [sample for sample in samples if sample["phase"] == "warmup"], "case"
                ),
                "cache_kind": summarize_groups(
                    [sample for sample in samples if sample["phase"] == "warmup"], "cache_kind"
                ),
                "outcome": summarize_groups(
                    [sample for sample in samples if sample["phase"] == "warmup"], "outcome"
                ),
            },
        },
        "interpretation": {
            "status": "raw_baseline",
            "performance_claim": False,
            "note": "Use this report to choose fixed samples and inspect bottlenecks; it is not a cross-machine speed promise.",
        },
        "artifacts": {
            "environment": "environment.json",
            "spans": "spans.ndjson",
            "commands": "commands.ndjson",
        },
    }
    write_json(output / "environment.json", environment)
    write_json(output / "summary.json", summary)
    (output / "spans.ndjson").write_text(
        "".join(json.dumps(record, sort_keys=True) + "\n" for record in spans)
    )
    (output / "commands.ndjson").write_text(
        "".join(json.dumps(command, sort_keys=True) + "\n" for command in commands)
    )
    print(json.dumps({"output": str(output), "summary": summary["counts"]}, indent=2))


def self_test() -> None:
    fixtures = {
        case_id: json.loads((FIXTURE_DIR / definition["fixture"]).read_text())
        for case_id, definition in CASE_DEFINITIONS.items()
    }
    assert fixtures["counter"]["initial_value"] == 0
    assert fixtures["login-invalid"]["network"]["response"]["code"] == "invalid_credentials"
    assert fixtures["list-scroll"]["items"]["count"] == 1000
    assert abs(percentile([1.0, 2.0, 3.0, 4.0], 0.95) - 3.85) < 1e-9
    assert summarize_groups(
        [
            {"case": "counter", "outcome": "succeeded", "cache_kind": "warm", "driver_elapsed_ms": 2.0},
            {"case": "counter", "outcome": "failed", "cache_kind": "warm", "driver_elapsed_ms": 4.0},
        ],
        "case",
    )["counter"]["samples"] == 2
    print("live-baseline self-test: ok")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gpui", type=Path, default=ROOT / "target" / "debug" / "gpui")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--cases", default=",".join(CASE_DEFINITIONS))
    parser.add_argument("--warmup", type=nonnegative, default=10)
    parser.add_argument("--measurements", type=nonnegative, default=30)
    parser.add_argument("--timeout", type=nonnegative, default=120)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.output is None:
        parser.error("--output is required unless --self-test is used")
    run_baseline(args)


if __name__ == "__main__":
    main()

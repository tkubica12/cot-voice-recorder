"""Compare full signed-out client fixtures; never use installed state or physical ports."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import statistics
import subprocess
import tempfile
import time
import uuid
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Self

SCOPE = "full-client-signed-out-fixture"
ROOT = Path(__file__).resolve().parents[1]


def fixture_files(
    state: Path, key: str, history_count: int, epoch: datetime | None = None
) -> dict[str, bytes]:
    state.mkdir()
    now = epoch or datetime.now(UTC)
    body = "synthetic history content " * 16
    files = {
        "migration-fixture.json": {"scope": SCOPE, "instance_key": key},
        "settings.json": {
            "backend_base_url": "https://fixture.example.invalid",
            "notifications_paused": True,
            "auto_start": False,
            "dictation_enabled": False,
        },
        "history.json": [
            {
                "transcript_id": f"fixture-{index}",
                "recording_id": f"fixture-recording-{index}",
                "body": body,
                "raw_body": body + " original",
                "preview": "synthetic history",
                "character_count": len(body),
                "completed_at": (now - timedelta(seconds=index)).isoformat(),
                "cached_at": now.isoformat(),
            }
            for index in range(history_count)
        ],
    }
    encoded = {
        name: json.dumps(value, separators=(",", ":"), ensure_ascii=True).encode()
        for name, value in files.items()
    }
    for name, data in encoded.items():
        state.joinpath(name).write_bytes(data)
    return encoded


class FixtureProcess:
    def __init__(
        self,
        binary: Path,
        implementation: str,
        parent: Path,
        history_count: int,
        epoch: datetime | None = None,
    ) -> None:
        if os.name != "nt":
            raise RuntimeError("Client comparison requires Windows.")
        expected = (
            "VoicePromptClientFixture.exe"
            if implementation == "csharp"
            else "VoicePromptRustPilot.exe"
        )
        if binary.name != expected or implementation not in {"csharp", "rust"}:
            raise ValueError(
                "Only the explicitly named isolated fixture binaries are allowed."
            )
        self.binary = binary.resolve(strict=True)
        self.implementation = implementation
        self.directory = tempfile.TemporaryDirectory(
            prefix="VoicePrompt-full-client-fixture-", dir=parent
        )
        self.root = Path(self.directory.name)
        self.state = self.root.joinpath("state")
        self.key = uuid.uuid4().hex
        self.inputs = fixture_files(self.state, self.key, history_count, epoch)
        ready = self.root.joinpath("ready.json")
        if implementation == "csharp":
            arguments = [
                "--client-fixture",
                str(self.state),
                str(ready),
                self.key,
                "3600",
            ]
        else:
            arguments = [
                "--signed-out-fixture",
                str(self.state),
                "--fixture-ready-file",
                str(ready),
                "--instance-key",
                self.key,
                "--run-for-seconds",
                "3600",
            ]
        environment = dict(os.environ)
        forbidden_oauth = self.root.joinpath("must-not-discover-oauth.json")
        forbidden_oauth.write_text(
            json.dumps(
                {
                    "installed": {
                        "client_id": "synthetic-must-not-discover",
                        "client_secret": "synthetic-fixture",
                    }
                }
            ),
            encoding="utf-8",
        )
        environment["VOICEPROMPT_GOOGLE_DESKTOP_CLIENT"] = str(forbidden_oauth)
        started = time.perf_counter()
        try:
            self.process = subprocess.Popen(
                [str(self.binary), *arguments],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                creationflags=subprocess.CREATE_NO_WINDOW,
                env=environment,
            )
            while not ready.exists():
                if self.process.poll() is not None:
                    _, error = self.process.communicate()
                    raise RuntimeError(
                        f"{implementation} fixture failed: {error.decode(errors='replace')}"
                    )
                if time.perf_counter() - started > 15:
                    raise TimeoutError(
                        "Full-client fixture readiness exceeded fifteen seconds."
                    )
                time.sleep(0.005)
            self.startup_ms = (time.perf_counter() - started) * 1000
            self.marker = json.loads(ready.read_bytes())
            expected_marker = {
                "status": "ready",
                "scope": SCOPE,
                "pid": self.process.pid,
                "implementation": implementation,
                "history_count": history_count,
                "auth": "not_configured",
                "physical_ports_enabled": False,
                "full_components": True,
                "instance_key": self.key,
            }
            if self.marker != expected_marker:
                raise RuntimeError(
                    f"Readiness did not match the actual full-client fixture: {self.marker}"
                )
        except BaseException:
            if hasattr(self, "process"):
                self.kill_owned()
            self.directory.cleanup()
            raise

    def kill_owned(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
        self.process.communicate(timeout=10)

    def close(self) -> None:
        try:
            if self.process.poll() is None:
                arguments = (
                    ["--client-fixture-quit", self.key]
                    if self.implementation == "csharp"
                    else ["--quit", "--instance-key", self.key]
                )
                quit_process = subprocess.run(
                    [str(self.binary), *arguments],
                    capture_output=True,
                    timeout=15,
                    check=False,
                    creationflags=subprocess.CREATE_NO_WINDOW,
                )
                if quit_process.returncode:
                    raise RuntimeError(
                        f"Isolated fixture quit failed: {quit_process.stderr!r}"
                    )
            output, error = self.process.communicate(timeout=15)
            if self.process.returncode or error:
                raise RuntimeError(
                    f"Fixture exited unsuccessfully: {error!r} {output!r}"
                )
            for name, expected in self.inputs.items():
                if self.state.joinpath(name).read_bytes() != expected:
                    raise RuntimeError(
                        f"Fixture unexpectedly changed its input {name}."
                    )
            if self.state.joinpath("tokens.bin").exists():
                raise RuntimeError(
                    "Signed-out fixture unexpectedly wrote a token cache."
                )
        finally:
            if self.process.poll() is None:
                self.kill_owned()
            self.directory.cleanup()

    def __enter__(self) -> Self:
        return self

    def __exit__(self, kind: object, value: object, traceback: object) -> None:
        self.close()


def p95(values: list[float]) -> float:
    return sorted(values)[math.ceil(len(values) * 0.95) - 1]


def bundle_hashes(binary: Path, implementation: str) -> dict[str, str]:
    files = [binary]
    if implementation == "csharp":
        files.extend(
            binary.parent.joinpath(name)
            for name in [
                "VoicePromptClientFixture.dll",
                "VoicePrompt.dll",
                "VoicePrompt.Core.dll",
                "VoicePromptClientFixture.deps.json",
                "VoicePromptClientFixture.runtimeconfig.json",
            ]
        )
    return {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in files}


def measure(
    csharp: Path,
    native: Path,
    directory: Path,
    revision: str,
    trials: int,
    samples: int,
    series: int,
    history_count: int,
) -> Path:
    directory = directory.resolve(strict=True)
    output = directory.joinpath("full-client-signed-out-comparison.json")
    if output.exists():
        raise FileExistsError(output)
    binaries = {"csharp": csharp, "rust": native}
    bundles = {name: bundle_hashes(path, name) for name, path in binaries.items()}
    epoch = datetime.now(UTC)
    corpus = None
    startup: dict[str, list[float]] = {"csharp": [], "rust": []}
    rows = []
    for trial in range(trials):
        order = ["csharp", "rust"] if trial % 2 == 0 else ["rust", "csharp"]
        for implementation in order:
            with FixtureProcess(
                binaries[implementation],
                implementation,
                directory,
                history_count,
                epoch,
            ) as fixture:
                digest = {
                    name: hashlib.sha256(fixture.inputs[name]).hexdigest()
                    for name in ["settings.json", "history.json"]
                }
                if corpus is not None and corpus != digest:
                    raise RuntimeError("Client input corpus changed.")
                corpus = digest
                startup[implementation].append(fixture.startup_ms)
                rows.append(
                    {
                        "trial": trial + 1,
                        "implementation": implementation,
                        "pid": fixture.process.pid,
                        "startup_ms": fixture.startup_ms,
                    }
                )
                print(
                    f"{implementation} startup {trial + 1}/{trials}: "
                    f"{fixture.startup_ms:.3f} ms; owned PID {fixture.process.pid}",
                    flush=True,
                )
    memory: dict[str, list[dict]] = {"csharp": [], "rust": []}
    for index in range(series):
        for implementation in (
            ["csharp", "rust"] if index % 2 == 0 else ["rust", "csharp"]
        ):
            target = directory.joinpath(f"{implementation}-idle-{index + 1}.json")
            if target.exists():
                raise FileExistsError(target)
            with FixtureProcess(
                binaries[implementation],
                implementation,
                directory,
                history_count,
                epoch,
            ) as fixture:
                digest = {
                    name: hashlib.sha256(fixture.inputs[name]).hexdigest()
                    for name in ["settings.json", "history.json"]
                }
                if corpus != digest:
                    raise RuntimeError(
                        "Client memory corpus differs from startup corpus."
                    )
                print(
                    f"Sampling {implementation} series {index + 1}/{series}, "
                    f"{samples} points, owned PID {fixture.process.pid}",
                    flush=True,
                )
                subprocess.run(
                    [
                        "pwsh",
                        "-NoProfile",
                        "-NonInteractive",
                        "-File",
                        str(ROOT.joinpath("scripts", "Measure-VoicePromptProcess.ps1")),
                        "-ProcessId",
                        str(fixture.process.pid),
                        "-Scenario",
                        SCOPE,
                        "-SampleCount",
                        str(samples),
                        "-IntervalMilliseconds",
                        "1000",
                        "-SourceRevision",
                        revision,
                        "-OutputPath",
                        str(target),
                    ],
                    check=True,
                    timeout=samples + 60,
                )
                report = json.loads(target.read_text(encoding="utf-8-sig"))
                if (
                    report["status"] != "complete"
                    or report["scenario"] != SCOPE
                    or report["source_revision"] != revision
                    or report["process"]["id"] != fixture.process.pid
                    or report["process"]["binary_sha256"].lower()
                    != bundles[implementation][binaries[implementation].name]
                    or report["measurement"]["sample_count"] != samples
                    or len(report["samples"]) != samples
                    or report["measurement"]["requested_interval_milliseconds"] != 1000
                ):
                    raise RuntimeError(
                        "The process sampler did not prove the requested process, inputs and sample shape."
                    )
                memory[implementation].append(report)
    private = {
        implementation: [
            sample["private_bytes"]
            for report in reports
            for sample in report["samples"]
        ]
        for implementation, reports in memory.items()
    }
    startup_summary = {
        implementation: {
            "mean_ms": statistics.mean(values),
            "p95_ms": p95(values),
            "max_ms": max(values),
        }
        for implementation, values in startup.items()
    }
    memory_summary = {
        implementation: {
            "mean_private_bytes": statistics.mean(values),
            "max_private_bytes": max(values),
        }
        for implementation, values in private.items()
    }
    per_series = {
        implementation: [
            {
                "mean_private_bytes": statistics.mean(
                    sample["private_bytes"] for sample in report["samples"]
                ),
                "max_private_bytes": max(
                    sample["private_bytes"] for sample in report["samples"]
                ),
            }
            for report in reports
        ]
        for implementation, reports in memory.items()
    }
    if bundles != {name: bundle_hashes(path, name) for name, path in binaries.items()}:
        raise RuntimeError("Client binaries changed during the measurement series.")
    sufficient = trials >= 30 and series >= 3 and samples >= 301
    result = {
        "schema_version": 1,
        "status": "complete",
        "scope": SCOPE,
        "source_revision": revision,
        "history_entries": history_count,
        "trial_count_per_implementation": trials,
        "memory_series": series,
        "samples_per_memory_series": samples,
        "sufficient_for_this_idle_profile": sufficient,
        "physical_ports_enabled": False,
        "measures_microphone_or_hour_capture": False,
        "measures_cloud_latency_or_billing": False,
        "measures_os_cold_cache": False,
        "whole_migration_acceptance": False,
        "percentile_method": "nearest-rank",
        "shared_machine": True,
        "default_installed_entry_point": False,
        "scope_exclusions": [
            "signed-in networking",
            "physical capture",
            "clipboard/paste",
            "registry autostart",
            "installation",
            "OS cold cache",
        ],
        "fixture_binary_bundle_sha256": bundles,
        "corpus_sha256": corpus,
        "fixture_epoch": epoch.isoformat(),
        "binary_sha256": {
            name: hashlib.sha256(path.read_bytes()).hexdigest()
            for name, path in binaries.items()
        },
        "startup": startup_summary,
        "memory": memory_summary,
        "memory_series_summary": per_series,
        "observed_profile_thresholds": {
            "startup": startup_summary["rust"]["p95_ms"] <= 1000
            and startup_summary["rust"]["p95_ms"]
            <= startup_summary["csharp"]["p95_ms"] * 0.70,
            "idle_private": memory_summary["rust"]["max_private_bytes"]
            <= 80 * 1024 * 1024
            and all(
                per_series["rust"][index]["mean_private_bytes"]
                <= per_series["csharp"][index]["mean_private_bytes"] * 0.50
                for index in range(series)
            ),
        },
        "startup_samples": rows,
        "memory_reports": {
            name: [f"{name}-idle-{index + 1}.json" for index in range(series)]
            for name in binaries
        },
    }
    with output.open("xb") as stream:
        stream.write(json.dumps(result, indent=2).encode())
        stream.flush()
        os.fsync(stream.fileno())
    return output


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csharp", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--startup-trials", type=int, default=30)
    parser.add_argument("--memory-samples", type=int, default=301)
    parser.add_argument("--memory-series", type=int, default=3)
    parser.add_argument("--history-count", type=int, default=200)
    args = parser.parse_args()
    if not (
        1 <= args.startup_trials <= 100
        and 2 <= args.memory_samples <= 3301
        and 1 <= args.memory_series <= 10
        and 0 <= args.history_count <= 200
    ):
        parser.error("Measurement counts are outside their supported bounds.")
    print(
        measure(
            args.csharp.resolve(strict=True),
            args.rust.resolve(strict=True),
            args.output_directory,
            args.source_revision,
            args.startup_trials,
            args.memory_samples,
            args.memory_series,
            args.history_count,
        )
    )


if __name__ == "__main__":
    main()

"""Compare full signed-out client fixtures; never use installed state or physical ports."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import subprocess
import tempfile
import time
import uuid
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Self

SCOPE = "full-client-signed-out-fixture"
ROOT = Path(__file__).resolve().parents[1]


def read_ready_payload(
    ready: Path, process: subprocess.Popen, started: float, timeout_seconds: int
) -> bytes:
    deadline = started + timeout_seconds
    lock_deadline = None
    while True:
        if time.perf_counter() >= deadline:
            raise TimeoutError(f"Full-client fixture readiness exceeded {timeout_seconds} seconds.")
        if process.poll() is not None:
            raise RuntimeError(
                f"Full-client fixture exited before readable readiness: {process.returncode}"
            )
        try:
            with ready.open("rb") as stream:
                payload = stream.read(4097)
        except FileNotFoundError:
            pass
        except PermissionError as error:
            now = time.perf_counter()
            if lock_deadline is None:
                lock_deadline = min(now + 0.250, deadline)
            if now >= lock_deadline:
                error.add_note(
                    "Readiness remained inaccessible; Windows error "
                    f"{error.winerror if hasattr(error, 'winerror') else None}. "
                    "No ACL/sharing controls were changed and the trial was not restarted."
                )
                raise
        else:
            if len(payload) > 4096:
                raise ValueError("Full-client readiness exceeds supported bounds.")
            if time.perf_counter() >= deadline:
                raise TimeoutError(
                    "Full-client readiness was not readable within its original startup budget."
                )
            return payload
        if time.perf_counter() >= deadline:
            raise TimeoutError(f"Full-client fixture readiness exceeded {timeout_seconds} seconds.")
        time.sleep(0.005)


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
        diagnostics: bool = False,
        readiness_timeout_seconds: int = 15,
    ) -> None:
        if os.name != "nt":
            raise RuntimeError("Client comparison requires Windows.")
        if not 1 <= readiness_timeout_seconds <= 60:
            raise ValueError("Fixture readiness requires an explicit one-to-sixty-second budget.")
        expected = "VoicePromptRustPilot.exe"
        if binary.name != expected or implementation != "rust":
            raise ValueError("Only the explicitly named isolated fixture binaries are allowed.")
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
        environment["VOICEPROMPT_FIXTURE_PROGRESS"] = "1" if diagnostics else "0"
        environment["DOTNET_DISABLE_GUI_ERRORS"] = "1"
        started = time.perf_counter()
        try:
            self.process = subprocess.Popen(
                [str(self.binary), *arguments],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                creationflags=subprocess.CREATE_NO_WINDOW,
                env=environment,
            )
            payload = read_ready_payload(ready, self.process, started, readiness_timeout_seconds)
            self.startup_ms = (time.perf_counter() - started) * 1000
            self.marker = json.loads(payload)
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
        except BaseException as error:
            if hasattr(self, "process"):
                error.add_note(
                    f"Owned fixture implementation={implementation}, pid={self.process.pid}, "
                    f"readiness budget={readiness_timeout_seconds}s, "
                    f"elapsed_ms={(time.perf_counter() - started) * 1000:.6f}."
                )
                output, failure = self.kill_owned()
                if output or failure:
                    error.add_note(f"Fixture output: {output!r}; stderr: {failure!r}")
            progress = Path(str(ready) + ".progress")
            if progress.exists():
                error.add_note("C# fixture progress:\n" + progress.read_text(encoding="utf-8"))
            self.directory.cleanup()
            raise

    def kill_owned(self) -> tuple[bytes, bytes]:
        if self.process.poll() is None:
            self.process.kill()
        return self.process.communicate(timeout=10)

    def close(self) -> None:
        try:
            if self.process.poll() is None:
                arguments = ["--quit", "--instance-key", self.key]
                quit_process = subprocess.run(
                    [str(self.binary), *arguments],
                    capture_output=True,
                    timeout=15,
                    check=False,
                    creationflags=subprocess.CREATE_NO_WINDOW,
                )
                if quit_process.returncode:
                    raise RuntimeError(f"Isolated fixture quit failed: {quit_process.stderr!r}")
            output, error = self.process.communicate(timeout=15)
            if self.process.returncode or error:
                raise RuntimeError(f"Fixture exited unsuccessfully: {error!r} {output!r}")
            for name, expected in self.inputs.items():
                if self.state.joinpath(name).read_bytes() != expected:
                    raise RuntimeError(f"Fixture unexpectedly changed its input {name}.")
            if self.state.joinpath("tokens.bin").exists():
                raise RuntimeError("Signed-out fixture unexpectedly wrote a token cache.")
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


def publish(path: Path, payload: dict) -> None:
    with path.open("xb") as stream:
        stream.write(json.dumps(payload, indent=2, allow_nan=False).encode())
        stream.flush()
        os.fsync(stream.fileno())


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
                "VoicePrompt.deps.json",
                "VoicePrompt.runtimeconfig.json",
                "NAudio.Core.dll",
                "NAudio.WinMM.dll",
            ]
        )
        files.append(binary.parent.joinpath("Assets", "app.ico"))
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
    if output.exists() or directory.joinpath("measurement-failure.json").exists():
        raise FileExistsError(output)
    raise RuntimeError(
        "Paired C#/Rust collection was retired with the C# application. "
        "The isolated Rust lifecycle and immutable-evidence guards remain."
    )


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
    try:
        result = measure(
            args.csharp.resolve(strict=True),
            args.rust.resolve(strict=True),
            args.output_directory,
            args.source_revision,
            args.startup_trials,
            args.memory_samples,
            args.memory_series,
            args.history_count,
        )
    except BaseException as error:
        if args.output_directory.is_dir() and not isinstance(error, FileExistsError):
            path = args.output_directory.joinpath("measurement-failure.json")
            if not path.exists():
                publish(
                    path,
                    {
                        "status": "failed",
                        "scope": SCOPE,
                        "source_revision": args.source_revision,
                        "error_type": type(error).__name__,
                        "error": str(error),
                        "notes": getattr(error, "__notes__", []),
                        "complete_profile": False,
                        "whole_migration_acceptance": False,
                    },
                )
        raise
    print(result)


if __name__ == "__main__":
    main()

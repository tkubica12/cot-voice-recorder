"""Verify full client measurement isolation with synthetic state; no physical ports."""

from __future__ import annotations

import argparse
import inspect
import json
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import measure_clients
from measure_clients import (
    FixtureProcess,
    bundle_hashes,
    fixture_files,
    publish,
    read_ready_payload,
)

BINARIES: dict[str, Path]


class ClientFixtureTests(unittest.TestCase):
    def test_refused_profile_reuse_does_not_add_failure_to_completed_evidence(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-complete-proof-") as directory:
            root = Path(directory)
            report = root.joinpath("full-client-signed-out-comparison.json")
            report.write_bytes(b'{"synthetic":"complete"}')
            rust = root.joinpath("VoicePromptRustPilot.exe")
            csharp = root.joinpath("VoicePromptClientFixture.exe")
            rust.write_bytes(b"synthetic")
            csharp.write_bytes(b"synthetic")
            arguments = [
                "measure_clients.py",
                "--rust",
                str(rust),
                "--csharp",
                str(csharp),
                "--output-directory",
                str(root),
                "--source-revision",
                "synthetic",
            ]
            with (
                patch.object(sys, "argv", arguments),
                patch.object(measure_clients, "measure", side_effect=FileExistsError(report)),
                self.assertRaises(FileExistsError),
            ):
                measure_clients.main()
            self.assertEqual(report.read_bytes(), b'{"synthetic":"complete"}')
            self.assertFalse(root.joinpath("measurement-failure.json").exists())

    def test_partial_startup_evidence_is_immutable_and_failure_is_never_complete(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-partial-proof-") as directory:
            root = Path(directory)
            path = root.joinpath("startup-rust-01.json")
            payload = {"status": "observed-startup", "startup_ms": 123.456789}
            publish(path, payload)
            with self.assertRaises(FileExistsError):
                publish(path, {"status": "incorrect-overwrite"})
            self.assertEqual(json.loads(path.read_bytes()), payload)
            rust = root.joinpath("VoicePromptRustPilot.exe")
            csharp = root.joinpath("VoicePromptClientFixture.exe")
            rust.write_bytes(b"synthetic")
            csharp.write_bytes(b"synthetic")
            arguments = [
                "measure_clients.py",
                "--rust",
                str(rust),
                "--csharp",
                str(csharp),
                "--output-directory",
                str(root),
                "--source-revision",
                "synthetic",
            ]
            with (
                patch.object(sys, "argv", arguments),
                patch.object(
                    measure_clients,
                    "measure",
                    side_effect=TimeoutError("synthetic readiness timeout"),
                ),
                self.assertRaises(TimeoutError),
            ):
                measure_clients.main()
            failed = json.loads(root.joinpath("measurement-failure.json").read_bytes())
            self.assertEqual(failed["status"], "failed")
            self.assertFalse(failed["complete_profile"])
            self.assertFalse(failed["whole_migration_acceptance"])
            self.assertFalse(root.joinpath("full-client-signed-out-comparison.json").exists())

    def test_readable_readiness_includes_transient_file_lock_latency_and_never_restarts_trial(
        self,
    ) -> None:
        process = Mock(returncode=None)
        process.poll.return_value = None
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-ready-lock-") as directory:
            path = Path(directory).joinpath("ready.json")
            path.write_bytes(b'{"synthetic":true}')
            original = Path.open
            attempts = 0

            def open_after_lock(selected: Path, *args: object, **kwargs: object) -> object:
                nonlocal attempts
                attempts += 1
                if attempts < 3:
                    raise PermissionError("Synthetic Windows sharing failure.")
                return original(selected, *args, **kwargs)

            began = time.perf_counter()
            with patch.object(Path, "open", open_after_lock):
                result = read_ready_payload(path, process, began, 15)
            self.assertEqual(result, b'{"synthetic":true}')
            self.assertEqual(attempts, 3)
            self.assertGreaterEqual(time.perf_counter() - began, 0.010)

    def test_persistent_readiness_access_error_and_oversize_payload_are_not_success(
        self,
    ) -> None:
        process = Mock(returncode=None)
        process.poll.return_value = None
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-ready-refusal-") as directory:
            path = Path(directory).joinpath("ready.json")
            began = time.perf_counter()
            with (
                patch.object(
                    Path,
                    "open",
                    side_effect=PermissionError("Synthetic persistent ACL failure."),
                ),
                self.assertRaises(PermissionError),
            ):
                read_ready_payload(path, process, began, 15)
            self.assertLess(time.perf_counter() - began, 1)
            path.write_bytes(b"x" * 4097)
            with self.assertRaises(ValueError):
                read_ready_payload(path, process, time.perf_counter(), 15)
            with self.assertRaises(TimeoutError):
                read_ready_payload(path, process, time.perf_counter() - 16, 15)

    def test_bundle_attestation_covers_runtime_dependencies_without_discovering_oauth_files(
        self,
    ) -> None:
        names = [
            "VoicePromptClientFixture.exe",
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
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-bundle-fixture-") as directory:
            root = Path(directory)
            for name in names:
                root.joinpath(name).write_bytes(b"synthetic build fixture")
            root.joinpath("Assets").mkdir()
            root.joinpath("Assets", "app.ico").write_bytes(b"synthetic icon")
            root.joinpath("must-not-discover-oauth.json").write_bytes(b"synthetic excluded config")
            before = bundle_hashes(root.joinpath(names[0]), "csharp")
            self.assertEqual(set(before), {*names, "app.ico"})
            root.joinpath("NAudio.WinMM.dll").write_bytes(b"changed synthetic library")
            after = bundle_hashes(root.joinpath(names[0]), "csharp")
            self.assertNotEqual(before["NAudio.WinMM.dll"], after["NAudio.WinMM.dll"])
            self.assertEqual(
                before["VoicePromptClientFixture.exe"],
                after["VoicePromptClientFixture.exe"],
            )

    def test_both_actual_composition_roots_start_with_complete_runtime_and_only_marked_state(
        self,
    ) -> None:
        for implementation, binary in BINARIES.items():
            with (
                self.subTest(implementation=implementation),
                tempfile.TemporaryDirectory(prefix="VoicePrompt-fixture-guards-") as directory,
            ):
                root = Path(directory)
                budget = 60 if implementation == "csharp" else 15
                with FixtureProcess(
                    binary,
                    implementation,
                    root,
                    5,
                    diagnostics=True,
                    readiness_timeout_seconds=budget,
                ) as fixture:
                    print(
                        f"{implementation} functional full-runtime ready: "
                        f"{fixture.startup_ms:.3f}ms; safety budget {budget}s",
                        flush=True,
                    )
                    recovery = "dictation-recovery" if implementation == "csharp" else "recovery"
                    self.assertTrue(fixture.state.joinpath(recovery).is_dir())
                    self.assertTrue(fixture.marker["full_components"])
                    self.assertFalse(fixture.marker["physical_ports_enabled"])
                    self.assertEqual(fixture.marker["history_count"], 5)
                self.assertEqual(list(root.iterdir()), [])

    def test_functional_budget_cannot_change_the_measurement_default_or_be_unbounded(
        self,
    ) -> None:
        default = inspect.signature(FixtureProcess).parameters["readiness_timeout_seconds"].default
        self.assertEqual(default, 15)
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-fixture-budget-") as directory:
            for value in [0, 61]:
                with self.subTest(value=value), self.assertRaises(ValueError):
                    FixtureProcess(
                        BINARIES["rust"],
                        "rust",
                        Path(directory),
                        5,
                        readiness_timeout_seconds=value,
                    )
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_existing_tokens_and_wrong_marker_are_rejected_without_state_mutation(
        self,
    ) -> None:
        for implementation, binary in BINARIES.items():
            for mode in ["tokens", "marker", "marker_fields"]:
                with (
                    self.subTest(implementation=implementation, mode=mode),
                    tempfile.TemporaryDirectory(prefix="VoicePrompt-fixture-refusal-") as directory,
                ):
                    root = Path(directory)
                    state = root.joinpath("state")
                    key = uuid.uuid4().hex
                    fixture_files(state, key, 5)
                    if mode == "tokens":
                        state.joinpath("tokens.bin").write_bytes(b"synthetic forbidden token cache")
                    elif mode == "marker":
                        state.joinpath("migration-fixture.json").write_text(
                            json.dumps({"scope": "not-a-measurement", "instance_key": key})
                        )
                    else:
                        state.joinpath("migration-fixture.json").write_text(
                            json.dumps({"unknown": True, "other": key})
                        )
                    before = {path.name: path.read_bytes() for path in state.iterdir()}
                    ready = root.joinpath("ready.json")
                    if implementation == "csharp":
                        arguments = [
                            "--client-fixture",
                            str(state),
                            str(ready),
                            key,
                            "2",
                        ]
                    else:
                        arguments = [
                            "--signed-out-fixture",
                            str(state),
                            "--fixture-ready-file",
                            str(ready),
                            "--instance-key",
                            key,
                            "--run-for-seconds",
                            "2",
                        ]
                    result = subprocess.run(
                        [str(binary), *arguments],
                        capture_output=True,
                        timeout=15,
                        check=False,
                        creationflags=subprocess.CREATE_NO_WINDOW,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertTrue(result.stderr)
                    self.assertFalse(ready.exists())
                    self.assertEqual(
                        {path.name: path.read_bytes() for path in state.iterdir()},
                        before,
                    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, required=True)
    args = parser.parse_args()
    BINARIES = {
        "rust": args.rust.resolve(strict=True),
    }
    unittest.main(argv=[__file__], verbosity=2)

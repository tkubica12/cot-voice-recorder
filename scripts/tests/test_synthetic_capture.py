"""Verify failed/zero-test native captures preserve bounded diagnostic evidence."""

from __future__ import annotations

import argparse
import errno
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import measure_synthetic_capture as capture

ORACLE: Path


class SyntheticCaptureTests(unittest.TestCase):
    def test_functional_required_result_denial_is_host_incomplete_not_a_cached_progress_pass(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-final-read-") as directory:
            root = Path(directory)
            oracle = root / "VoicePromptNativeControllerFixture.exe"
            shutil.copyfile(ORACLE, oracle)
            evidence = root / "evidence"
            evidence.mkdir()
            read = capture.read_control
            read_bytes = capture.read_control_bytes
            def unavailable(path, limit):
                if path.name == "idle-ready.json":
                    raise PermissionError(errno.EACCES, "controlled essential result denial")
                return read_bytes(path, limit)
            def bounded(owned, process, binary, digest, name, seconds, **kwargs):
                return read(owned, process, binary, digest, name,
                            0.02 if name == "idle-ready.json" else seconds, **kwargs)
            with (
                patch.object(capture, "read_control", side_effect=bounded),
                patch.object(capture, "read_control_bytes", side_effect=unavailable),
                self.assertRaises(capture.ControlReadError),
            ):
                capture.run_functional(oracle, evidence, "essential-result-guard", 3)
            failure = json.loads((evidence / "failure.json").read_bytes())
            self.assertEqual(failure["control_read"]["control_file"], "idle-ready.json")
            self.assertEqual(failure["control_read"]["kind"], "read_timeout")
            self.assertIsNone(failure["native_exit_before_cleanup"])
            self.assertTrue(failure["native_kill_requested_by_collector"])
            self.assertIsNotNone(failure["native_exit_after_cleanup"])
            self.assertEqual(failure["stderr"], "")
            self.assertFalse((evidence / "functional-controller.json").exists())
            self.assertTrue((evidence / "state-hashes.json").exists())
            self.assertTrue(json.loads((evidence / "data-cleanup.json").read_bytes())["removed"])

    def test_functional_owned_child_failure_preserves_exit_and_streams_without_diagnostic_override(self) -> None:
        original = subprocess.Popen
        def failed_child(arguments, **kwargs):
            return original([sys.executable, "-c",
                             "import sys; print('owned-host-out'); "
                             "print('owned-child-failure',file=sys.stderr); sys.exit(7)"], **kwargs)
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-child-exit-") as directory:
            root = Path(directory)
            oracle = root / "VoicePromptNativeControllerFixture.exe"
            shutil.copyfile(ORACLE, oracle)
            evidence = root / "evidence"
            evidence.mkdir()
            with patch.object(capture.subprocess, "Popen", side_effect=failed_child), self.assertRaises(capture.ControlReadError):
                capture.run_functional(oracle, evidence, "owned-child-failure", 3)
            failure = json.loads((evidence / "failure.json").read_bytes())
            self.assertEqual(failure["native_exit_before_cleanup"], 7)
            self.assertFalse(failure["native_kill_requested_by_collector"])
            self.assertEqual(failure["native_exit_after_cleanup"], 7)
            self.assertIn("owned-host-out", failure["stdout"])
            self.assertIn("owned-child-failure", failure["stderr"])
            self.assertFalse((evidence / "functional-controller.json").exists())
            self.assertTrue(json.loads((evidence / "data-cleanup.json").read_bytes())["removed"])

    def test_functional_controller_has_no_sampler_and_exact_owned_cleanup(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-functional-smoke-") as directory:
            root = Path(directory)
            oracle = root / "VoicePromptNativeControllerFixture.exe"
            shutil.copyfile(ORACLE, oracle)
            evidence = root / "evidence"
            evidence.mkdir()
            with patch.object(capture, "sample", side_effect=AssertionError("No resource sampler")):
                capture.run_functional(oracle, evidence, "functional-smoke", 3, "native-fixture")
            report = json.loads((evidence / "functional-controller.json").read_bytes())
            self.assertEqual(report["status"], "pass")
            self.assertFalse(report["resource_sampling"])
            self.assertEqual(report["actual_capture"]["accepted_bytes"], 96000)
            self.assertEqual(report["actual_capture"]["confirmed_bytes"], 96000)
            self.assertEqual(report["native_source_revision"], "native-fixture")
            self.assertTrue(report["owned_control_directory_removed"])
            data = json.loads((evidence / "data-root.json").read_bytes())
            self.assertFalse(Path(data["root"]).is_relative_to(evidence))
            self.assertFalse(Path(data["root"]).exists())
            self.assertTrue((evidence / "control-state" / "normal-exit.json").exists())
            self.assertFalse((evidence / "before.json").exists())
            self.assertFalse((evidence / "capture.json").exists())
            self.assertFalse((evidence / "after.json").exists())

    def test_functional_zero_test_preserves_first_logs_state_before_cleanup(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-functional-zero-") as directory:
            root = Path(directory)
            oracle = root / "VoicePromptNativeControllerFixture.exe"
            shutil.copyfile(ORACLE, oracle)
            evidence = root / "evidence"
            evidence.mkdir()
            with patch.object(capture, "CONTROLLER_TEST", "missing_owned_controller"), self.assertRaises(RuntimeError):
                capture.run_functional(oracle, evidence, "functional-zero", 3)
            report = json.loads((evidence / "failure.json").read_bytes())
            self.assertIn("0 passed", report["stdout"])
            self.assertTrue((evidence / "failure-state" / "migration-fixture.json").exists())
            self.assertTrue((evidence / "state-hashes.json").exists())
            self.assertTrue(json.loads((evidence / "data-cleanup.json").read_bytes())["removed"])
            self.assertFalse((evidence / "functional-controller.json").exists())

    def test_controller_mode_rejects_an_engine_named_oracle_before_any_launch(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(
            prefix="VoicePrompt-controller-name-"
        ) as directory:
            with (
                patch.object(capture.subprocess, "Popen") as start,
                self.assertRaises(ValueError),
            ):
                capture.run(
                    ORACLE, Path(directory), "name-guard", 3, 3, controller=True
                )
            start.assert_not_called()

    def test_missing_controller_test_preserves_failure_and_never_claims_hour(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(
            prefix="VoicePrompt-controller-zero-"
        ) as directory:
            root = Path(directory)
            oracle = root.joinpath("VoicePromptNativeControllerFixture.exe")
            shutil.copyfile(ORACLE, oracle)
            evidence = root.joinpath("evidence")
            evidence.mkdir()
            with (
                patch.object(capture, "CONTROLLER_TEST", "missing_controller_fixture"),
                self.assertRaises(RuntimeError),
            ):
                capture.run(
                    oracle, evidence, "controller-zero-test", 3, 3, controller=True
                )
            report = json.loads(evidence.joinpath("failure.json").read_bytes())
            self.assertEqual(report["scope"], capture.CONTROLLER_SCOPE)
            self.assertEqual(report["status"], "failed")
            self.assertIn("0 passed", report["stdout"])
            self.assertFalse(
                evidence.joinpath("synthetic-controller-hour.json").exists()
            )
            self.assertTrue(
                evidence.joinpath("failure-state", "capture-options.json").exists()
            )

    def test_zero_tests_is_not_success_and_native_logs_and_state_are_preserved(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(
            prefix="VoicePrompt-zero-test-evidence-"
        ) as directory:
            root = Path(directory)
            with (
                patch.object(capture, "TEST", "missing_owned_fixture"),
                self.assertRaises(RuntimeError),
            ):
                capture.run(ORACLE, root, "synthetic-failure-test", 3, 3)
            report = json.loads(root.joinpath("failure.json").read_bytes())
            self.assertEqual(report["status"], "failed")
            self.assertFalse(report["whole_migration_acceptance"])
            self.assertIn("0 passed", report["stdout"])
            self.assertTrue(
                root.joinpath("failure-state", "capture-options.json").is_file()
            )
            self.assertFalse(root.joinpath("synthetic-engine-hour.json").exists())
            self.assertFalse(
                any(
                    path.name.startswith("VoicePrompt-hour-control-")
                    for path in root.iterdir()
                )
            )

    def test_real_child_pipe_backpressure_does_not_hide_stdout_or_stderr(self) -> None:
        original = subprocess.Popen

        def noisy_child(arguments: list[str], **kwargs: object) -> subprocess.Popen:
            return original(
                [
                    sys.executable,
                    "-c",
                    (
                        "import sys; print('synthetic-out-'*8192); "
                        "print('synthetic-error-'*8192,file=sys.stderr); sys.exit(7)"
                    ),
                ],
                **kwargs,
            )

        with tempfile.TemporaryDirectory(
            prefix="VoicePrompt-pipe-failure-"
        ) as directory:
            root = Path(directory)
            with (
                patch.object(capture.subprocess, "Popen", side_effect=noisy_child),
                self.assertRaises(RuntimeError),
            ):
                capture.run(ORACLE, root, "synthetic-pipe-test", 3, 3)
            report = json.loads(root.joinpath("failure.json").read_bytes())
            self.assertIn("synthetic-out-", report["stdout"])
            self.assertIn("synthetic-error-", report["stderr"])
            self.assertLessEqual(len(report["stdout"]), 200 * 16384)
            self.assertLessEqual(len(report["stderr"]), 200 * 16384)
            self.assertFalse(root.joinpath("synthetic-engine-hour.json").exists())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    args = parser.parse_args()
    ORACLE = args.oracle.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

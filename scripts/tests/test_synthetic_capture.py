"""Verify failed/zero-test native captures preserve bounded diagnostic evidence."""

from __future__ import annotations

import argparse
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

"""Desktop-only isolated native client lifecycle. No OAuth, network or clipboard."""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
import unittest
from datetime import UTC, datetime
from pathlib import Path

PILOT: Path


def run(*arguments: str) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [str(PILOT), *arguments], input=b"", capture_output=True, timeout=15, check=False
    )


class ClientLifecycleTests(unittest.TestCase):
    def test_missing_state_is_not_discovered_and_no_files_are_created(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-native-empty-") as directory:
            result = run(
                "--self-test", "--instance-key", "client-empty-fixture",
                "--state-directory", directory,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_synthetic_history_is_visible_without_touching_tokens_or_clipboard(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-native-history-") as directory:
            now = datetime.now(UTC).isoformat()
            path = Path(directory).joinpath("history.json")
            data = json.dumps([{
                "transcript_id": "fixture", "recording_id": "r", "preview": "synthetic",
                "body": "synthetic body", "raw_body": "synthetic original",
                "completed_at": now, "cached_at": now, "character_count": 14,
            }]).encode()
            path.write_bytes(data)
            result = run(
                "--self-test", "--instance-key", "client-history-fixture",
                "--state-directory", directory,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(path.read_bytes(), data)
            self.assertEqual(list(Path(directory).iterdir()), [path])

    def test_corrupt_state_fails_visibly_and_is_preserved(self) -> None:
        for name in ["settings.json", "history.json", "tokens.bin"]:
            with self.subTest(name=name), tempfile.TemporaryDirectory(
                prefix="VoicePrompt-native-corrupt-"
            ) as directory:
                path = Path(directory).joinpath(name)
                path.write_bytes(b"corrupt synthetic fixture")
                result = run(
                    "--self-test", "--instance-key", "client-corrupt-fixture",
                    "--state-directory", directory,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(result.stderr)
                self.assertEqual(path.read_bytes(), b"corrupt synthetic fixture")
                self.assertEqual(list(Path(directory).iterdir()), [path])

    def test_connected_mode_cannot_masquerade_as_foundation_measurement(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-native-options-") as directory:
            path = Path(directory).joinpath("ready.json")
            result = run(
                "--self-test", "--instance-key", "client-options-fixture",
                "--state-directory", directory, "--ready-file", str(path),
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(path.exists())
            result = run(
                "--self-test", "--oauth-config", str(Path(directory).joinpath("must-not-open.json"))
            )
            self.assertNotEqual(result.returncode, 0)

    def test_non_selftest_mode_really_loads_the_explicit_state(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-native-connected-") as directory:
            path = Path(directory).joinpath("history.json")
            path.write_bytes(b"corrupt synthetic fixture")
            result = run(
                "--instance-key", "client-normal-fixture", "--run-for-seconds", "1",
                "--state-directory", directory,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(result.stderr)
            self.assertEqual(path.read_bytes(), b"corrupt synthetic fixture")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pilot", type=Path, required=True)
    args = parser.parse_args()
    PILOT = args.pilot.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

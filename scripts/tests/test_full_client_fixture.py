"""Verify full client measurement isolation with synthetic state; no physical ports."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from measure_clients import FixtureProcess, fixture_files

BINARIES: dict[str, Path]


class ClientFixtureTests(unittest.TestCase):
    def test_both_actual_composition_roots_start_with_complete_runtime_and_only_marked_state(
        self,
    ) -> None:
        for implementation, binary in BINARIES.items():
            with (
                self.subTest(implementation=implementation),
                tempfile.TemporaryDirectory(
                    prefix="VoicePrompt-fixture-guards-"
                ) as directory,
            ):
                root = Path(directory)
                with FixtureProcess(binary, implementation, root, 5) as fixture:
                    recovery = (
                        "dictation-recovery"
                        if implementation == "csharp"
                        else "recovery"
                    )
                    self.assertTrue(fixture.state.joinpath(recovery).is_dir())
                    self.assertTrue(fixture.marker["full_components"])
                    self.assertFalse(fixture.marker["physical_ports_enabled"])
                    self.assertEqual(fixture.marker["history_count"], 5)
                self.assertEqual(list(root.iterdir()), [])

    def test_existing_tokens_and_wrong_marker_are_rejected_without_state_mutation(
        self,
    ) -> None:
        for implementation, binary in BINARIES.items():
            for mode in ["tokens", "marker", "marker_fields"]:
                with (
                    self.subTest(implementation=implementation, mode=mode),
                    tempfile.TemporaryDirectory(
                        prefix="VoicePrompt-fixture-refusal-"
                    ) as directory,
                ):
                    root = Path(directory)
                    state = root.joinpath("state")
                    key = uuid.uuid4().hex
                    fixture_files(state, key, 5)
                    if mode == "tokens":
                        state.joinpath("tokens.bin").write_bytes(
                            b"synthetic forbidden token cache"
                        )
                    elif mode == "marker":
                        state.joinpath("migration-fixture.json").write_text(
                            json.dumps(
                                {"scope": "not-a-measurement", "instance_key": key}
                            )
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
    parser.add_argument("--csharp", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True)
    args = parser.parse_args()
    BINARIES = {
        "csharp": args.csharp.resolve(strict=True),
        "rust": args.rust.resolve(strict=True),
    }
    unittest.main(argv=[__file__], verbosity=2)

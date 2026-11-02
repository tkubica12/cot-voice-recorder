"""Verify exact thresholds, owned HTTP processes and no partial performance reports."""

from __future__ import annotations

import argparse
import copy
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from measure_backend import OPERATIONS, Authority, Fixture, check, metrics, percentile, publish

ORACLE: Path


class BackendMeasurementTests(unittest.TestCase):
    def data(self) -> tuple:
        startup = {"python": [2000.0] * 30, "rust": [1000.0] * 30}
        samples = {
            name: [[dict.fromkeys(OPERATIONS, timing) for _ in range(100)] for _ in range(3)]
            for name, timing in [("python", 10.0), ("rust", 11.0)]
        }
        memory = {"python": [[200] * 10 for _ in range(3)], "rust": [[100] * 10 for _ in range(3)]}
        return startup, samples, memory

    def test_exact_thresholds_and_every_series_operation_are_evaluated(self) -> None:
        startup, samples, memory = self.data()
        self.assertTrue(all(metrics(startup, samples, memory, 30, 100)[3].values()))
        startup["rust"][28] = startup["rust"][29] = 1000.001
        self.assertFalse(metrics(startup, samples, memory, 30, 100)[3]["readiness"])
        memory["rust"][2] = [101] * 10
        self.assertFalse(metrics(startup, samples, memory, 30, 100)[3]["rss"])
        for row in samples["rust"][2]:
            row["upload"] = 11.001
        self.assertFalse(metrics(startup, samples, memory, 30, 100)[3]["api_p95"])
        self.assertEqual(percentile(list(range(1, 31))), 29)

    def test_missing_corrupt_or_zero_data_cannot_be_a_complete_profile(self) -> None:
        original = self.data()
        for mode in ["startup", "series", "flow", "operation", "rss", "nan", "zero"]:
            with self.subTest(mode=mode):
                startup, samples, memory = copy.deepcopy(original)
                if mode == "startup":
                    startup["rust"].pop()
                elif mode == "series":
                    samples["python"].pop()
                elif mode == "flow":
                    samples["python"][2].pop()
                elif mode == "operation":
                    samples["rust"][2][99].pop("archive")
                elif mode == "rss":
                    memory["rust"][2].pop()
                else:
                    startup["rust"][0] = float("nan") if mode == "nan" else 0
                with self.assertRaises(ValueError):
                    metrics(startup, samples, memory, 30, 100)

    def test_report_publication_never_overwrites_or_leaves_temporary_files(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-backend-profile-") as directory:
            target = Path(directory).joinpath("synthetic.json")
            publish(target, {"fixture": True})
            previous = target.read_bytes()
            with self.assertRaises(FileExistsError):
                publish(target, {"fixture": False})
            self.assertEqual(target.read_bytes(), previous)
            self.assertEqual(list(Path(directory).iterdir()), [target])

    def test_actual_http_factories_verify_signatures_and_run_the_same_flows(self) -> None:
        check(ORACLE)

    def test_normal_application_binary_names_are_never_started(self) -> None:
        authority = Authority()
        try:
            for name in ["VoicePrompt.exe", "VoicePromptRustPilot.exe", "voice-recorder"]:
                with self.subTest(name=name), self.assertRaises(ValueError):
                    Fixture("rust", Path(name), authority)
        finally:
            authority.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, required=True)
    args = parser.parse_args()
    ORACLE = args.rust.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

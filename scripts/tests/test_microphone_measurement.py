"""Pure synthetic telemetry tests only: never launch a microphone or device probe."""

from __future__ import annotations

import copy
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from measure_microphone import SCOPE, bundle, reading, summary


def payload(first: float = 100.0) -> dict:
    return {
        "scope": SCOPE,
        "open_ms": 150.0,
        "first_pcm_ms": first,
        "stop_ms": 20.0,
        "buffers": 6,
        "pcm_bytes_counted_then_zeroed": 6400,
        "saved_or_uploaded": False,
        "clipboard_or_keyboard": False,
        "full_migration_acceptance": False,
    }


class MicrophoneMeasurementTests(unittest.TestCase):
    def test_exact_native_and_csharp_stdout_shapes_have_one_typed_first_pcm_reading(
        self,
    ) -> None:
        for prefix in ["PASS count-only\n", "\nrunning 1 test\ntest manual ... "]:
            expected = payload()
            self.assertEqual(
                reading((prefix + json.dumps(expected) + "\nok\n").encode()), expected
            )
        with self.assertRaises(ValueError):
            reading((json.dumps(payload()) + "\n" + json.dumps(payload())).encode())

    def test_absence_invalid_numeric_flags_or_incomplete_pcm_is_not_readiness(
        self,
    ) -> None:
        for field, value in [
            ("first_pcm_ms", None),
            ("first_pcm_ms", float("nan")),
            ("open_ms", -1),
            ("stop_ms", 0),
            ("buffers", 0),
            ("pcm_bytes_counted_then_zeroed", 6399),
            ("saved_or_uploaded", True),
            ("clipboard_or_keyboard", True),
            ("full_migration_acceptance", True),
        ]:
            data = payload()
            data[field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                reading(json.dumps(data).encode())

    def test_every_trial_and_nearest_rank_p95_use_the_original_110_percent_threshold(
        self,
    ) -> None:
        rows = [
            {"implementation": name, "trial": trial, "reading": payload(first)}
            for name, first in [("csharp", 100.0), ("rust", 110.0)]
            for trial in range(1, 31)
        ]
        values, passes = summary(rows)
        self.assertTrue(passes)
        self.assertEqual(values["rust"]["first_pcm_ms"]["p95"], 110.0)
        for row in rows[-2:]:
            row["reading"]["first_pcm_ms"] = 110.001
        self.assertFalse(summary(rows)[1])
        for altered in [rows[:-1], [*rows, copy.deepcopy(rows[0])]]:
            with self.assertRaises(ValueError):
                summary(altered)
        rows[-1]["trial"] = 29
        with self.assertRaises(ValueError):
            summary(rows)

    def test_normal_application_binary_names_are_rejected_before_any_file_or_device_access(
        self,
    ) -> None:
        for implementation in ["csharp", "rust"]:
            with (
                self.subTest(implementation=implementation),
                self.assertRaises(ValueError),
            ):
                bundle(Path("VoicePrompt.exe"), implementation)


if __name__ == "__main__":
    unittest.main(verbosity=2)

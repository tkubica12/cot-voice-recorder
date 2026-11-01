"""Direct Rust audit -> actual Python service, envelope, digest and retrieval parity."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import subprocess
import sys
import unittest
from datetime import UTC, datetime, timedelta
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT.joinpath("backend", "src")))
sys.path.insert(0, str(ROOT.joinpath("backend", "tests")))
from conftest import FakeClock, context
from voice_recorder.ai.fakes import FakeRefiner, FakeTranscriber
from voice_recorder.config import Settings
from voice_recorder.models import DictationArchiveRequest, TranscriptSummary
from voice_recorder.problems import ProblemError
from voice_recorder.realtime.memory import InMemoryRealtimeGateway
from voice_recorder.services.dictation_archive import archive
from voice_recorder.services.transcripts import get_transcript
from voice_recorder.storage.memory import InMemoryBlobStore, InMemoryWorkQueue

ORACLE: Path


class ArchiveParityTests(unittest.TestCase):
    def test_exact_envelope_digest_preview_expiry_and_original_final_text(self) -> None:
        now = datetime.now(UTC)
        for text, completed in [
            ("synthetic final", now - timedelta(hours=1)),
            ("český text 😀\nsecond line\tend", now.replace(microsecond=100000)),
            (" first\x1csecond\x1fthird " * 20, now.astimezone()),
        ]:
            with self.subTest(text=text):
                settings = Settings(
                    environment="test", allowlisted_email="fixture@example.invalid"
                )
                blobs = InMemoryBlobStore()
                ctx = context.__wrapped__(
                    settings,
                    FakeClock(now),
                    FakeTranscriber(),
                    FakeRefiner(),
                    InMemoryRealtimeGateway(),
                    InMemoryWorkQueue(),
                    blobs,
                )
                data = DictationArchiveRequest(
                    text=text,
                    raw_text="synthetic original",
                    completed_at=completed,
                    polished=True,
                )
                session = "0123456789abcdef0123456789abcdef"
                expected = archive(ctx, session, data)
                result = subprocess.run(
                    [str(ORACLE), "archive"],
                    input=json.dumps(
                        {"id": session, "payload": data.model_dump(mode="json")}
                    ).encode(),
                    capture_output=True,
                    timeout=20,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                actual = json.loads(result.stdout)
                self.assertEqual(actual["status"], 200, actual)
                envelope = blobs.get(settings.transcript_container, expected.body_path)
                self.assertEqual(base64.b64decode(actual["envelope"]), envelope)
                self.assertEqual(actual["digest"], hashlib.sha256(envelope).hexdigest())
                self.assertEqual(actual["path"], expected.body_path)
                summary = TranscriptSummary.model_validate(actual["summary"])
                self.assertEqual(summary.preview, expected.preview)
                self.assertEqual(summary.completed_at, expected.completed_at)
                self.assertEqual(summary.expires_at, expected.expires_at)
                self.assertEqual(summary.source, expected.source)
                full = get_transcript(ctx, expected.transcript_id)
                for key in [
                    "body",
                    "raw_body",
                    "character_count",
                    "language",
                    "refine_model",
                ]:
                    self.assertEqual(actual["full"][key], getattr(full, key))

    def test_service_utf8_byte_bound_is_not_replaced_with_a_character_bound(
        self,
    ) -> None:
        data = DictationArchiveRequest(
            text="😀" * 262145,
            raw_text="synthetic original",
            completed_at=datetime.now(UTC),
        )
        settings = Settings(
            environment="test", allowlisted_email="fixture@example.invalid"
        )
        ctx = context.__wrapped__(
            settings,
            FakeClock(datetime.now(UTC)),
            FakeTranscriber(),
            FakeRefiner(),
            InMemoryRealtimeGateway(),
            InMemoryWorkQueue(),
            InMemoryBlobStore(),
        )
        with self.assertRaises(ProblemError):
            archive(ctx, "0123456789abcdef0123456789abcdef", data)
        result = subprocess.run(
            [str(ORACLE), "archive"],
            input=json.dumps(
                {
                    "id": "0123456789abcdef0123456789abcdef",
                    "payload": data.model_dump(mode="json"),
                }
            ).encode(),
            capture_output=True,
            timeout=20,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["status"], 422)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    args = parser.parse_args()
    ORACLE = args.oracle.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

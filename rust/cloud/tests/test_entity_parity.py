"""Actual Python Azure Table entity writers -> Rust domain, synthetic and local only."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import unittest
from dataclasses import asdict
from datetime import UTC, datetime, timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3].joinpath("backend", "src")))

from azure.data.tables._deserialize import _convert_to_entity
from voice_recorder.domain import (
    Chunk,
    ChunkState,
    Recording,
    RecordingState,
    Transcript,
)
from voice_recorder.repositories.tables import (
    _from_chunk_entity,
    _from_recording_entity,
    _from_transcript_entity,
    _to_chunk_entity,
    _to_recording_entity,
    _to_transcript_entity,
)

ORACLE: Path


def encode(value: object) -> bytes:
    return json.dumps(value, default=lambda value: value.isoformat()).encode()


class EntityParityTests(unittest.TestCase):
    def compare(self, mode: str, value: object, entity: dict[str, object]) -> None:
        result = subprocess.run(
            [str(ORACLE), mode],
            input=encode(entity),
            capture_output=True,
            timeout=15,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        actual = json.loads(result.stdout)
        expected = json.loads(encode(asdict(value)))
        expected.pop("etag", None)
        for name in [
            "created_at",
            "updated_at",
            "received_at",
            "started_at",
            "complete_requested_at",
            "stream_lease_expires_at",
            "completed_at",
            "expires_at",
        ]:
            if name in expected and expected[name] is not None:
                self.assertEqual(
                    datetime.fromisoformat(actual[name].replace("Z", "+00:00")),
                    datetime.fromisoformat(expected[name]),
                )
                expected[name] = actual[name]
        self.assertEqual(actual, expected)
        generated = subprocess.run(
            [str(ORACLE), f"encode-{mode}"],
            input=encode(asdict(value)),
            capture_output=True,
            timeout=15,
            check=False,
        )
        self.assertEqual(generated.returncode, 0, generated.stderr)
        wire = json.loads(generated.stdout)
        decoded = _convert_to_entity(wire)
        reader = {
            "recording": _from_recording_entity,
            "chunk": _from_chunk_entity,
            "transcript": _from_transcript_entity,
        }[mode]
        roundtrip = asdict(reader(decoded))
        original = asdict(value)
        roundtrip.pop("etag", None)
        original.pop("etag", None)
        self.assertEqual(roundtrip, original)

    def test_legacy_and_streaming_recording_fields_defaults_and_lease_state_are_preserved(
        self,
    ) -> None:
        now = datetime(2026, 11, 1, 10, 0, 0, 123456, tzinfo=UTC)
        for streaming in [False, True]:
            recording = Recording(
                recording_id="fixture",
                client_recording_id="client",
                state=RecordingState.RECORDING,
                refine_model="gpt-6-luna",
                language="cs",
                created_at=now,
                updated_at=now,
                transcription_mode="streaming" if streaming else "chunked",
                audio_layout="contiguous" if streaming else "legacy_overlap",
                refinement_enabled=not streaming,
                streamed_samples=2_700_000_000,
                stream_attempt=3,
                stream_preview="synthetic preview",
                stream_lease_id="synthetic-lease",
                stream_lease_expires_at=now + timedelta(seconds=120),
            )
            self.compare("recording", recording, _to_recording_entity(recording))

    def test_chunk_sample_and_source_metadata_roundtrip(self) -> None:
        now = datetime(2026, 11, 1, 10, 0, tzinfo=UTC)
        chunk = Chunk(
            recording_id="fixture",
            index=2,
            checksum="synthetic-digest",
            state=ChunkState.ACCEPTED,
            received_at=now,
            size_bytes=320044,
            blob_path="fixture/2-digest.wav",
            start_sample=320000,
            sample_count=160000,
            duration_ms=10000,
            overlap_ms=0,
        )
        self.compare("chunk", chunk, _to_chunk_entity(chunk))

    def test_transcript_audit_digest_and_expiry_are_preserved(self) -> None:
        now = datetime(2026, 11, 1, 10, 0, 0, 123456, tzinfo=UTC)
        transcript = Transcript(
            transcript_id="fixture",
            recording_id="fixture",
            preview="synthetic",
            language="auto",
            refine_model="none",
            completed_at=now,
            expires_at=now + timedelta(hours=48),
            character_count=9,
            body_path="windows-dictation/fixture/digest.json",
            source="windows_dictation",
            transcribe_model="MAI-Transcribe-2-Streaming",
            content_digest="synthetic-digest",
        )
        self.compare("transcript", transcript, _to_transcript_entity(transcript))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    args = parser.parse_args()
    ORACLE = args.oracle.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

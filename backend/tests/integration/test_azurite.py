"""Opt-in Azurite integration tests.

These exercise the real Azure Blob/Queue/Table adapters against a local Azurite
emulator. They are skipped unless ``VR_RUN_AZURITE=1`` and Azurite is reachable, so the
default unit-test run never depends on Docker.

Run Azurite first (see ``backend/README.md``), then:

    VR_RUN_AZURITE=1 uv run pytest -m azurite
"""

from __future__ import annotations

import os
import uuid

import pytest

from voice_recorder.config import AZURITE_DEV_CONNECTION_STRING, Settings

pytestmark = pytest.mark.azurite


def _azurite_available() -> bool:
    if os.environ.get("VR_RUN_AZURITE") != "1":
        return False
    import socket

    for port in (10000, 10001, 10002):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.5)
            if sock.connect_ex(("127.0.0.1", port)) != 0:
                return False
    return True


pytest.importorskip("azure.data.tables")

if not _azurite_available():
    pytest.skip(
        "Azurite not available (set VR_RUN_AZURITE=1 and start Azurite).",
        allow_module_level=True,
    )


def _settings() -> Settings:
    suffix = uuid.uuid4().hex[:12]
    return Settings(
        environment="local",
        storage_use_azurite=True,
        storage_azurite_connection_string=AZURITE_DEV_CONNECTION_STRING,
        use_fake_ai=True,
        use_fake_realtime=True,
        allowlisted_email="owner@example.com",
        # Unique names so parallel/repeated runs don't collide.
        audio_container=f"audio{suffix}",
        transcript_container=f"transcripts{suffix}",
        raw_container=f"raw{suffix}",
        work_queue=f"work{suffix}",
        recordings_table=f"recordings{suffix}",
        chunks_table=f"chunks{suffix}",
        transcripts_table=f"transcripts{suffix}",
    )


def test_full_pipeline_against_azurite() -> None:
    from voice_recorder.bootstrap import build_context
    from voice_recorder.clock import SystemClock
    from voice_recorder.digest import compute_content_digest
    from voice_recorder.domain import RecordingState
    from voice_recorder.services.context import audio_path
    from voice_recorder.services.recordings import (
        complete_recording,
        create_recording,
        get_recording,
        upload_chunk,
    )
    from voice_recorder.worker import QueueWorker

    from ..conftest import make_wav

    ctx = build_context(_settings(), clock=SystemClock())

    rec, created = create_recording(
        ctx, client_recording_id=str(uuid.uuid4()), refine_model="gpt-5.6-luna", language="cs"
    )
    assert created
    rid = rec.recording_id

    # Idempotent create returns the same recording.
    again, created_again = create_recording(
        ctx,
        client_recording_id=rec.client_recording_id,
        refine_model="gpt-5.6-luna",
        language="cs",
    )
    assert not created_again
    assert again.recording_id == rid

    data = make_wav()
    upload_chunk(
        ctx,
        recording_id=rid,
        index=0,
        data=data,
        checksum=compute_content_digest(data),
        duration_ms=30000,
        overlap_ms=1500,
        started_at=None,
    )
    complete_recording(ctx, recording_id=rid, chunk_count=1)

    worker = QueueWorker(ctx)
    for _ in range(50):
        if worker.run_once() == 0:
            break

    final = get_recording(ctx, rid)
    assert final.state == RecordingState.COMPLETED
    assert final.transcript_id is not None
    # Audio blob deleted after transcription.
    assert not ctx.blobs.exists(ctx.settings.audio_container, audio_path(rid, 0))
    # Transcript metadata persisted in Table storage.
    assert ctx.transcripts.get(final.transcript_id) is not None

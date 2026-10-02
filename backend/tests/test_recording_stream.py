from __future__ import annotations

import asyncio
import base64
import io
import json
import uuid
import wave
from contextlib import asynccontextmanager
from dataclasses import replace
from threading import Event

import pytest

from voice_recorder.digest import compute_content_digest, parse_content_digest
from voice_recorder.domain import RecordingState
from voice_recorder.errors import StreamingPending
from voice_recorder.problems import ChunkConflictError
from voice_recorder.services.pipeline import finalize
from voice_recorder.services.recording_stream import claim, transcribe_recording, update_owned
from voice_recorder.services.recordings import complete_recording, create_recording, upload_chunk
from voice_recorder.worker import QueueWorker


def wav(pcm: bytes) -> bytes:
    output = io.BytesIO()
    with wave.open(output, "wb") as writer:
        writer.setnchannels(1)
        writer.setsampwidth(2)
        writer.setframerate(16000)
        writer.writeframes(pcm)
    return output.getvalue()


class Stream:
    def __init__(self, fail=False, stop=None):
        self.events = asyncio.Queue()
        self.pcm = bytearray()
        self.fail = fail
        self.stop = stop

    async def send(self, message):
        data = json.loads(message)
        if data["type"] == "input_audio_buffer.append":
            if self.fail and len(self.pcm) > 640:
                raise OSError("Injected connection loss")
            self.pcm.extend(base64.b64decode(data["audio"]))
            if self.stop is not None:
                self.stop.set()
        else:
            await self.events.put(
                json.dumps(
                    {
                        "type": "conversation.item.input_audio_transcription.completed",
                        "transcript": "One contiguous transcript.",
                    }
                )
            )

    async def recv(self):
        return await self.events.get()


class Provider:
    def __init__(self, fail_first=False, stop=None):
        self.sessions = []
        self.fail_first = fail_first
        self.stop = stop

    @asynccontextmanager
    async def session(self, language):
        current = Stream(self.fail_first and not self.sessions, self.stop)
        self.sessions.append(current)
        yield current

    async def close(self):
        pass


def setup(context, provider, layout="contiguous", refine=False):
    settings = context.settings.model_copy(
        update={
            "recording_stream_poll_seconds": 0.005,
            "recording_stream_idle_seconds": 0.05,
            "recording_stream_heartbeat_seconds": 0.01,
            "recording_stream_replay_speed": 8.0,
        }
    )
    ctx = replace(context, settings=settings, streaming_provider=provider)
    rec, _ = create_recording(
        ctx,
        client_recording_id=str(uuid.uuid4()),
        refine_model="gpt-6-luna",
        language="cs",
        transcription_mode="streaming",
        audio_layout=layout,
        refinement_enabled=refine,
    )
    return ctx, rec.recording_id


def upload(ctx, rid, index, pcm, start=None):
    data = wav(pcm)
    upload_chunk(
        ctx,
        recording_id=rid,
        index=index,
        data=data,
        checksum=parse_content_digest(compute_content_digest(data)),
        duration_ms=len(pcm) * 1000 // 32000,
        overlap_ms=0 if start is not None else 1500,
        started_at=None,
        start_sample=start,
        sample_count=len(pcm) // 2,
    )


def stored_path(ctx, rid, index):
    return ctx.chunks.get(rid, index).blob_path


def test_contiguous_audio_has_no_headers_duplicates_or_seams(context):
    provider = Provider()
    ctx, rid = setup(context, provider)
    first = b"\x10\x01" * 160000
    tail = b"\x20\x02" * 1600
    upload(ctx, rid, 0, first, 0)
    upload(ctx, rid, 1, tail, 160000)
    complete_recording(ctx, recording_id=rid, chunk_count=2)
    asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    assert bytes(provider.sessions[0].pcm) == first + tail
    assert ctx.transcriber.calls == []
    assert ctx.blobs.exists(ctx.settings.audio_container, stored_path(ctx, rid, 0))
    finalize(ctx, rid)
    rec = ctx.recordings.get(rid)
    assert rec.state == RecordingState.COMPLETED
    assert ctx.refiner.calls == []
    meta = ctx.transcripts.get(rec.transcript_id)
    assert meta.refine_model == "none"
    assert (
        ctx.blobs.get(ctx.settings.transcript_container, meta.body_path).decode()
        == "One contiguous transcript."
    )
    assert not ctx.blobs.exists(ctx.settings.audio_container, stored_path(ctx, rid, 0))
    assert len(ctx.realtime.sent) == 1


def test_existing_overlap_windows_are_trimmed_by_exact_sample_positions(context):
    provider = Provider()
    ctx, rid = setup(context, provider, layout="legacy_overlap")
    first = b"\x10\x01" * 480000
    new = b"\x30\x03" * 1600
    second = first[-48000:] + new
    upload(ctx, rid, 0, first)
    upload(ctx, rid, 1, second)
    complete_recording(ctx, recording_id=rid, chunk_count=2)
    asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    assert bytes(provider.sessions[0].pcm) == first + new


def test_failure_keeps_all_audio_and_next_attempt_replays_from_zero(context, clock):
    provider = Provider(fail_first=True)
    ctx, rid = setup(context, provider)
    pcm = b"\x10\x01" * 1600
    upload(ctx, rid, 0, pcm, 0)
    complete_recording(ctx, recording_id=rid, chunk_count=1)
    with pytest.raises(OSError):
        asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    rec = ctx.recordings.get(rid)
    assert not rec.stream_asr_ready and rec.stream_lease_id is None
    assert ctx.blobs.exists(ctx.settings.audio_container, stored_path(ctx, rid, 0))
    assert not ctx.transcripts.list_page(limit=10, cursor=None)[0]
    asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    assert bytes(provider.sessions[1].pcm) == pcm
    assert ctx.recordings.get(rid).stream_attempt == 2


def test_one_lease_owns_recording_and_expired_owner_can_be_replaced(context, clock):
    provider = Provider()
    ctx, rid = setup(context, provider)
    assert claim(ctx, rid, "owner-one") is not None
    asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    assert provider.sessions == []
    clock.advance(seconds=121)
    assert claim(ctx, rid, "owner-two").stream_lease_id == "owner-two"
    with pytest.raises(StreamingPending):
        update_owned(ctx, rid, "owner-one", stream_asr_ready=True)


def test_idle_is_pending_not_failed_and_shutdown_retains_audio(context):
    provider = Provider()
    ctx, rid = setup(context, provider)
    with pytest.raises(StreamingPending):
        asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    assert ctx.recordings.get(rid).state != RecordingState.FAILED
    stop = Event()
    provider = Provider(stop=stop)
    ctx = replace(ctx, streaming_provider=provider)
    pcm = b"\x10\x01" * 1600
    upload(ctx, rid, 0, pcm, 0)
    with pytest.raises(StreamingPending):
        asyncio.run(transcribe_recording(ctx, rid, stop, lambda: None))
    assert ctx.blobs.exists(ctx.settings.audio_container, stored_path(ctx, rid, 0))
    assert not ctx.recordings.get(rid).stream_asr_ready


def test_worker_renews_receipt_and_removes_its_work_item(context):
    provider = Provider()
    ctx, rid = setup(context, provider)
    upload(ctx, rid, 0, b"\x10\x01" * 1600, 0)
    complete_recording(ctx, recording_id=rid, chunk_count=1)
    worker = QueueWorker(ctx)
    assert worker.run_once() == 1
    assert ctx.recordings.get(rid).stream_asr_ready
    for _ in range(8):
        worker.run_once()
    assert ctx.recordings.get(rid).state == RecordingState.COMPLETED
    assert len(ctx.realtime.sent) == 1
    assert not worker._stream_receipts


def test_missing_first_segment_does_not_insert_silence_or_skip_a_gap(context):
    provider = Provider()
    ctx, rid = setup(context, provider)
    pcm = b"\x10\x01" * 1600
    upload(ctx, rid, 1, pcm, 160000)
    with pytest.raises(StreamingPending):
        asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    assert provider.sessions[0].pcm == b""
    assert ctx.blobs.exists(ctx.settings.audio_container, stored_path(ctx, rid, 1))


def test_failed_raw_publication_preserves_audio_and_does_not_mark_asr_ready(context, monkeypatch):
    provider = Provider()
    ctx, rid = setup(context, provider)
    upload(ctx, rid, 0, b"\x10\x01" * 1600, 0)
    complete_recording(ctx, recording_id=rid, chunk_count=1)
    original_put = ctx.blobs.put

    def put(container, path, data, *, content_type):
        if path.startswith("stream/"):
            raise OSError("Injected publication failure")
        return original_put(container, path, data, content_type=content_type)

    monkeypatch.setattr(ctx.blobs, "put", put)
    with pytest.raises(OSError):
        asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    assert not ctx.recordings.get(rid).stream_asr_ready
    assert ctx.blobs.exists(ctx.settings.audio_container, stored_path(ctx, rid, 0))


def test_identical_upload_retry_repairs_failed_wakeup(context, monkeypatch):
    ctx, rid = setup(context, Provider())
    original_send = ctx.queue.send

    def unavailable(*args, **kwargs):
        raise OSError("Injected queue outage")

    monkeypatch.setattr(ctx.queue, "send", unavailable)
    pcm = b"\x10\x01" * 1600
    with pytest.raises(OSError):
        upload(ctx, rid, 0, pcm, 0)
    assert ctx.blobs.exists(ctx.settings.audio_container, stored_path(ctx, rid, 0))
    monkeypatch.setattr(ctx.queue, "send", original_send)
    upload(ctx, rid, 0, pcm, 0)
    assert ctx.queue.receive(max_messages=1, visibility_seconds=30)[0].content["type"] == "stream"


def test_racing_conflicting_upload_cannot_overwrite_winning_audio(context, monkeypatch):
    ctx, rid = setup(context, Provider())
    first = b"\x10\x01" * 1600
    upload(ctx, rid, 0, first, 0)
    path = stored_path(ctx, rid, 0)
    original_get = ctx.chunks.get
    reads = 0

    def stale_get(recording_id, index):
        nonlocal reads
        reads += 1
        return None if reads == 1 else original_get(recording_id, index)

    monkeypatch.setattr(ctx.chunks, "get", stale_get)
    with pytest.raises(ChunkConflictError):
        upload(ctx, rid, 0, b"\x20\x02" * 1600, 0)
    assert ctx.chunks.get(rid, 0).blob_path == path
    assert ctx.blobs.get(ctx.settings.audio_container, path) == wav(first)


def test_notification_failure_retries_same_canonical_transcript_without_losing_audio(
    context, monkeypatch
):
    ctx, rid = setup(context, Provider())
    upload(ctx, rid, 0, b"\x10\x01" * 1600, 0)
    complete_recording(ctx, recording_id=rid, chunk_count=1)
    asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    original_notify = ctx.realtime.notify_completed

    def unavailable(*args):
        raise OSError("Injected notification outage")

    monkeypatch.setattr(ctx.realtime, "notify_completed", unavailable)
    with pytest.raises(OSError):
        finalize(ctx, rid)
    rec = ctx.recordings.get(rid)
    transcript = ctx.transcripts.get(rec.transcript_id)
    assert rec.state == RecordingState.COMPLETED
    assert ctx.blobs.exists(ctx.settings.audio_container, stored_path(ctx, rid, 0))
    monkeypatch.setattr(ctx.realtime, "notify_completed", original_notify)
    finalize(ctx, rid)
    assert ctx.transcripts.get(rec.transcript_id) == transcript
    assert not ctx.blobs.exists(ctx.settings.audio_container, stored_path(ctx, rid, 0))
    assert len(ctx.realtime.sent) == 1
    assert ctx.refiner.calls == []
    finalize(ctx, rid)
    assert len(ctx.realtime.sent) == 1
    assert ctx.recordings.get(rid).stream_delivery_completed


def test_latest_preview_and_submitted_audio_are_flushed_while_waiting_for_upload(context):
    class HypothesisStream(Stream):
        async def send(self, message):
            await super().send(message)
            text = "Starting hypothesis" if len(self.pcm) == 640 else "Latest available hypothesis"
            await self.events.put(
                json.dumps(
                    {
                        "type": "conversation.item.input_audio_transcription.intermediate",
                        "intermediate": text,
                    }
                )
            )

    class HypothesisProvider(Provider):
        @asynccontextmanager
        async def session(self, language):
            current = HypothesisStream()
            self.sessions.append(current)
            yield current

    ctx, rid = setup(context, HypothesisProvider())
    ctx = replace(
        ctx,
        settings=ctx.settings.model_copy(
            update={
                "recording_stream_idle_seconds": 5.5,
                "recording_stream_heartbeat_seconds": 20,
            }
        ),
    )
    upload(ctx, rid, 0, b"\x10\x01" * 1600, 0)
    with pytest.raises(StreamingPending):
        asyncio.run(transcribe_recording(ctx, rid, Event(), lambda: None))
    rec = ctx.recordings.get(rid)
    assert rec.streamed_samples == 1600
    assert rec.stream_preview == "Latest available hypothesis"

"""One leased model session over durable ordered audio; failed attempts replay from zero."""

from __future__ import annotations

import asyncio
import base64
import io
import json
import time
import uuid
import wave
from collections.abc import Callable
from datetime import timedelta
from threading import Event
from typing import Any

from ..ai.streaming import StreamingProvider
from ..digest import verify_content_digest
from ..domain import Recording, RecordingState
from ..errors import ConcurrencyConflict, StreamingPending, TerminalError, TransientError
from ..text import make_preview
from ..wav import validate_wav
from .context import ServiceContext, audio_path
from .finalize import try_enqueue_finalize

PREFIX = "conversation.item.input_audio_transcription."
MAX_TEXT_BYTES = 16 * 1024 * 1024


async def persisted[T](function: Callable[..., T], *args: object, **kwargs: Any) -> T:
    work = asyncio.create_task(asyncio.to_thread(function, *args, **kwargs))
    try:
        return await asyncio.shield(work)
    except asyncio.CancelledError:
        await work
        raise


def claim(ctx: ServiceContext, rid: str, owner: str) -> Recording | None:
    for _ in range(8):
        rec = ctx.recordings.get(rid)
        if rec is None or rec.state in {RecordingState.COMPLETED, RecordingState.FAILED}:
            return None
        if rec.stream_asr_ready:
            try_enqueue_finalize(ctx, rid)
            return None
        now = ctx.clock.now()
        if (
            rec.stream_lease_id
            and rec.stream_lease_expires_at
            and rec.stream_lease_expires_at > now
        ):
            return None
        try:
            return ctx.recordings.update(
                rec.with_changes(
                    stream_lease_id=owner,
                    stream_lease_expires_at=now
                    + timedelta(seconds=ctx.settings.recording_stream_lease_seconds),
                    stream_attempt=rec.stream_attempt + 1,
                    streamed_samples=0,
                    stream_preview="",
                    stream_error=None,
                    updated_at=now,
                )
            )
        except ConcurrencyConflict:
            continue
    raise StreamingPending("Recording lease is contended")


def update_owned(ctx: ServiceContext, rid: str, owner: str, **changes: object) -> Recording:
    for _ in range(8):
        rec = ctx.recordings.get(rid)
        now = ctx.clock.now()
        if (
            rec is None
            or rec.stream_lease_id != owner
            or rec.state
            in {
                RecordingState.COMPLETED,
                RecordingState.FAILED,
            }
        ):
            raise StreamingPending("Recording lease was lost")
        if rec.stream_lease_expires_at is None or rec.stream_lease_expires_at <= now:
            raise StreamingPending("Recording lease expired")
        try:
            return ctx.recordings.update(rec.with_changes(updated_at=now, **changes))
        except ConcurrencyConflict:
            continue
    raise StreamingPending("Recording lease update is contended")


def release(ctx: ServiceContext, rid: str, owner: str, error: str | None) -> None:
    for _ in range(8):
        rec = ctx.recordings.get(rid)
        if rec is None or rec.stream_lease_id != owner:
            return
        if rec.stream_asr_ready:
            error = None
        try:
            ctx.recordings.update(
                rec.with_changes(
                    stream_lease_id=None,
                    stream_lease_expires_at=None,
                    stream_error=error,
                    updated_at=ctx.clock.now(),
                )
            )
            return
        except ConcurrencyConflict:
            continue
    raise TransientError("Could not release recording lease")


def pcm_for_chunk(ctx: ServiceContext, rid: str, index: int, cursor: int) -> bytes | None:
    chunk = ctx.chunks.get(rid, index)
    if chunk is None:
        return None
    audio = ctx.blobs.get(ctx.settings.audio_container, chunk.blob_path or audio_path(rid, index))
    validate_wav(audio, sample_rate=16000, channels=1, bits_per_sample=16, strict=True)
    if not verify_content_digest(audio, chunk.checksum):
        raise TerminalError("Stored audio checksum mismatch")
    with wave.open(io.BytesIO(audio), "rb") as wav:
        pcm = wav.readframes(wav.getnframes())
    if chunk.start_sample is None or chunk.sample_count != len(pcm) // 2:
        raise TerminalError("Missing or inconsistent sample metadata")
    skip = cursor - chunk.start_sample
    if skip < 0 or skip >= len(pcm) // 2:
        raise TerminalError("Gap or redundant audio range")
    # Discard only proven duplicate source samples, never infer overlap from recognized words.
    return pcm[skip * 2 :]


async def transcribe_recording(
    ctx: ServiceContext,
    rid: str,
    stop: Event,
    renew_queue: Callable[[], None],
    *,
    provider: StreamingProvider | None = None,
) -> None:
    provider = provider or ctx.streaming_provider
    if provider is None:
        raise TransientError("Streaming provider is not configured")
    owner = uuid.uuid4().hex
    rec = await asyncio.to_thread(claim, ctx, rid, owner)
    if rec is None:
        return
    error: str | None = "interrupted"
    try:
        async with asyncio.timeout(ctx.settings.recording_stream_session_seconds):
            async with provider.session(rec.language) as stream:
                cursor = 0
                index = 0
                deltas = ""
                partial = ""
                final: str | None = None
                last_progress = 0.0
                eof_sent = False

                async def publish_progress() -> None:
                    nonlocal last_progress
                    now = time.monotonic()
                    if now - last_progress < 5:
                        return
                    last_progress = now
                    await persisted(
                        update_owned,
                        ctx,
                        rid,
                        owner,
                        streamed_samples=cursor,
                        stream_preview=make_preview((deltas + partial)[-140:], max_chars=140),
                    )

                async def receive() -> None:
                    nonlocal deltas, partial, final
                    while True:
                        event = json.loads(await stream.recv())
                        kind = event.get("type")
                        if kind in {"error", PREFIX + "failed"}:
                            raise TransientError("MAI stream returned a provider error")
                        if kind == PREFIX + "delta":
                            text = event.get("delta")
                            if not isinstance(text, str):
                                raise TransientError("Invalid MAI delta")
                            deltas += text
                            partial = ""
                        elif kind == PREFIX + "intermediate":
                            text = event.get("intermediate")
                            if not isinstance(text, str):
                                raise TransientError("Invalid MAI hypothesis")
                            partial = text
                        elif kind == PREFIX + "completed":
                            if not eof_sent or not isinstance(event.get("transcript"), str):
                                raise TransientError("Unexpected MAI completion")
                            final = event["transcript"]
                            if len(final.encode("utf-8")) > MAX_TEXT_BYTES:
                                raise TerminalError("Streaming transcript exceeds supported bounds")
                            return
                        if len((deltas + partial).encode("utf-8")) > MAX_TEXT_BYTES:
                            raise TerminalError("Streaming hypothesis exceeds supported bounds")
                        await publish_progress()

                async def send() -> None:
                    nonlocal cursor, index, eof_sent
                    waiting_since = time.monotonic()
                    while True:
                        if stop.is_set():
                            raise StreamingPending("Worker shutdown")
                        latest = await asyncio.to_thread(ctx.recordings.get, rid)
                        if latest is None or latest.stream_lease_id != owner:
                            raise StreamingPending("Recording ownership changed")
                        if ctx.clock.now() - latest.created_at >= timedelta(
                            hours=ctx.settings.retention_hours
                        ):
                            raise TerminalError("Streaming recording exceeded retention")
                        if (
                            latest.expected_chunk_count is not None
                            and index == latest.expected_chunk_count
                        ):
                            if not cursor:
                                raise TerminalError("Empty streaming recording")
                            eof_sent = True
                            await stream.send(json.dumps({"type": "input_audio_buffer.commit"}))
                            return
                        pcm = await asyncio.to_thread(pcm_for_chunk, ctx, rid, index, cursor)
                        if pcm is None:
                            await publish_progress()
                            if (
                                time.monotonic() - waiting_since
                                >= ctx.settings.recording_stream_idle_seconds
                            ):
                                raise StreamingPending("Waiting for durable audio")
                            await asyncio.sleep(ctx.settings.recording_stream_poll_seconds)
                            continue
                        waiting_since = time.monotonic()
                        began = time.monotonic()
                        for offset in range(0, len(pcm), 640):
                            if stop.is_set():
                                raise StreamingPending("Worker shutdown")
                            packet = pcm[offset : offset + 640]
                            await stream.send(
                                json.dumps(
                                    {
                                        "type": "input_audio_buffer.append",
                                        "audio": base64.b64encode(packet).decode("ascii"),
                                    }
                                )
                            )
                            cursor += len(packet) // 2
                            target = (
                                (offset + len(packet))
                                / 32000
                                / ctx.settings.recording_stream_replay_speed
                            )
                            await asyncio.sleep(max(0, target - (time.monotonic() - began)))
                        index += 1
                        waiting_since = time.monotonic()
                        await publish_progress()

                async def heartbeat() -> None:
                    while True:
                        if stop.is_set():
                            raise StreamingPending("Worker shutdown")
                        await persisted(
                            update_owned,
                            ctx,
                            rid,
                            owner,
                            stream_lease_expires_at=ctx.clock.now()
                            + timedelta(seconds=ctx.settings.recording_stream_lease_seconds),
                        )
                        await persisted(renew_queue)
                        await asyncio.sleep(ctx.settings.recording_stream_heartbeat_seconds)

                sender = asyncio.create_task(send())
                receiver = asyncio.create_task(receive())
                keeper = asyncio.create_task(heartbeat())
                try:
                    done, _ = await asyncio.wait(
                        (sender, receiver, keeper), return_when=asyncio.FIRST_COMPLETED
                    )
                    for task in done:
                        await task
                    if sender not in done and not eof_sent:
                        raise TransientError("Stream ended before all source audio was sent")
                    await sender
                    done, _ = await asyncio.wait(
                        (receiver, keeper), timeout=45, return_when=asyncio.FIRST_COMPLETED
                    )
                    for task in done:
                        await task
                    if receiver not in done or final is None:
                        raise TransientError("MAI finalization deadline exceeded")
                finally:
                    for task in (sender, receiver, keeper):
                        task.cancel()
                    await asyncio.gather(sender, receiver, keeper, return_exceptions=True)
        path = f"stream/{rid}/{owner}.txt"
        await asyncio.to_thread(
            ctx.blobs.put,
            ctx.settings.raw_container,
            path,
            final.encode("utf-8"),
            content_type="text/plain; charset=utf-8",
        )
        await asyncio.to_thread(
            update_owned,
            ctx,
            rid,
            owner,
            stream_asr_ready=True,
            stream_raw_path=path,
            streamed_samples=cursor,
            stream_preview=make_preview(final, max_chars=140),
            stream_error=None,
        )
        error = None
        await asyncio.to_thread(try_enqueue_finalize, ctx, rid)
    except TimeoutError as exc:
        error = "session_rotated"
        raise StreamingPending("Model session rotation; full durable replay scheduled") from exc
    except StreamingPending:
        error = "awaiting_audio_or_restart"
        raise
    except Exception:
        error = "stream_failed"
        raise
    finally:
        await asyncio.to_thread(release, ctx, rid, owner, error)

"""Regression tests for finalize enqueue durability (queue send failures + replays)."""

from __future__ import annotations

from dataclasses import replace
from typing import Any

import pytest

from voice_recorder.digest import compute_content_digest
from voice_recorder.domain import RecordingState
from voice_recorder.services.context import ServiceContext
from voice_recorder.services.finalize import arm_finalize, try_enqueue_finalize
from voice_recorder.services.messages import FINALIZE
from voice_recorder.services.pipeline import finalize
from voice_recorder.services.recordings import (
    complete_recording,
    create_recording,
    get_recording,
    upload_chunk,
)
from voice_recorder.storage.memory import InMemoryWorkQueue
from voice_recorder.storage.protocols import QueueMessage

from .conftest import drain, unique_wav


class _SendFailingQueue:
    """In-memory queue whose ``send`` fails for the first N finalize messages."""

    def __init__(self, inner: InMemoryWorkQueue, *, finalize_send_failures: int = 0) -> None:
        self._inner = inner
        self.finalize_send_failures = finalize_send_failures
        self.finalize_sends = 0

    def send(self, message: dict[str, Any], *, delay_seconds: int = 0) -> None:
        if message.get("type") == FINALIZE:
            self.finalize_sends += 1
            if self.finalize_send_failures > 0:
                self.finalize_send_failures -= 1
                raise RuntimeError("queue send failed")
        self._inner.send(message, delay_seconds=delay_seconds)

    def receive(self, *, max_messages: int = 1, visibility_seconds: int = 60) -> list[QueueMessage]:
        return self._inner.receive(max_messages=max_messages, visibility_seconds=visibility_seconds)

    def delete(self, message: QueueMessage) -> None:
        self._inner.delete(message)

    def renew(self, message: QueueMessage, *, visibility_seconds: int) -> QueueMessage:
        return self._inner.renew(message, visibility_seconds=visibility_seconds)

    def depth(self) -> int:
        return self._inner.depth()


def _immediate_retry(context: ServiceContext) -> ServiceContext:
    """Same context with zero retry backoff so ``drain`` observes redeliveries."""
    return replace(
        context,
        settings=context.settings.model_copy(
            update={"queue_visibility_seconds": 0, "queue_retry_base_seconds": 0}
        ),
    )


def _with_failing_queue(
    context: ServiceContext, failures: int
) -> tuple[ServiceContext, _SendFailingQueue]:
    queue = _SendFailingQueue(context.queue, finalize_send_failures=failures)  # type: ignore[arg-type]
    return replace(context, queue=queue), queue  # type: ignore[arg-type]


def _seed(context: ServiceContext, *, chunks: int = 1) -> str:
    rec, _ = create_recording(
        context, client_recording_id="crid-fin", refine_model="gpt-5.6-luna", language="cs"
    )
    rid = rec.recording_id
    for index in range(chunks):
        data = unique_wav(index)
        upload_chunk(
            context,
            recording_id=rid,
            index=index,
            data=data,
            checksum=compute_content_digest(data),
            duration_ms=30000,
            overlap_ms=1500,
            started_at=None,
        )
    return rid


def test_failed_finalize_send_rolls_back_the_enqueued_flag(context: ServiceContext) -> None:
    rid = _seed(context)
    drain(context)  # transcribe the chunk (no completion requested yet)
    failing, _queue = _with_failing_queue(context, failures=1)

    # Declaring the count would normally enqueue finalize immediately.
    with pytest.raises(RuntimeError):
        complete_recording(failing, recording_id=rid, chunk_count=1)

    recording = get_recording(context, rid)
    assert recording.expected_chunk_count == 1, "completion intent must be persisted"
    assert not recording.finalize_enqueued, "reservation must be rolled back after a failed send"
    assert recording.state not in {RecordingState.COMPLETED, RecordingState.FAILED}


def test_complete_retry_after_queue_failure_eventually_completes(
    context: ServiceContext,
) -> None:
    rid = _seed(context)
    drain(context)
    failing, queue = _with_failing_queue(context, failures=1)

    with pytest.raises(RuntimeError):
        complete_recording(failing, recording_id=rid, chunk_count=1)
    assert get_recording(context, rid).state != RecordingState.COMPLETED

    # The client retries the idempotent POST /complete; the replay must repair the
    # missing finalize work instead of returning early.
    _rec, status = complete_recording(failing, recording_id=rid, chunk_count=1)
    assert status == 200
    assert queue.finalize_sends == 2
    drain(context)

    final = get_recording(context, rid)
    assert final.state == RecordingState.COMPLETED
    assert final.transcript_id is not None


def test_transcribe_retry_after_queue_failure_eventually_completes(
    context: ServiceContext,
) -> None:
    """The worker path (not just the API) must also recover from a failed send."""
    ctx = _immediate_retry(context)
    rid = _seed(ctx)
    complete_recording(ctx, recording_id=rid, chunk_count=1)
    failing, _queue = _with_failing_queue(ctx, failures=1)

    # Transcription succeeds but the finalize enqueue fails, so the transcribe message is
    # retried rather than silently lost; the retry re-arms finalize and completes.
    drain(failing)

    final = get_recording(ctx, rid)
    assert final.state == RecordingState.COMPLETED
    assert final.transcript_id is not None


def test_duplicate_finalize_messages_are_safe(context: ServiceContext) -> None:
    rid = _seed(context)
    drain(context)
    complete_recording(context, recording_id=rid, chunk_count=1)
    # Force extra finalize messages onto the queue (what a send-first design or a
    # repeated complete replay would produce).
    for _ in range(3):
        arm_finalize(context, rid)
    drain(context)

    final = get_recording(context, rid)
    assert final.state == RecordingState.COMPLETED
    transcripts, _cursor = context.transcripts.list_page(limit=10, cursor=None)
    assert len(transcripts) == 1
    assert len(context.realtime.sent) == 1  # type: ignore[attr-defined]


def test_complete_replay_rearms_watchdog_when_chunks_are_missing(
    context: ServiceContext,
) -> None:
    rid = _seed(context)
    complete_recording(context, recording_id=rid, chunk_count=2)  # second chunk never arrives
    drain(context)

    before = context.queue.depth()  # type: ignore[attr-defined]
    _rec, status = complete_recording(context, recording_id=rid, chunk_count=2)
    assert status == 200
    assert context.queue.depth() == before + 1, "replay must re-arm the watchdog"  # type: ignore[attr-defined]

    # The watchdog fires after the grace window and fails the stranded recording.
    context.clock.advance(seconds=context.settings.chunk_grace_seconds + 1)  # type: ignore[attr-defined]
    finalize(context, rid)
    assert get_recording(context, rid).state == RecordingState.FAILED


def test_replay_on_a_completed_recording_enqueues_nothing(context: ServiceContext) -> None:
    rid = _seed(context)
    complete_recording(context, recording_id=rid, chunk_count=1)
    drain(context)
    assert get_recording(context, rid).state == RecordingState.COMPLETED

    depth_before = context.queue.depth()  # type: ignore[attr-defined]
    _rec, status = complete_recording(context, recording_id=rid, chunk_count=1)
    assert status == 200
    assert context.queue.depth() == depth_before  # type: ignore[attr-defined]


def test_try_enqueue_finalize_is_still_exactly_once_under_concurrency(
    context: ServiceContext,
) -> None:
    rid = _seed(context)
    drain(context)
    complete_recording(context, recording_id=rid, chunk_count=1)
    # The completion already enqueued finalize; a second trigger must not enqueue again.
    assert try_enqueue_finalize(context, rid) is False

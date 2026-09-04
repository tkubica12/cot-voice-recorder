import threading
import time
from dataclasses import replace

import pytest

from voice_recorder.ai.fakes import FakeRefiner
from voice_recorder.ai.protocols import (
    TerminalTranscriptionError,
    TranscriptionError,
)
from voice_recorder.config import Settings
from voice_recorder.digest import compute_content_digest
from voice_recorder.domain import FailureReason, RecordingState
from voice_recorder.errors import QueueMessageGone, TransientError
from voice_recorder.realtime.memory import InMemoryRealtimeGateway
from voice_recorder.repositories.memory import (
    InMemoryChunkRepository,
    InMemoryRecordingRepository,
    InMemoryTranscriptRepository,
)
from voice_recorder.services.context import ServiceContext
from voice_recorder.services.messages import TRANSCRIBE
from voice_recorder.services.pipeline import finalize
from voice_recorder.services.recordings import (
    complete_recording,
    create_recording,
    get_recording,
    upload_chunk,
)
from voice_recorder.storage.memory import InMemoryBlobStore, InMemoryWorkQueue
from voice_recorder.storage.protocols import QueueMessage
from voice_recorder.worker import QueueWorker, _backoff_seconds

from .conftest import FakeClock, unique_wav


class _RaisingTranscriber:
    def __init__(self, exc: Exception) -> None:
        self._exc = exc
        self.attempts = 0

    def transcribe(self, audio: bytes, *, language: str, prompt: str) -> str:
        self.attempts += 1
        raise self._exc


def _build(transcriber: object, **overrides: object) -> ServiceContext:
    options: dict[str, object] = {
        "environment": "test",
        "queue_visibility_seconds": 0,
        "queue_retry_base_seconds": 0,
        "queue_receive_error_backoff_seconds": 0.0,
        "queue_receive_error_max_backoff_seconds": 0.0,
        "max_transcribe_attempts": 3,
        "max_refine_attempts": 3,
    }
    options.update(overrides)
    settings = Settings(**options)  # type: ignore[arg-type]
    return ServiceContext(
        settings=settings,
        clock=FakeClock(),
        recordings=InMemoryRecordingRepository(),
        chunks=InMemoryChunkRepository(),
        transcripts=InMemoryTranscriptRepository(),
        blobs=InMemoryBlobStore(),
        queue=InMemoryWorkQueue(),
        transcriber=transcriber,  # type: ignore[arg-type]
        refiner=FakeRefiner(),
        realtime=InMemoryRealtimeGateway(),
    )


def _seed_one_chunk(ctx: ServiceContext) -> str:
    rec, _ = create_recording(
        ctx, client_recording_id="crid-w", refine_model="gpt-5.6-luna", language="cs"
    )
    rid = rec.recording_id
    data = unique_wav(0)
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
    return rid


def test_backoff_is_bounded_and_exponential() -> None:
    assert _backoff_seconds(1, base=5) == 5
    assert _backoff_seconds(2, base=5) == 10
    assert _backoff_seconds(3, base=5) == 20
    assert _backoff_seconds(100, base=5) == 600  # capped


def test_transient_transcription_retries_then_fails() -> None:
    transcriber = _RaisingTranscriber(TranscriptionError("throttled"))
    ctx = _build(transcriber)
    rid = _seed_one_chunk(ctx)
    worker = QueueWorker(ctx)

    for _ in range(20):
        if worker.run_once() == 0:
            break

    assert transcriber.attempts >= 3  # retried up to the bound
    rec = get_recording(ctx, rid)
    assert rec.state == RecordingState.FAILED
    assert rec.failure_reason == FailureReason.TRANSCRIPTION_FAILED
    # Audio deleted on terminal chunk failure.
    from voice_recorder.services.context import audio_path

    assert not ctx.blobs.exists(ctx.settings.audio_container, audio_path(rid, 0))


def test_terminal_transcription_fails_immediately() -> None:
    transcriber = _RaisingTranscriber(TerminalTranscriptionError("bad audio"))
    ctx = _build(transcriber)
    rid = _seed_one_chunk(ctx)
    worker = QueueWorker(ctx)
    worker.run_once()

    assert transcriber.attempts == 1
    rec = get_recording(ctx, rid)
    assert rec.state == RecordingState.FAILED
    assert rec.failure_reason == FailureReason.TRANSCRIPTION_FAILED


def test_unknown_message_type_is_terminal() -> None:
    ctx = _build(_RaisingTranscriber(TranscriptionError("x")))
    ctx.queue.send({"type": "bogus", "recording_id": "r"})
    worker = QueueWorker(ctx)
    worker.run_once()
    assert ctx.queue.depth() == 0  # type: ignore[attr-defined]


def test_finalize_missing_chunks_after_grace() -> None:
    from voice_recorder.ai.fakes import FakeTranscriber

    ctx = _build(FakeTranscriber())
    rec, _ = create_recording(
        ctx, client_recording_id="crid-miss", refine_model="gpt-5.6-luna", language="cs"
    )
    rid = rec.recording_id
    data = unique_wav(0)
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
    # Declare two chunks but only one will ever arrive.
    complete_recording(ctx, recording_id=rid, chunk_count=2)
    # Transcribe the single received chunk.
    worker = QueueWorker(ctx)
    worker.run_once()

    # Before grace: finalize should signal a transient wait.
    with pytest.raises(TransientError):
        finalize(ctx, rid)

    # After grace: finalize fails with missing_chunks.
    ctx.clock.advance(seconds=ctx.settings.chunk_grace_seconds + 1)  # type: ignore[attr-defined]
    finalize(ctx, rid)
    failed = get_recording(ctx, rid)
    assert failed.state == RecordingState.FAILED
    assert failed.failure_reason == FailureReason.MISSING_CHUNKS


# --------------------------------------------------------------- queue-level faults
class _FaultyQueue:
    """Wraps the in-memory queue and injects failures on receive/delete/renew."""

    def __init__(
        self,
        inner: InMemoryWorkQueue,
        *,
        receive_failures: int = 0,
        delete_failures: int = 0,
        renew_failures: int = 0,
        receive_error: type[Exception] | Exception = TransientError("queue receive failed"),
        delete_error: type[Exception] | Exception = TransientError("queue delete failed"),
        renew_error: type[Exception] | Exception = QueueMessageGone("stale pop receipt"),
    ) -> None:
        self._inner = inner
        self.receive_failures = receive_failures
        self.delete_failures = delete_failures
        self.renew_failures = renew_failures
        self._receive_error = receive_error
        self._delete_error = delete_error
        self._renew_error = renew_error
        self.receive_calls: list[tuple[int, int]] = []
        self.delete_calls = 0
        self.renew_calls = 0

    @staticmethod
    def _raise(err: type[Exception] | Exception) -> None:
        raise err() if isinstance(err, type) else err

    def send(self, message: dict[str, object], *, delay_seconds: int = 0) -> None:
        self._inner.send(message, delay_seconds=delay_seconds)  # type: ignore[arg-type]

    def receive(self, *, max_messages: int = 1, visibility_seconds: int = 60) -> list[QueueMessage]:
        self.receive_calls.append((max_messages, visibility_seconds))
        if self.receive_failures > 0:
            self.receive_failures -= 1
            self._raise(self._receive_error)
        return self._inner.receive(max_messages=max_messages, visibility_seconds=visibility_seconds)

    def delete(self, message: QueueMessage) -> None:
        self.delete_calls += 1
        if self.delete_failures > 0:
            self.delete_failures -= 1
            self._raise(self._delete_error)
        self._inner.delete(message)

    def renew(self, message: QueueMessage, *, visibility_seconds: int) -> QueueMessage:
        self.renew_calls += 1
        if self.renew_failures > 0:
            self.renew_failures -= 1
            self._raise(self._renew_error)
        return self._inner.renew(message, visibility_seconds=visibility_seconds)

    def depth(self) -> int:
        return self._inner.depth()


def _with_faulty_queue(
    ctx: ServiceContext, **kwargs: object
) -> tuple[ServiceContext, _FaultyQueue]:
    """Return a context whose queue injects faults (ServiceContext is frozen)."""
    faulty = _FaultyQueue(ctx.queue, **kwargs)  # type: ignore[arg-type]
    return replace(ctx, queue=faulty), faulty  # type: ignore[arg-type]


def test_receive_error_is_absorbed_and_later_messages_still_process() -> None:
    from voice_recorder.ai.fakes import FakeTranscriber

    ctx = _build(FakeTranscriber())
    rid = _seed_one_chunk(ctx)
    ctx, faulty = _with_faulty_queue(ctx, receive_failures=3)
    worker = QueueWorker(ctx)

    # Three consecutive receive failures must not raise and must not settle anything.
    for _ in range(3):
        assert worker.run_once() == 0
    assert faulty.depth() == 1

    # The queue recovers and the pending message is processed normally.
    assert worker.run_once() == 1
    chunk = ctx.chunks.get(rid, 0)
    assert chunk is not None
    assert chunk.state.value == "transcribed"


def test_receive_error_of_unexpected_type_is_absorbed() -> None:
    from voice_recorder.ai.fakes import FakeTranscriber

    ctx = _build(FakeTranscriber())
    _seed_one_chunk(ctx)
    ctx, _faulty = _with_faulty_queue(
        ctx, receive_failures=1, receive_error=RuntimeError("connection reset")
    )
    worker = QueueWorker(ctx)

    assert worker.run_once() == 0  # no exception escapes
    assert worker.run_once() == 1


def test_delete_failure_does_not_kill_the_loop_and_redelivery_is_safe() -> None:
    from voice_recorder.ai.fakes import FakeTranscriber

    ctx = _build(FakeTranscriber())
    rid = _seed_one_chunk(ctx)
    ctx, faulty = _with_faulty_queue(ctx, delete_failures=1)
    worker = QueueWorker(ctx)

    # First pass: work succeeds, delete blows up -> message stays on the queue.
    assert worker.run_once() == 1
    assert faulty.depth() == 1
    chunk = ctx.chunks.get(rid, 0)
    assert chunk is not None and chunk.state.value == "transcribed"

    # Redelivery is idempotent and the message is finally removed.
    assert worker.run_once() == 1
    assert faulty.depth() == 0
    assert get_recording(ctx, rid).state != RecordingState.FAILED


def test_delete_failure_on_terminal_message_keeps_worker_alive() -> None:
    ctx = _build(_RaisingTranscriber(TerminalTranscriptionError("bad audio")))
    rid = _seed_one_chunk(ctx)
    ctx, faulty = _with_faulty_queue(ctx, delete_failures=5, delete_error=RuntimeError("boom"))
    worker = QueueWorker(ctx)

    for _ in range(3):
        worker.run_once()  # must not raise

    assert faulty.delete_calls >= 3
    assert get_recording(ctx, rid).state == RecordingState.FAILED


def test_renew_failure_does_not_kill_the_loop() -> None:
    transcriber = _RaisingTranscriber(TranscriptionError("throttled"))
    ctx = _build(transcriber)
    _seed_one_chunk(ctx)
    ctx, faulty = _with_faulty_queue(ctx, renew_failures=99)
    worker = QueueWorker(ctx)

    for _ in range(6):
        if worker.run_once() == 0:
            break

    assert faulty.renew_calls >= 1
    assert transcriber.attempts >= 3  # kept retrying despite every renew failing


def test_renew_failure_of_unexpected_type_is_absorbed() -> None:
    transcriber = _RaisingTranscriber(TranscriptionError("throttled"))
    ctx = _build(transcriber)
    _seed_one_chunk(ctx)
    ctx, _faulty = _with_faulty_queue(
        ctx, renew_failures=1, renew_error=RuntimeError("pop receipt gone")
    )
    worker = QueueWorker(ctx)

    worker.run_once()  # must not raise
    assert transcriber.attempts == 1
    assert worker.run_once() == 1  # loop still functional


def test_run_loop_survives_persistent_receive_errors_and_stops_promptly() -> None:
    from voice_recorder.ai.fakes import FakeTranscriber

    # A long receive backoff proves the wait is interruptible (Event.wait, not sleep).
    ctx = _build(
        FakeTranscriber(),
        queue_receive_error_backoff_seconds=30.0,
        queue_receive_error_max_backoff_seconds=30.0,
    )
    ctx, faulty = _with_faulty_queue(ctx, receive_failures=10_000)
    worker = QueueWorker(ctx, poll_interval=0.01)

    thread = threading.Thread(target=worker.run, daemon=True)
    thread.start()
    deadline = time.monotonic() + 5.0
    while not faulty.receive_calls and time.monotonic() < deadline:
        time.sleep(0.01)
    assert faulty.receive_calls, "worker never polled the queue"
    assert thread.is_alive(), "worker died on a receive error"

    worker.stop()
    thread.join(timeout=5.0)
    assert not thread.is_alive(), "worker did not react to stop() during receive backoff"


def test_fail_marking_error_does_not_kill_the_loop() -> None:
    ctx = _build(_RaisingTranscriber(TerminalTranscriptionError("bad audio")))
    _seed_one_chunk(ctx)

    class _BrokenRecordings:
        def __getattr__(self, name: str) -> object:
            def _boom(*_a: object, **_k: object) -> object:
                raise RuntimeError("table storage unavailable")

            return _boom

    broken_ctx = replace(ctx, recordings=_BrokenRecordings())  # type: ignore[arg-type]
    worker = QueueWorker(broken_ctx)

    # Every repository call explodes; the loop must absorb it, exhaust the attempts and
    # finally settle the message rather than propagate out of run_once.
    for _ in range(10):
        if worker.run_once() == 0:
            break
    assert ctx.queue.depth() == 0  # type: ignore[attr-defined]


# ------------------------------------------------------- receive shape / visibility
def test_worker_receives_one_message_at_a_time_with_conservative_visibility() -> None:
    from voice_recorder.ai.fakes import FakeTranscriber

    settings = Settings(environment="test")
    assert settings.queue_batch_size == 1
    assert settings.queue_visibility_seconds >= 300

    ctx = replace(_build(FakeTranscriber()), settings=settings)
    ctx, faulty = _with_faulty_queue(ctx)
    QueueWorker(ctx).run_once()

    assert faulty.receive_calls == [(1, settings.queue_visibility_seconds)]


def test_batch_of_messages_is_not_pulled_even_when_many_are_queued() -> None:
    from voice_recorder.ai.fakes import FakeTranscriber

    ctx = _build(FakeTranscriber())
    for index in range(5):
        ctx.queue.send({"type": TRANSCRIBE, "recording_id": "r", "index": index})
    ctx, faulty = _with_faulty_queue(ctx)

    assert QueueWorker(ctx).run_once() == 1
    assert faulty.receive_calls == [(1, ctx.settings.queue_visibility_seconds)]


def test_queue_batch_size_must_be_positive() -> None:
    with pytest.raises(ValueError, match="queue_batch_size"):
        Settings(environment="test", queue_batch_size=0)

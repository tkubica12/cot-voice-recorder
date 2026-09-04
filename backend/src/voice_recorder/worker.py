"""Queue worker: transcribe chunks and finalize recordings with bounded retries.

Retry behaviour is driven by the queue's dequeue count and visibility timeout: a
transient failure leaves the message on the queue (re-hidden with exponential backoff)
until a bounded attempt count is exhausted, at which point the recording is failed with
a useful reason. Terminal failures fail the recording immediately. There are no broad
silent excepts — every failure is classified and logged.

The loop is designed to outlive queue-level faults. ``receive`` errors back off (while
staying responsive to SIGTERM) instead of terminating the process, and per-message
``delete``/``renew`` errors are contained: both pipeline entry points are idempotent, so
the worst case is a redelivery that re-runs work which detects it is already done.

Messages are received one at a time (``queue_batch_size``): processing is sequential, so
a larger batch would let the visibility lease of the tail of the batch expire while the
head is still calling Foundry, producing duplicate model calls.
"""

from __future__ import annotations

import signal
import threading
import types

from .domain import FailureReason, RecordingState
from .errors import QueueMessageGone, TerminalError, TransientError
from .logging_config import get_logger
from .services.context import ServiceContext
from .services.finalize import all_expected_transcribed
from .services.messages import FINALIZE, TRANSCRIBE
from .services.pipeline import fail_chunk, fail_recording, finalize, transcribe_chunk
from .storage.protocols import QueueMessage

logger = get_logger(__name__)


def _backoff_seconds(dequeue_count: int, base: int, cap: int = 600) -> int:
    exponent = max(0, dequeue_count - 1)
    return int(min(cap, base * (2**exponent)))


class QueueWorker:
    def __init__(self, ctx: ServiceContext, *, poll_interval: float = 1.0) -> None:
        self._ctx = ctx
        self._poll_interval = poll_interval
        self._stop = threading.Event()
        self._receive_failures = 0

    def stop(self) -> None:
        self._stop.set()

    def install_signal_handlers(self) -> None:
        def _handler(signum: int, _frame: types.FrameType | None) -> None:
            logger.info("worker_signal", extra={"signal": signum})
            self.stop()

        signal.signal(signal.SIGTERM, _handler)
        signal.signal(signal.SIGINT, _handler)

    def run(self) -> None:
        logger.info("worker_started")
        while not self._stop.is_set():
            handled = self.run_once()
            if handled == 0:
                self._stop.wait(self._poll_interval)
        logger.info("worker_stopped")

    def run_once(self, *, max_messages: int | None = None) -> int:
        """Receive and process one batch. Never raises on queue-level faults."""
        batch = self._ctx.settings.queue_batch_size if max_messages is None else max_messages
        messages = self._receive(batch)
        for message in messages:
            try:
                self._handle(message)
            except Exception as exc:  # defensive: keep the loop alive for later messages
                logger.error(
                    "message_handler_crashed",
                    extra={
                        "type": message.content.get("type"),
                        "recording_id": str(message.content.get("recording_id", "")),
                        "error_type": type(exc).__name__,
                    },
                )
        return len(messages)

    # ------------------------------------------------------------------ internals
    def _receive(self, max_messages: int) -> list[QueueMessage]:
        """Poll the queue; on failure log, back off (SIGTERM-aware) and return nothing."""
        try:
            messages = self._ctx.queue.receive(
                max_messages=max_messages,
                visibility_seconds=self._ctx.settings.queue_visibility_seconds,
            )
        except Exception as exc:
            self._receive_failures += 1
            backoff = self._receive_backoff()
            logger.warning(
                "queue_receive_failed",
                extra={
                    "error_type": type(exc).__name__,
                    "error": str(exc),
                    "consecutive_failures": self._receive_failures,
                    "backoff_seconds": backoff,
                },
            )
            # Event.wait returns immediately once stop() / SIGTERM has been signalled.
            self._stop.wait(backoff)
            return []
        if self._receive_failures:
            logger.info(
                "queue_receive_recovered",
                extra={"consecutive_failures": self._receive_failures},
            )
            self._receive_failures = 0
        return messages

    def _receive_backoff(self) -> float:
        base = self._ctx.settings.queue_receive_error_backoff_seconds
        cap = self._ctx.settings.queue_receive_error_max_backoff_seconds
        return float(min(cap, base * (2 ** (self._receive_failures - 1))))

    def _handle(self, message: QueueMessage) -> None:
        msg_type = message.content.get("type")
        recording_id = str(message.content.get("recording_id", ""))
        try:
            self._process(message)
        except TerminalError as exc:
            logger.error(
                "message_terminal_failure",
                extra={"type": msg_type, "recording_id": recording_id, "error": str(exc)},
            )
            self._fail_safely(message, msg_type, recording_id)
            self._delete(message, msg_type, recording_id)
        except Exception as exc:
            if not isinstance(exc, TransientError):
                # Unexpected: classified explicitly rather than swallowed, then treated as
                # retryable so a latent bug cannot silently drop a recording.
                logger.error(
                    "message_unexpected_failure",
                    extra={
                        "type": msg_type,
                        "recording_id": recording_id,
                        "error_type": type(exc).__name__,
                        "error": str(exc),
                    },
                )
            max_attempts = self._max_attempts(msg_type)
            if message.dequeue_count >= max_attempts:
                logger.error(
                    "message_attempts_exhausted",
                    extra={
                        "type": msg_type,
                        "recording_id": recording_id,
                        "dequeue_count": message.dequeue_count,
                        "error": str(exc),
                    },
                )
                self._fail_safely(message, msg_type, recording_id)
                self._delete(message, msg_type, recording_id)
            else:
                backoff = _backoff_seconds(
                    message.dequeue_count, self._ctx.settings.queue_retry_base_seconds
                )
                logger.warning(
                    "message_retry_scheduled",
                    extra={
                        "type": msg_type,
                        "recording_id": recording_id,
                        "dequeue_count": message.dequeue_count,
                        "backoff_seconds": backoff,
                    },
                )
                self._renew(message, backoff, msg_type, recording_id)
        else:
            self._delete(message, msg_type, recording_id)

    def _delete(self, message: QueueMessage, msg_type: object, recording_id: str) -> None:
        """Delete a settled message. A failure only costs an idempotent redelivery."""
        try:
            self._ctx.queue.delete(message)
        except Exception as exc:
            logger.warning(
                "queue_delete_failed",
                extra={
                    "type": msg_type,
                    "recording_id": recording_id,
                    "dequeue_count": message.dequeue_count,
                    "error_type": type(exc).__name__,
                    "error": str(exc),
                },
            )

    def _renew(
        self, message: QueueMessage, backoff: int, msg_type: object, recording_id: str
    ) -> None:
        """Re-hide a message for ``backoff`` seconds.

        A stale / not-found pop receipt only means the message becomes visible again on
        its own schedule, so the retry still happens — never kill the loop over it.
        """
        try:
            self._ctx.queue.renew(message, visibility_seconds=backoff)
        except QueueMessageGone:
            # The lease is gone; the message reappears on its own and is retried then.
            logger.info(
                "queue_renew_message_gone",
                extra={
                    "type": msg_type,
                    "recording_id": recording_id,
                    "dequeue_count": message.dequeue_count,
                },
            )
        except Exception as exc:
            logger.warning(
                "queue_renew_failed",
                extra={
                    "type": msg_type,
                    "recording_id": recording_id,
                    "dequeue_count": message.dequeue_count,
                    "backoff_seconds": backoff,
                    "error_type": type(exc).__name__,
                    "error": str(exc),
                },
            )

    def _fail_safely(self, message: QueueMessage, msg_type: object, recording_id: str) -> None:
        """Mark the recording failed; storage faults here must not stop the worker."""
        try:
            self._fail(message)
        except Exception as exc:
            logger.error(
                "recording_fail_marking_failed",
                extra={
                    "type": msg_type,
                    "recording_id": recording_id,
                    "error_type": type(exc).__name__,
                    "error": str(exc),
                },
            )

    def _process(self, message: QueueMessage) -> None:
        msg_type = message.content.get("type")
        recording_id = str(message.content["recording_id"])
        if msg_type == TRANSCRIBE:
            transcribe_chunk(self._ctx, recording_id, int(message.content["index"]))
        elif msg_type == FINALIZE:
            finalize(self._ctx, recording_id)
        else:
            raise TerminalError(f"unknown message type: {msg_type!r}")

    def _max_attempts(self, msg_type: object) -> int:
        if msg_type == TRANSCRIBE:
            return self._ctx.settings.max_transcribe_attempts
        return self._ctx.settings.max_refine_attempts

    def _fail(self, message: QueueMessage) -> None:
        msg_type = message.content.get("type")
        recording_id = str(message.content.get("recording_id", ""))
        if not recording_id:
            return
        if msg_type == TRANSCRIBE:
            index = int(message.content.get("index", -1))
            if index >= 0:
                fail_chunk(self._ctx, recording_id, index)
            fail_recording(self._ctx, recording_id, FailureReason.TRANSCRIPTION_FAILED)
        elif msg_type == FINALIZE:
            fail_recording(self._ctx, recording_id, self._finalize_failure_reason(recording_id))

    def _finalize_failure_reason(self, recording_id: str) -> FailureReason:
        recording = self._ctx.recordings.get(recording_id)
        if recording is None:
            return FailureReason.INTERNAL_ERROR
        chunks = self._ctx.chunks.list_for_recording(recording_id)
        if recording.state == RecordingState.REFINING or all_expected_transcribed(
            recording, chunks
        ):
            return FailureReason.REFINEMENT_FAILED
        return FailureReason.MISSING_CHUNKS


def run_worker(ctx: ServiceContext) -> None:
    worker = QueueWorker(ctx)
    worker.install_signal_handlers()
    worker.run()

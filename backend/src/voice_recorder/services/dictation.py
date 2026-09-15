"""Ephemeral dictation, isolated from the durable recording pipeline."""

from __future__ import annotations

import asyncio
from concurrent.futures import Future, ThreadPoolExecutor
from contextvars import copy_context
from threading import BoundedSemaphore

from ..ai.protocols import (
    TerminalTranscriptionError,
    Transcriber,
    TranscriptionError,
    TranscriptionHints,
)
from ..logging_config import get_logger
from ..problems import (
    BadGatewayError,
    GatewayTimeoutError,
    ServiceUnavailableError,
    TooManyRequestsError,
)

MAX_AUDIO_BYTES = 320_000  # 10 seconds of 16 kHz mono, 16-bit PCM.
MAX_BODY_BYTES = MAX_AUDIO_BYTES + 4096
MAX_CONCURRENT_CALLS = 4
TIMEOUT_SECONDS = 20.0

logger = get_logger(__name__)


class DictationService:
    def __init__(
        self, transcriber: Transcriber | None, *, timeout_seconds: float = TIMEOUT_SECONDS
    ) -> None:
        self._transcriber = transcriber
        self._timeout = timeout_seconds
        self._slots = BoundedSemaphore(MAX_CONCURRENT_CALLS)
        self._executor = ThreadPoolExecutor(
            max_workers=MAX_CONCURRENT_CALLS, thread_name_prefix="dictation"
        )

    def close(self) -> None:
        self._executor.shutdown(wait=True, cancel_futures=True)

    def _release(self, future: Future[str]) -> None:
        self._slots.release()

    def _transcribe(self, audio: bytes, language: str) -> str:
        assert self._transcriber is not None
        try:
            text = self._transcriber.transcribe(
                audio, hints=TranscriptionHints(language=language, prompt="", phrases=())
            )
            if not isinstance(text, str):
                raise TerminalTranscriptionError("Invalid transcription result type")
            return text
        except TerminalTranscriptionError:
            raise BadGatewayError("The transcription provider rejected the request.") from None
        except TranscriptionError:
            raise ServiceUnavailableError(
                "The transcription provider is temporarily unavailable."
            ) from None
        except Exception:
            # Provider exceptions may embed audio, text, or credentials. Never log them.
            logger.error("dictation_provider_error")
            raise BadGatewayError("The transcription provider failed.") from None

    async def transcribe(self, audio: bytes, *, language: str) -> str:
        if self._transcriber is None:
            raise ServiceUnavailableError(
                "Dictation requires the azure_speech provider with a configured MAI model."
            )
        if not self._slots.acquire(blocking=False):
            raise TooManyRequestsError(
                "All dictation transcription slots are busy.", headers={"Retry-After": "1"}
            )
        try:
            future = self._executor.submit(copy_context().run, self._transcribe, audio, language)
        except RuntimeError:
            self._slots.release()
            raise ServiceUnavailableError("Dictation is shutting down.") from None
        # Release on the concurrent future, NOT the cancelled/timed-out async waiter.
        # A running synchronous call cannot be cancelled and still consumes capacity.
        future.add_done_callback(self._release)
        try:
            return await asyncio.wait_for(asyncio.wrap_future(future), timeout=self._timeout)
        except TimeoutError:
            raise GatewayTimeoutError("The transcription provider timed out.") from None

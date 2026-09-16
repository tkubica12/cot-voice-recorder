"""Bounded off-loop polishing. Capacity belongs to work, not to HTTP waiters."""

from __future__ import annotations

import asyncio
from concurrent.futures import Future, ThreadPoolExecutor
from contextvars import copy_context
from threading import BoundedSemaphore
from time import perf_counter

from ..ai.protocols import DictationRefiner, RefinementError, TerminalRefinementError
from ..dictation_edits import (
    InvalidDictationEdits,
    ValidatedDictationResult,
    validate_dictation_edits,
)
from ..logging_config import get_logger
from ..problems import (
    BadGatewayError,
    GatewayTimeoutError,
    ServiceUnavailableError,
    TooManyRequestsError,
)

MAX_BODY_BYTES = 32768
MAX_CONCURRENT_CALLS = 2
TIMEOUT_SECONDS = 8.0
logger = get_logger(__name__)


class DictationRefinementService:
    def __init__(
        self, refiner: DictationRefiner | None, *, timeout_seconds: float = TIMEOUT_SECONDS
    ) -> None:
        self._refiner = refiner
        self._timeout = timeout_seconds
        self._slots = BoundedSemaphore(MAX_CONCURRENT_CALLS)
        self._executor = ThreadPoolExecutor(
            max_workers=MAX_CONCURRENT_CALLS, thread_name_prefix="dictation-refinement"
        )

    def close(self) -> None:
        self._executor.shutdown(wait=True, cancel_futures=True)
        if self._refiner is not None:
            self._refiner.close()

    def _release(self, future: Future[ValidatedDictationResult]) -> None:
        self._slots.release()

    def _refine(self, text: str, previous_text: str) -> ValidatedDictationResult:
        assert self._refiner is not None
        try:
            payload = self._refiner.propose_edits(text, previous_text=previous_text)
            return validate_dictation_edits(text, payload, previous_text=previous_text)
        except InvalidDictationEdits as exc:
            logger.warning("dictation_refinement_rejected", extra={"category": exc.category})
            raise BadGatewayError("The refinement provider returned unusable edits.") from None
        except TerminalRefinementError:
            logger.warning("dictation_refinement_rejected", extra={"category": "provider_terminal"})
            raise BadGatewayError("The refinement provider returned unusable edits.") from None
        except RefinementError:
            raise ServiceUnavailableError(
                "The refinement provider is temporarily unavailable."
            ) from None
        except Exception:
            logger.error("dictation_refinement_provider_error")
            raise BadGatewayError("The refinement provider failed.") from None

    async def refine(self, text: str, *, previous_text: str = "") -> ValidatedDictationResult:
        start = perf_counter()
        outcome = "failed"
        try:
            if self._refiner is None:
                outcome = "not_configured"
                raise ServiceUnavailableError("Dictation refinement is not configured.")
            if not self._slots.acquire(blocking=False):
                outcome = "capacity"
                raise TooManyRequestsError(
                    "All dictation refinement slots are busy.", headers={"Retry-After": "1"}
                )
            try:
                future = self._executor.submit(
                    copy_context().run, self._refine, text, previous_text
                )
            except RuntimeError:
                self._slots.release()
                outcome = "shutting_down"
                raise ServiceUnavailableError("Dictation refinement is shutting down.") from None
            future.add_done_callback(self._release)
            try:
                result = await asyncio.wait_for(asyncio.wrap_future(future), timeout=self._timeout)
            except TimeoutError:
                outcome = "timeout"
                raise GatewayTimeoutError("The refinement provider timed out.") from None
            except BadGatewayError:
                outcome = "unusable_provider_result"
                raise
            except ServiceUnavailableError:
                outcome = "provider_unavailable"
                raise
            except asyncio.CancelledError:
                outcome = "cancelled"
                raise
            outcome = "changed" if result.edits else "unchanged"
            return result
        finally:
            logger.info(
                "dictation_refinement_completed",
                extra={
                    "outcome": outcome,
                    "elapsed_ms": round((perf_counter() - start) * 1000, 2),
                },
            )

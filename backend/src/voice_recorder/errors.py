"""Cross-cutting error types used to drive retry/terminal decisions in the worker."""

from __future__ import annotations


class TransientError(Exception):
    """A retryable failure (network blip, throttling, transient 5xx)."""


class TerminalError(Exception):
    """A non-retryable failure; the operation should not be retried."""


class ConcurrencyConflict(Exception):
    """Optimistic-concurrency (ETag) mismatch; the caller should reload and retry."""


class BlobNotFound(Exception):
    """Requested blob does not exist."""


class QueueMessageGone(Exception):
    """The queue message no longer exists, or its pop receipt is stale/expired.

    Not retryable against the same receipt: the message (if it still exists) becomes
    visible again on its own schedule and is redelivered. Callers must treat this as a
    benign outcome because message processing is idempotent.
    """

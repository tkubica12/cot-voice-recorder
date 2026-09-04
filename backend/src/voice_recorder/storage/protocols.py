"""Storage and queue protocols. Implementations: in-memory and Azure Blob/Queue."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Any, Protocol


class BlobStore(Protocol):
    def put(self, container: str, path: str, data: bytes, *, content_type: str) -> None: ...

    def get(self, container: str, path: str) -> bytes:
        """Return blob bytes; raise :class:`BlobNotFound` when missing."""
        ...

    def delete(self, container: str, path: str) -> bool:
        """Delete if present; return True when a blob was removed. Idempotent."""
        ...

    def exists(self, container: str, path: str) -> bool: ...

    def list_paths(self, container: str, *, prefix: str = "") -> list[tuple[str, datetime]]:
        """Return (path, last_modified) tuples for cleanup enumeration."""
        ...


@dataclass(frozen=True, slots=True)
class QueueMessage:
    id: str
    pop_receipt: str
    dequeue_count: int
    content: dict[str, Any]


class WorkQueue(Protocol):
    def send(self, message: dict[str, Any], *, delay_seconds: int = 0) -> None: ...

    def receive(
        self, *, max_messages: int = 1, visibility_seconds: int = 60
    ) -> list[QueueMessage]: ...

    def delete(self, message: QueueMessage) -> None: ...

    def renew(self, message: QueueMessage, *, visibility_seconds: int) -> QueueMessage:
        """Extend a message's invisibility to implement backoff before the next attempt."""
        ...

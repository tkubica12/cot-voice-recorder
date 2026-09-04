"""In-memory blob store and work queue for tests and gated local mode."""

from __future__ import annotations

import json
import threading
import uuid
from collections import deque
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import Any

from ..errors import BlobNotFound
from .protocols import QueueMessage


@dataclass
class _Blob:
    data: bytes
    content_type: str
    last_modified: datetime


class InMemoryBlobStore:
    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._blobs: dict[tuple[str, str], _Blob] = {}

    def put(self, container: str, path: str, data: bytes, *, content_type: str) -> None:
        with self._lock:
            self._blobs[(container, path)] = _Blob(
                data=bytes(data),
                content_type=content_type,
                last_modified=datetime.now(UTC),
            )

    def get(self, container: str, path: str) -> bytes:
        with self._lock:
            blob = self._blobs.get((container, path))
            if blob is None:
                raise BlobNotFound(f"{container}/{path}")
            return blob.data

    def delete(self, container: str, path: str) -> bool:
        with self._lock:
            return self._blobs.pop((container, path), None) is not None

    def exists(self, container: str, path: str) -> bool:
        with self._lock:
            return (container, path) in self._blobs

    def list_paths(self, container: str, *, prefix: str = "") -> list[tuple[str, datetime]]:
        with self._lock:
            return [
                (path, blob.last_modified)
                for (cont, path), blob in self._blobs.items()
                if cont == container and path.startswith(prefix)
            ]


@dataclass
class _QueuedItem:
    message: QueueMessage
    visible_at: datetime


class InMemoryWorkQueue:
    """At-least-once queue with visibility timeouts and dequeue counting."""

    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._items: deque[_QueuedItem] = deque()

    def send(self, message: dict[str, Any], *, delay_seconds: int = 0) -> None:
        with self._lock:
            msg = QueueMessage(
                id=str(uuid.uuid4()),
                pop_receipt=str(uuid.uuid4()),
                dequeue_count=0,
                content=json.loads(json.dumps(message)),
            )
            visible_at = datetime.now(UTC) + timedelta(seconds=delay_seconds)
            self._items.append(_QueuedItem(message=msg, visible_at=visible_at))

    def receive(self, *, max_messages: int = 1, visibility_seconds: int = 60) -> list[QueueMessage]:
        now = datetime.now(UTC)
        out: list[QueueMessage] = []
        with self._lock:
            for item in list(self._items):
                if len(out) >= max_messages:
                    break
                if item.visible_at > now:
                    continue
                receipt = str(uuid.uuid4())
                item.message = QueueMessage(
                    id=item.message.id,
                    pop_receipt=receipt,
                    dequeue_count=item.message.dequeue_count + 1,
                    content=item.message.content,
                )
                item.visible_at = now + timedelta(seconds=visibility_seconds)
                out.append(item.message)
        return out

    def delete(self, message: QueueMessage) -> None:
        with self._lock:
            for item in list(self._items):
                if item.message.id == message.id:
                    if item.message.pop_receipt == message.pop_receipt:
                        self._items.remove(item)
                    return

    def renew(self, message: QueueMessage, *, visibility_seconds: int) -> QueueMessage:
        with self._lock:
            for item in self._items:
                if item.message.id == message.id:
                    item.visible_at = datetime.now(UTC) + timedelta(seconds=visibility_seconds)
                    return item.message
        return message

    # Test helper.
    def depth(self) -> int:
        with self._lock:
            return len(self._items)

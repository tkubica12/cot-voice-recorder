"""In-memory realtime gateway that records notifications for assertions."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from ..models import TranscriptCompletedEvent
from .protocols import ClientAccess


class InMemoryRealtimeGateway:
    def __init__(self, *, hub: str = "transcripts", group: str = "user") -> None:
        self._hub = hub
        self._group = group
        self.sent: list[tuple[str, TranscriptCompletedEvent]] = []

    def negotiate(self, user_id: str) -> ClientAccess:
        return ClientAccess(
            url=f"wss://local.invalid/client/hubs/{self._hub}?access_token=fake",
            hub=self._hub,
            group=self._group,
            expires_at=datetime.now(UTC) + timedelta(hours=1),
        )

    def notify_completed(self, user_id: str, event: TranscriptCompletedEvent) -> None:
        self.sent.append((user_id, event))

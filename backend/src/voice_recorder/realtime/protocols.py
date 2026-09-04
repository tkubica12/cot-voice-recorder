"""Realtime (Web PubSub) protocols."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Protocol

from ..models import TranscriptCompletedEvent


@dataclass(frozen=True, slots=True)
class ClientAccess:
    url: str
    hub: str
    group: str
    expires_at: datetime


class RealtimeGateway(Protocol):
    def negotiate(self, user_id: str) -> ClientAccess:
        """Mint a short-lived, user-scoped client access URL."""
        ...

    def notify_completed(self, user_id: str, event: TranscriptCompletedEvent) -> None:
        """Send a completion event to the user (preview only, never the body)."""
        ...

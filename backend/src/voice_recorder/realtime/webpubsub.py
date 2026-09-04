"""Azure Web PubSub adapter using managed identity.

Mints user-scoped client access URIs and sends completion notifications directly to the
target user. Notifications carry a short preview only — never the transcript body.
"""

from __future__ import annotations

from datetime import timedelta
from typing import Any

from ..clock import Clock
from ..models import TranscriptCompletedEvent
from .protocols import ClientAccess


class WebPubSubGateway:
    def __init__(
        self,
        client: Any,
        *,
        hub: str,
        group: str,
        token_ttl_minutes: int,
        clock: Clock,
    ) -> None:
        self._client = client
        self._hub = hub
        self._group = group
        self._ttl_minutes = token_ttl_minutes
        self._clock = clock

    def negotiate(self, user_id: str) -> ClientAccess:
        token = self._client.get_client_access_token(
            user_id=user_id,
            groups=[self._group],
            roles=[f"webpubsub.joinLeaveGroup.{self._group}"],
            minutes_to_expire=self._ttl_minutes,
        )
        expires_at = self._clock.now() + timedelta(minutes=self._ttl_minutes)
        return ClientAccess(
            url=token["url"],
            hub=self._hub,
            group=self._group,
            expires_at=expires_at,
        )

    def notify_completed(self, user_id: str, event: TranscriptCompletedEvent) -> None:
        self._client.send_to_user(
            user_id,
            event.model_dump(mode="json"),
            content_type="application/json",
        )

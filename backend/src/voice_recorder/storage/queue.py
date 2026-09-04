"""Azure Queue Storage adapter (managed identity or gated Azurite)."""

from __future__ import annotations

import contextlib
import json
from typing import Any

from azure.core.exceptions import (
    HttpResponseError,
    ResourceExistsError,
    ResourceNotFoundError,
)
from azure.storage.queue import (
    QueueClient,
    TextBase64DecodePolicy,
    TextBase64EncodePolicy,
)

from ..errors import QueueMessageGone
from ..logging_config import get_logger
from ._azure_common import wrap_transient
from .protocols import QueueMessage

logger = get_logger(__name__)

# Azure returns 404 MessageNotFound for an already-deleted message and 400
# PopReceiptMismatch / InvalidQueryParameterValue for a stale or expired pop receipt.
_GONE_ERROR_CODES = frozenset({"PopReceiptMismatch", "InvalidQueryParameterValue"})


def _is_gone(exc: BaseException) -> bool:
    """True when the message/receipt no longer identifies a live dequeued message."""
    if isinstance(exc, ResourceNotFoundError):
        return True
    if isinstance(exc, HttpResponseError):
        if exc.status_code == 404:
            return True
        if exc.status_code == 400:
            return str(getattr(exc, "error_code", "") or "") in _GONE_ERROR_CODES
    return False


class AzureWorkQueue:
    def __init__(self, client: QueueClient) -> None:
        self._client = client
        with contextlib.suppress(ResourceExistsError):
            self._client.create_queue()

    def send(self, message: dict[str, Any], *, delay_seconds: int = 0) -> None:
        try:
            self._client.send_message(
                json.dumps(message),
                visibility_timeout=delay_seconds or None,
            )
        except Exception as exc:
            raise wrap_transient(exc, "queue send failed") from exc

    def receive(self, *, max_messages: int = 1, visibility_seconds: int = 60) -> list[QueueMessage]:
        try:
            received = self._client.receive_messages(
                max_messages=max_messages, visibility_timeout=visibility_seconds
            )
            out: list[QueueMessage] = []
            for msg in received:
                out.append(
                    QueueMessage(
                        id=msg.id,
                        pop_receipt=str(msg.pop_receipt),
                        dequeue_count=int(msg.dequeue_count or 0),
                        content=json.loads(msg.content),
                    )
                )
            return out
        except Exception as exc:
            raise wrap_transient(exc, "queue receive failed") from exc

    def delete(self, message: QueueMessage) -> None:
        """Delete a dequeued message. Idempotent: an already-gone message is a success."""
        try:
            self._client.delete_message(message.id, message.pop_receipt)
        except Exception as exc:
            if _is_gone(exc):
                logger.info("queue_delete_already_gone", extra={"message_id": message.id})
                return
            raise wrap_transient(exc, "queue delete failed") from exc

    def renew(self, message: QueueMessage, *, visibility_seconds: int) -> QueueMessage:
        try:
            updated = self._client.update_message(
                message.id,
                pop_receipt=message.pop_receipt,
                visibility_timeout=visibility_seconds,
            )
            return QueueMessage(
                id=message.id,
                pop_receipt=str(updated.pop_receipt),
                dequeue_count=message.dequeue_count,
                content=message.content,
            )
        except Exception as exc:
            if _is_gone(exc):
                raise QueueMessageGone(f"stale pop receipt for message {message.id}") from exc
            raise wrap_transient(exc, "queue update failed") from exc


def build_queue_client(
    *,
    use_azurite: bool,
    connection_string: str,
    queue_endpoint: str,
    queue_name: str,
    credential: Any,
) -> QueueClient:
    encode = TextBase64EncodePolicy()
    decode = TextBase64DecodePolicy()
    if use_azurite:
        return QueueClient.from_connection_string(
            connection_string,
            queue_name,
            message_encode_policy=encode,
            message_decode_policy=decode,
        )
    return QueueClient(
        account_url=queue_endpoint,
        queue_name=queue_name,
        credential=credential,
        message_encode_policy=encode,
        message_decode_policy=decode,
    )

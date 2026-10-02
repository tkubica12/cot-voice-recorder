"""Server-owned MAI configuration; no Azure credential is returned to the desktop."""

from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator
from contextlib import AbstractAsyncContextManager, asynccontextmanager
from typing import Protocol
from urllib.parse import urlsplit

from azure.core.credentials import TokenCredential
from azure.identity import DefaultAzureCredential
from websockets.asyncio.client import connect


class UpstreamStream(Protocol):
    async def send(self, message: str) -> None: ...
    async def recv(self) -> str | bytes: ...


class StreamingProvider(Protocol):
    def session(self, language: str) -> AbstractAsyncContextManager[UpstreamStream]: ...
    async def close(self) -> None: ...


class MaiStreamingProvider:
    def __init__(
        self, endpoint: str, deployment: str, credential: TokenCredential | None = None
    ) -> None:
        parsed = urlsplit(endpoint)
        if (
            parsed.scheme != "https"
            or not parsed.hostname
            or parsed.username
            or parsed.password
            or parsed.query
            or parsed.fragment
            or parsed.path not in ("", "/")
        ):
            raise ValueError("Foundry streaming requires an HTTPS resource root")
        hostname = parsed.hostname.replace(".cognitiveservices.azure.com", ".services.ai.azure.com")
        self._url = f"wss://{hostname}/mai/v1/realtime?intent=transcription"
        self._deployment = deployment
        self._owned_credential: DefaultAzureCredential | None = None
        if credential is None:
            self._owned_credential = DefaultAzureCredential()
            self._credential: TokenCredential = self._owned_credential
        else:
            self._credential = credential

    async def close(self) -> None:
        if self._owned_credential is not None:
            await asyncio.to_thread(self._owned_credential.close)

    @asynccontextmanager
    async def session(self, language: str) -> AsyncIterator[UpstreamStream]:
        token = await asyncio.to_thread(
            self._credential.get_token, "https://cognitiveservices.azure.com/.default"
        )
        async with connect(
            self._url,
            additional_headers={"Authorization": f"Bearer {token.token}"},
            open_timeout=15,
            close_timeout=3,
            max_size=2 * 1024 * 1024,
            max_queue=16,
        ) as ws:

            async def wait_for(kind: str) -> None:
                async with asyncio.timeout(15):
                    while True:
                        event = json.loads(await ws.recv())
                        if event.get("type") == "error":
                            raise RuntimeError("MAI rejected session configuration")
                        if event.get("type") == kind:
                            return

            await wait_for("session.created")
            transcription = {"model": self._deployment}
            if language != "auto":
                transcription["language"] = language
            await ws.send(
                json.dumps(
                    {
                        "type": "session.update",
                        "session": {
                            "type": "transcription",
                            "audio": {
                                "input": {
                                    "format": {"type": "audio/pcm", "rate": 16000},
                                    "transcription": transcription,
                                    "turn_detection": None,
                                    "noise_reduction": None,
                                }
                            },
                        },
                    }
                )
            )
            await wait_for("session.updated")
            yield ws

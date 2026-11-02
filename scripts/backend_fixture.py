"""Loopback-only actual FastAPI protocol fixture; no Azure/model or personal state."""

from __future__ import annotations

import json
import socket
import sys
from pathlib import Path
from urllib.parse import urlsplit

import uvicorn

sys.path.insert(0, str(Path(__file__).resolve().parents[1].joinpath("backend", "src")))
from voice_recorder.ai.fakes import FakeRefiner, FakeTranscriber
from voice_recorder.app import create_app
from voice_recorder.auth import GoogleTokenVerifier
from voice_recorder.clock import SystemClock
from voice_recorder.config import Settings
from voice_recorder.models import TranscriptCompletedEvent
from voice_recorder.problems import ServiceUnavailableError
from voice_recorder.realtime.protocols import ClientAccess
from voice_recorder.repositories.memory import (
    InMemoryChunkRepository,
    InMemoryRecordingRepository,
    InMemoryTranscriptRepository,
)
from voice_recorder.services.context import ServiceContext
from voice_recorder.storage.memory import InMemoryBlobStore, InMemoryWorkQueue


class UnavailableRealtime:
    def negotiate(self, user_id: str) -> ClientAccess:
        raise ServiceUnavailableError("Realtime is not enabled in the protocol fixture.")

    def notify_completed(self, user_id: str, event: TranscriptCompletedEvent) -> None:
        raise ServiceUnavailableError("No model worker or realtime delivery runs in this fixture.")


def run(jwks: str) -> None:
    endpoint = urlsplit(jwks)
    if (
        endpoint.scheme != "http"
        or endpoint.hostname != "127.0.0.1"
        or not endpoint.port
        or endpoint.username
        or endpoint.password
        or endpoint.query
        or endpoint.fragment
    ):
        raise ValueError("An explicit loopback synthetic JWKS endpoint is required.")
    settings = Settings(
        _env_file=None,
        environment="test",
        log_level="ERROR",
        auth_disabled=False,
        google_allowed_audiences=("fixture",),
        allowlisted_email="fixture@example.invalid",
        use_fake_storage=True,
        use_fake_ai=False,
        use_fake_realtime=False,
        transcribe_provider="azure_openai",
        foundry_endpoint="",
        dictation_stream_enabled=False,
    )
    context = ServiceContext(
        settings=settings,
        clock=SystemClock(),
        recordings=InMemoryRecordingRepository(),
        chunks=InMemoryChunkRepository(),
        transcripts=InMemoryTranscriptRepository(),
        blobs=InMemoryBlobStore(),
        queue=InMemoryWorkQueue(),
        transcriber=FakeTranscriber(),
        refiner=FakeRefiner(),
        realtime=UnavailableRealtime(),
    )
    verifier = GoogleTokenVerifier(
        jwks_uri=jwks,
        allowed_audiences=("fixture",),
        allowed_issuers=("https://accounts.google.com", "accounts.google.com"),
        allowlisted_email="fixture@example.invalid",
    )
    app = create_app(settings=settings, context=context, verifier=verifier)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(128)
        listener.setblocking(False)

        class ReadyServer(uvicorn.Server):
            async def startup(self, sockets: list[socket.socket] | None = None) -> None:
                await super().startup(sockets=sockets)
                if not self.started:
                    raise RuntimeError("FastAPI fixture stopped before readiness.")
                print(
                    json.dumps({"url": f"http://127.0.0.1:{listener.getsockname()[1]}"}), flush=True
                )

        server = ReadyServer(
            uvicorn.Config(
                app,
                log_level="error",
                log_config=None,
                access_log=False,
                ws_max_size=16384,
                ws_max_queue=4,
            )
        )
        server.run(sockets=[listener])


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Pass one loopback JWKS fixture URL.")
    run(sys.argv[1])

"""Shared pytest fixtures and helpers.

Everything here is built from in-memory fakes so unit/API tests never touch Azure or
the network. Auth is a deterministic :class:`StaticTokenVerifier`.
"""

from __future__ import annotations

import struct
from collections.abc import Iterator
from datetime import UTC, datetime, timedelta

import pytest
from fastapi.testclient import TestClient

from voice_recorder.ai.fakes import FakeDictationRefiner, FakeRefiner, FakeTranscriber
from voice_recorder.app import create_app
from voice_recorder.auth import AuthenticatedUser, StaticTokenVerifier
from voice_recorder.config import Settings
from voice_recorder.digest import compute_content_digest
from voice_recorder.realtime.memory import InMemoryRealtimeGateway
from voice_recorder.repositories.memory import (
    InMemoryChunkRepository,
    InMemoryRecordingRepository,
    InMemoryTranscriptRepository,
)
from voice_recorder.services.context import ServiceContext
from voice_recorder.storage.memory import InMemoryBlobStore, InMemoryWorkQueue
from voice_recorder.worker import QueueWorker

ALLOWLISTED_EMAIL = "owner@example.com"
GOOD_TOKEN = "good-token"
FORBIDDEN_TOKEN = "forbidden-token"


class FakeClock:
    def __init__(self, start: datetime | None = None) -> None:
        self._now = start or datetime(2026, 9, 3, 12, 0, 0, tzinfo=UTC)

    def now(self) -> datetime:
        return self._now

    def advance(self, *, seconds: float = 0, hours: float = 0) -> None:
        self._now += timedelta(seconds=seconds, hours=hours)


def make_wav(data_bytes: int = 640) -> bytes:
    """Build a valid 16 kHz mono 16-bit PCM WAV with ``data_bytes`` of silence."""
    sample_rate = 16_000
    channels = 1
    bits = 16
    byte_rate = sample_rate * channels * bits // 8
    block_align = channels * bits // 8
    payload = bytes(data_bytes)
    fmt_chunk = struct.pack(
        "<4sIHHIIHH",
        b"fmt ",
        16,
        1,  # PCM
        channels,
        sample_rate,
        byte_rate,
        block_align,
        bits,
    )
    data_chunk = struct.pack("<4sI", b"data", len(payload)) + payload
    riff_size = 4 + len(fmt_chunk) + len(data_chunk)
    return struct.pack("<4sI4s", b"RIFF", riff_size, b"WAVE") + fmt_chunk + data_chunk


def unique_wav(seed: int, data_bytes: int = 640) -> bytes:
    """A valid WAV whose bytes are unique per ``seed`` (distinct digests / fake STT)."""
    base = bytearray(make_wav(data_bytes))
    # Perturb the last byte of the data payload deterministically.
    base[-1] = seed % 256
    return bytes(base)


@pytest.fixture
def clock() -> FakeClock:
    return FakeClock()


@pytest.fixture
def settings() -> Settings:
    return Settings(environment="test", allowlisted_email=ALLOWLISTED_EMAIL)


@pytest.fixture
def transcriber() -> FakeTranscriber:
    return FakeTranscriber()


@pytest.fixture
def refiner() -> FakeRefiner:
    return FakeRefiner()


@pytest.fixture
def realtime() -> InMemoryRealtimeGateway:
    return InMemoryRealtimeGateway()


@pytest.fixture
def queue() -> InMemoryWorkQueue:
    return InMemoryWorkQueue()


@pytest.fixture
def blobs() -> InMemoryBlobStore:
    return InMemoryBlobStore()


@pytest.fixture
def context(
    settings: Settings,
    clock: FakeClock,
    transcriber: FakeTranscriber,
    refiner: FakeRefiner,
    realtime: InMemoryRealtimeGateway,
    queue: InMemoryWorkQueue,
    blobs: InMemoryBlobStore,
) -> ServiceContext:
    return ServiceContext(
        settings=settings,
        clock=clock,
        recordings=InMemoryRecordingRepository(),
        chunks=InMemoryChunkRepository(),
        transcripts=InMemoryTranscriptRepository(),
        blobs=blobs,
        queue=queue,
        transcriber=transcriber,
        refiner=refiner,
        realtime=realtime,
    )


@pytest.fixture
def verifier() -> StaticTokenVerifier:
    return StaticTokenVerifier(
        valid_tokens={
            GOOD_TOKEN: AuthenticatedUser(
                email=ALLOWLISTED_EMAIL, subject="sub-123", audience="aud"
            )
        },
        forbidden_tokens=frozenset({FORBIDDEN_TOKEN}),
    )


@pytest.fixture
def dictation_transcriber() -> FakeTranscriber:
    return FakeTranscriber()


@pytest.fixture
def client(
    context: ServiceContext,
    verifier: StaticTokenVerifier,
    dictation_transcriber: FakeTranscriber,
) -> Iterator[TestClient]:
    app = create_app(
        settings=context.settings,
        context=context,
        verifier=verifier,
        dictation_transcriber=dictation_transcriber,
        dictation_refiner=FakeDictationRefiner(),
    )
    with TestClient(app) as test_client:
        yield test_client


@pytest.fixture
def auth() -> dict[str, str]:
    return {"Authorization": f"Bearer {GOOD_TOKEN}"}


def drain(context: ServiceContext) -> None:
    """Process every currently-visible queue message to completion."""
    worker = QueueWorker(context)
    for _ in range(200):
        if worker.run_once() == 0:
            break


def chunk_headers(data: bytes) -> dict[str, str]:
    return {
        "Content-Type": "audio/wav",
        "Content-Digest": compute_content_digest(data),
    }

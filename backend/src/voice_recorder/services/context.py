"""Dependency container shared by services, routers, worker, and cleanup.

Bundling the ports in one object keeps wiring in one place and lets both the API and
the worker call the same finalization-trigger logic. Everything here is a Protocol, so
tests build a context from in-memory fakes.
"""

from __future__ import annotations

from dataclasses import dataclass

from ..ai.protocols import Refiner, Transcriber
from ..clock import Clock
from ..config import Settings
from ..realtime.protocols import RealtimeGateway
from ..repositories.protocols import (
    ChunkRepository,
    RecordingRepository,
    TranscriptRepository,
)
from ..storage.protocols import BlobStore, WorkQueue


@dataclass(frozen=True, slots=True)
class ServiceContext:
    settings: Settings
    clock: Clock
    recordings: RecordingRepository
    chunks: ChunkRepository
    transcripts: TranscriptRepository
    blobs: BlobStore
    queue: WorkQueue
    transcriber: Transcriber
    refiner: Refiner
    realtime: RealtimeGateway
    # Single-user system: a stable group/user id for realtime notifications.
    user_id: str = "user"


def audio_path(recording_id: str, index: int) -> str:
    return f"{recording_id}/{index}.wav"


def transcript_path(transcript_id: str) -> str:
    return f"{transcript_id}.txt"


def raw_path(recording_id: str) -> str:
    return f"{recording_id}.txt"

"""Queue message schema and helpers."""

from __future__ import annotations

from typing import Any, Literal

TRANSCRIBE = "transcribe"
FINALIZE = "finalize"

MessageType = Literal["transcribe", "finalize"]


def transcribe_message(recording_id: str, index: int) -> dict[str, Any]:
    return {"type": TRANSCRIBE, "recording_id": recording_id, "index": index}


def finalize_message(recording_id: str) -> dict[str, Any]:
    return {"type": FINALIZE, "recording_id": recording_id}

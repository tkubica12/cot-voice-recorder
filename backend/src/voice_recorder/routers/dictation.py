"""Authenticated, nonpersistent low-latency transcription."""

from __future__ import annotations

from typing import Literal

from fastapi import APIRouter, Depends, Request

from ..auth import AuthenticatedUser
from ..deps import require_user
from ..httpio import read_bounded_body
from ..models import DictationResult
from ..problems import UnsupportedMediaTypeError, ValidationProblemError
from ..services.dictation import MAX_AUDIO_BYTES, MAX_BODY_BYTES, DictationService
from ..wav import validate_wav

router = APIRouter(prefix="/v1/dictation", tags=["dictation"])


@router.post(
    "/transcribe",
    response_model=DictationResult,
    operation_id="transcribeDictation",
    openapi_extra={
        "requestBody": {
            "required": True,
            "content": {
                "audio/wav": {
                    "schema": {"type": "string", "format": "binary", "maxLength": MAX_BODY_BYTES}
                }
            },
        }
    },
)
async def transcribe_dictation(
    request: Request,
    language: Literal["auto", "cs", "en"] = "auto",
    _user: AuthenticatedUser = Depends(require_user),
) -> DictationResult:
    content_type = request.headers.get("content-type", "").split(";")[0].strip().lower()
    if content_type != "audio/wav":
        raise UnsupportedMediaTypeError("Dictation requires audio/wav.")
    if request.headers.get("content-encoding", "identity").lower() != "identity":
        raise UnsupportedMediaTypeError("Encoded dictation bodies are not supported.")
    audio = await read_bounded_body(request, MAX_BODY_BYTES)
    props = validate_wav(audio, sample_rate=16_000, channels=1, bits_per_sample=16, strict=True)
    if props.data_bytes > MAX_AUDIO_BYTES:
        raise ValidationProblemError("Dictation audio must be no longer than 10 seconds.")
    service: DictationService = request.app.state.dictation
    return DictationResult(text=await service.transcribe(audio, language=language))

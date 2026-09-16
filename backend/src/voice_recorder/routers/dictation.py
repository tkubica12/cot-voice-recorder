"""Authenticated, nonpersistent low-latency transcription."""

from __future__ import annotations

import json
from typing import Literal

from fastapi import APIRouter, Depends, Request
from pydantic import ValidationError

from ..auth import AuthenticatedUser
from ..deps import require_user
from ..dictation_edits import strict_json_object
from ..httpio import read_bounded_body
from ..models import DictationEdit, DictationRefineRequest, DictationRefineResult, DictationResult
from ..problems import UnsupportedMediaTypeError, ValidationProblemError
from ..services.dictation import MAX_AUDIO_BYTES, MAX_BODY_BYTES, DictationService
from ..services.dictation_refinement import MAX_BODY_BYTES as MAX_REFINE_BODY_BYTES
from ..services.dictation_refinement import DictationRefinementService
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


@router.post(
    "/refine",
    response_model=DictationRefineResult,
    operation_id="refineDictation",
    openapi_extra={
        "requestBody": {
            "required": True,
            "content": {"application/json": {"schema": DictationRefineRequest.model_json_schema()}},
        }
    },
)
async def refine_dictation(
    request: Request,
    _user: AuthenticatedUser = Depends(require_user),
) -> DictationRefineResult:
    content_type = request.headers.get("content-type", "").split(";")[0].strip().lower()
    if content_type != "application/json":
        raise UnsupportedMediaTypeError("Dictation refinement requires application/json.")
    if request.headers.get("content-encoding", "identity").lower() != "identity":
        raise UnsupportedMediaTypeError("Encoded dictation bodies are not supported.")
    body = await read_bounded_body(request, MAX_REFINE_BODY_BYTES)
    try:
        payload = json.loads(body.decode("utf-8"), object_pairs_hook=strict_json_object)
        data = DictationRefineRequest.model_validate(payload)
        # Reject unpaired surrogates before they reach HTTP encoding or response serialization.
        data.text.encode("utf-8")
        data.previous_text.encode("utf-8")
    except (ValueError, ValidationError, RecursionError):
        raise ValidationProblemError("Invalid dictation refinement request.") from None
    service: DictationRefinementService = request.app.state.dictation_refinement
    result = await service.refine(data.text, previous_text=data.previous_text)
    return DictationRefineResult(
        text=result.text,
        edits=[
            DictationEdit(original=edit.original, replacement=edit.replacement)
            for edit in result.edits
        ],
    )

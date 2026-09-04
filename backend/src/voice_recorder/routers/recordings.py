"""Recording lifecycle and chunk upload routes."""

from __future__ import annotations

from datetime import datetime

from fastapi import APIRouter, Depends, Header, Request, Response

from ..auth import AuthenticatedUser
from ..deps import get_context, require_user
from ..digest import parse_content_digest, verify_content_digest
from ..httpio import read_bounded_body
from ..models import (
    ChunkAccepted,
    CompleteRecordingRequest,
    CreateRecordingRequest,
    Recording,
)
from ..problems import UnsupportedMediaTypeError, ValidationProblemError
from ..services.context import ServiceContext
from ..services.recordings import (
    complete_recording,
    create_recording,
    get_recording,
    to_api_recording,
    upload_chunk,
)
from ..wav import validate_wav

router = APIRouter(prefix="/v1/recordings", tags=["recordings"])


@router.post("", status_code=201, response_model=Recording, operation_id="createRecording")
def create_recording_route(
    body: CreateRecordingRequest,
    response: Response,
    _user: AuthenticatedUser = Depends(require_user),
    ctx: ServiceContext = Depends(get_context),
) -> Recording:
    recording, created = create_recording(
        ctx,
        client_recording_id=body.client_recording_id,
        refine_model=body.refine_model,
        language=body.language,
    )
    if created:
        response.status_code = 201
        response.headers["Location"] = f"/v1/recordings/{recording.recording_id}"
    else:
        response.status_code = 200
    return to_api_recording(ctx, recording)


@router.get("/{recording_id}", response_model=Recording, operation_id="getRecording")
def get_recording_route(
    recording_id: str,
    _user: AuthenticatedUser = Depends(require_user),
    ctx: ServiceContext = Depends(get_context),
) -> Recording:
    recording = get_recording(ctx, recording_id)
    return to_api_recording(ctx, recording)


@router.put(
    "/{recording_id}/chunks/{index}",
    status_code=202,
    response_model=ChunkAccepted,
    operation_id="uploadChunk",
)
async def upload_chunk_route(
    recording_id: str,
    index: int,
    request: Request,
    response: Response,
    content_digest: str | None = Header(default=None, alias="Content-Digest"),
    x_chunk_duration_ms: int | None = Header(default=None, alias="X-Chunk-Duration-Ms"),
    x_chunk_overlap_ms: int | None = Header(default=None, alias="X-Chunk-Overlap-Ms"),
    x_chunk_started_at: datetime | None = Header(default=None, alias="X-Chunk-Started-At"),
    _user: AuthenticatedUser = Depends(require_user),
    ctx: ServiceContext = Depends(get_context),
) -> ChunkAccepted:
    if index < 0:
        raise ValidationProblemError(
            "Chunk index must be non-negative.",
            errors=[{"field": "index", "message": "must be >= 0"}],
        )
    content_type = request.headers.get("content-type", "")
    if not content_type.split(";")[0].strip().lower() == "audio/wav":
        raise UnsupportedMediaTypeError("Chunk upload must be audio/wav.")

    data = await read_bounded_body(request, ctx.settings.max_chunk_bytes)
    digest = parse_content_digest(content_digest)
    if not verify_content_digest(data, digest):
        raise ValidationProblemError(
            "Request body does not match the Content-Digest.",
            errors=[{"field": "Content-Digest", "message": "digest mismatch"}],
        )
    validate_wav(
        data,
        sample_rate=ctx.settings.wav_sample_rate,
        channels=ctx.settings.wav_channels,
        bits_per_sample=ctx.settings.wav_bits_per_sample,
    )

    model, status = upload_chunk(
        ctx,
        recording_id=recording_id,
        index=index,
        data=data,
        checksum=digest,
        duration_ms=x_chunk_duration_ms,
        overlap_ms=x_chunk_overlap_ms,
        started_at=x_chunk_started_at,
    )
    response.status_code = status
    return model


@router.post(
    "/{recording_id}/complete",
    status_code=202,
    response_model=Recording,
    operation_id="completeRecording",
)
def complete_recording_route(
    recording_id: str,
    body: CompleteRecordingRequest,
    response: Response,
    _user: AuthenticatedUser = Depends(require_user),
    ctx: ServiceContext = Depends(get_context),
) -> Recording:
    recording, status = complete_recording(
        ctx, recording_id=recording_id, chunk_count=body.chunk_count
    )
    response.status_code = status
    return to_api_recording(ctx, recording)

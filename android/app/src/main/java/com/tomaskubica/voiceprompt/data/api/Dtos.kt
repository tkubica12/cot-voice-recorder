package com.tomaskubica.voiceprompt.data.api

import com.squareup.moshi.Json
import com.squareup.moshi.JsonClass

// ---------------------------------------------------------------------------
// Wire DTOs matching openapi/voice-recorder.yaml exactly.
// ---------------------------------------------------------------------------

@JsonClass(generateAdapter = true)
data class ClientInfoDto(
    val platform: String = "android",
    @Json(name = "app_version") val appVersion: String? = null,
)

@JsonClass(generateAdapter = true)
data class CreateRecordingRequestDto(
    @Json(name = "client_recording_id") val clientRecordingId: String,
    @Json(name = "refine_model") val refineModel: String? = null,
    val language: String? = null,
    val client: ClientInfoDto? = null,
    @Json(name = "started_at") val startedAt: String? = null,
)

@JsonClass(generateAdapter = true)
data class RecordingProgressDto(
    @Json(name = "expected_chunk_count") val expectedChunkCount: Int? = null,
    @Json(name = "received_chunk_count") val receivedChunkCount: Int = 0,
    @Json(name = "transcribed_chunk_count") val transcribedChunkCount: Int = 0,
)

@JsonClass(generateAdapter = true)
data class RecordingDto(
    @Json(name = "recording_id") val recordingId: String,
    @Json(name = "client_recording_id") val clientRecordingId: String,
    val state: String,
    @Json(name = "refine_model") val refineModel: String,
    val language: String,
    val progress: RecordingProgressDto,
    @Json(name = "transcript_id") val transcriptId: String? = null,
    @Json(name = "failure_reason") val failureReason: String? = null,
    @Json(name = "created_at") val createdAt: String,
    @Json(name = "updated_at") val updatedAt: String,
)

@JsonClass(generateAdapter = true)
data class ChunkAcceptedDto(
    @Json(name = "recording_id") val recordingId: String,
    val index: Int,
    @Json(name = "chunk_state") val chunkState: String,
    val checksum: String,
    @Json(name = "received_at") val receivedAt: String,
)

@JsonClass(generateAdapter = true)
data class CompleteRecordingRequestDto(
    @Json(name = "chunk_count") val chunkCount: Int,
    @Json(name = "stopped_at") val stoppedAt: String? = null,
)

@JsonClass(generateAdapter = true)
data class TranscriptSummaryDto(
    @Json(name = "transcript_id") val transcriptId: String,
    @Json(name = "recording_id") val recordingId: String,
    val preview: String,
    @Json(name = "completed_at") val completedAt: String,
    @Json(name = "expires_at") val expiresAt: String,
    val language: String,
    @Json(name = "refine_model") val refineModel: String,
)

@JsonClass(generateAdapter = true)
data class TranscriptListPageDto(
    val items: List<TranscriptSummaryDto>,
    @Json(name = "next_cursor") val nextCursor: String? = null,
)

@JsonClass(generateAdapter = true)
data class TranscriptDto(
    @Json(name = "transcript_id") val transcriptId: String,
    @Json(name = "recording_id") val recordingId: String,
    val body: String,
    val preview: String,
    val language: String,
    @Json(name = "refine_model") val refineModel: String,
    @Json(name = "completed_at") val completedAt: String,
    @Json(name = "expires_at") val expiresAt: String,
    @Json(name = "character_count") val characterCount: Int,
)

@JsonClass(generateAdapter = true)
data class ProblemFieldErrorDto(
    val field: String? = null,
    val message: String,
)

@JsonClass(generateAdapter = true)
data class ProblemDto(
    val type: String? = null,
    val title: String? = null,
    val status: Int? = null,
    val detail: String? = null,
    val instance: String? = null,
    val errors: List<ProblemFieldErrorDto>? = null,
)

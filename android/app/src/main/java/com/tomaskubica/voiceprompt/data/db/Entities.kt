package com.tomaskubica.voiceprompt.data.db

import androidx.room.Entity
import androidx.room.Index
import androidx.room.PrimaryKey

/**
 * One row per recording, keyed by the client-generated UUID (idempotency key). The server
 * recording id is filled in once session creation succeeds.
 */
@Entity(tableName = "recordings")
data class RecordingEntity(
    @PrimaryKey val clientRecordingId: String,
    val recordingId: String? = null,
    val localState: String,          // LocalRecordingState
    val serverState: String = "UNKNOWN",
    val refineModel: String,
    val language: String,
    val chunkCount: Int? = null,     // declared at stop
    val transcriptId: String? = null,
    val failureReason: String? = null,
    /** Set when the user asked to retry a failed upload; cleared once the upload settles. */
    val retryRequestedAtEpochMs: Long? = null,
    val startedAtEpochMs: Long,
    val stoppedAtEpochMs: Long? = null,
    val createdAtEpochMs: Long,
    val updatedAtEpochMs: Long,
)

/**
 * One row per captured chunk. The WAV file is stored on disk and deleted only after the backend
 * acknowledges the upload (ack-then-delete).
 */
@Entity(
    tableName = "chunks",
    indices = [Index(value = ["clientRecordingId", "index"], unique = true)],
)
data class ChunkEntity(
    @PrimaryKey(autoGenerate = true) val id: Long = 0,
    val clientRecordingId: String,
    val index: Int,
    val filePath: String?,           // null once deleted after ack
    val checksum: String,
    val sizeBytes: Int,
    val durationMs: Int,
    val overlapMs: Int,
    val startedAtIso: String?,
    val uploadState: String,         // ChunkUploadState
    val createdAtEpochMs: Long,
)

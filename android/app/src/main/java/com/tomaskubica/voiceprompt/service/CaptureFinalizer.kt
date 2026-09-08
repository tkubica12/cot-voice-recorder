package com.tomaskubica.voiceprompt.service

import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.model.ChunkUploadState
import com.tomaskubica.voiceprompt.work.UploadWorkScheduler

/** What happened to the local recording when capture ended. */
sealed interface CaptureResult {
    /** Nothing durable existed (or the user cancelled): local state removed. */
    data class Discarded(val reason: String) : CaptureResult

    /**
     * [chunkCount] durable chunks are scheduled for upload and completion.
     * [captureFailed] is true when capture ended with an error after partial capture.
     */
    data class Uploading(val chunkCount: Int, val captureFailed: Boolean) : CaptureResult
}

/**
 * Decides what happens to a recording when capture ends, from the durable state rather than
 * from an in-memory counter.
 *
 * The bug this replaces collapsed *any* mid-recording exception to "0 chunks emitted" and then
 * deleted the whole recording directory — silently destroying every WAV that had already been
 * written to disk. Here the repository is the source of truth: local data is deleted only when
 * the user explicitly cancelled, or when the repository proves nothing was persisted. Any other
 * outcome (including a crash after partial capture) schedules upload of every durable chunk and
 * declares the real chunk count, which is exactly what the backend `complete` contract expects.
 */
class CaptureFinalizer(
    private val repository: RecordingRepository,
    private val scheduler: UploadWorkScheduler,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    /**
     * @param outcome result of the capture loop; a failure must not imply "nothing captured".
     * @param cancelled true only when the user explicitly cancelled this recording.
     */
    suspend fun finish(
        clientId: String,
        outcome: Result<Int>,
        cancelled: Boolean,
    ): CaptureResult = repository.withUploadScheduling {
        val persisted = repository.persistedChunkCount(clientId)

        if (cancelled) {
            repository.discardRecording(clientId)
            return@withUploadScheduling CaptureResult.Discarded("cancelled")
        }
        if (persisted == 0) {
            // Genuinely nothing to upload: either silence or a failure before the first chunk.
            repository.discardRecording(clientId)
            return@withUploadScheduling CaptureResult.Discarded(if (outcome.isFailure) "capture_failed" else "empty")
        }

        // Guarantee upload work exists for every durable chunk. Enqueueing a chunk twice is
        // harmless (uploads are idempotent), whereas a missing enqueue would stall `complete`.
        repository.chunksFor(clientId)
            .filter { it.uploadState != ChunkUploadState.ACKED.name }
            .forEach { scheduler.enqueueChunk(clientId, it.index) }

        repository.markStopped(clientId, persisted, clock())
        if (outcome.isFailure) {
            repository.noteCaptureError(clientId, "capture_error")
        }
        scheduler.enqueueComplete(clientId)
        CaptureResult.Uploading(persisted, captureFailed = outcome.isFailure)
    }
}

package com.tomaskubica.voiceprompt.data

import com.tomaskubica.voiceprompt.audio.ContentDigest
import com.tomaskubica.voiceprompt.audio.StreamingChunker
import com.tomaskubica.voiceprompt.audio.WavWriter
import com.tomaskubica.voiceprompt.data.db.ChunkDao
import com.tomaskubica.voiceprompt.data.db.ChunkEntity
import com.tomaskubica.voiceprompt.data.db.RecordingDao
import com.tomaskubica.voiceprompt.data.db.RecordingEntity
import com.tomaskubica.voiceprompt.data.model.ChunkUploadState
import com.tomaskubica.voiceprompt.data.model.LocalRecordingState
import com.tomaskubica.voiceprompt.data.model.ServerRecordingState
import kotlinx.coroutines.flow.Flow
import java.time.Instant

/**
 * Durable store of recordings and chunks. All audio and metadata are persisted before upload so
 * recordings survive process death, service restart and backend cold start. WAV files are deleted
 * only after backend acknowledgement (ack-then-delete), handled here on [markChunkAcked].
 */
class RecordingRepository(
    private val recordingDao: RecordingDao,
    private val chunkDao: ChunkDao,
    private val fileStore: ChunkFileStore,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    // -------------------------------------------------------------- recordings
    suspend fun createLocalRecording(
        clientRecordingId: String,
        refineModel: String,
        language: String,
        startedAtEpochMs: Long,
    ): RecordingEntity {
        val now = clock()
        val entity = RecordingEntity(
            clientRecordingId = clientRecordingId,
            recordingId = null,
            localState = LocalRecordingState.RECORDING.name,
            serverState = ServerRecordingState.UNKNOWN.name,
            refineModel = refineModel,
            language = language,
            startedAtEpochMs = startedAtEpochMs,
            createdAtEpochMs = now,
            updatedAtEpochMs = now,
        )
        recordingDao.upsert(entity)
        return entity
    }

    suspend fun getRecording(clientId: String): RecordingEntity? = recordingDao.get(clientId)

    fun observeRecording(clientId: String): Flow<RecordingEntity?> = recordingDao.observe(clientId)

    fun observeRecent(limit: Int): Flow<List<RecordingEntity>> = recordingDao.observeRecent(limit)

    fun observeLatest(): Flow<RecordingEntity?> = recordingDao.observeLatest()

    suspend fun setServerRecordingId(clientId: String, recordingId: String, serverState: String) {
        val r = recordingDao.get(clientId) ?: return
        recordingDao.update(r.copy(recordingId = recordingId, serverState = serverState, updatedAtEpochMs = clock()))
    }

    suspend fun updateServerState(clientId: String, serverState: String, transcriptId: String?, failureReason: String?) {
        val r = recordingDao.get(clientId) ?: return
        val localState = when (serverState) {
            ServerRecordingState.COMPLETED.name -> LocalRecordingState.COMPLETED.name
            ServerRecordingState.FAILED.name -> LocalRecordingState.FAILED.name
            else -> r.localState
        }
        val terminal = localState == LocalRecordingState.COMPLETED.name ||
            localState == LocalRecordingState.FAILED.name
        recordingDao.update(
            r.copy(
                serverState = serverState,
                transcriptId = transcriptId ?: r.transcriptId,
                failureReason = failureReason ?: r.failureReason,
                localState = localState,
                // The retry settled one way or the other: stop advertising "retrying".
                retryRequestedAtEpochMs = if (terminal) null else r.retryRequestedAtEpochMs,
                updatedAtEpochMs = clock(),
            ),
        )
    }

    suspend fun markStopped(clientId: String, chunkCount: Int, stoppedAtEpochMs: Long) {
        val r = recordingDao.get(clientId) ?: return
        recordingDao.update(
            r.copy(
                localState = LocalRecordingState.COMPLETING.name,
                chunkCount = chunkCount,
                stoppedAtEpochMs = stoppedAtEpochMs,
                updatedAtEpochMs = clock(),
            ),
        )
    }

    suspend fun markFailed(clientId: String, reason: String) {
        val r = recordingDao.get(clientId) ?: return
        recordingDao.update(
            r.copy(
                localState = LocalRecordingState.FAILED.name,
                failureReason = reason,
                retryRequestedAtEpochMs = null,
                updatedAtEpochMs = clock(),
            ),
        )
    }

    /**
     * Prepare a user-initiated retry of a failed/stalled upload.
     *
     * Clears the sticky [LocalRecordingState.FAILED] state (which otherwise makes
     * `UploadOrchestrator.complete` refuse forever) and the stale failure reason, and puts the
     * recording back on the state its progress warrants: [LocalRecordingState.COMPLETING] once
     * capture has stopped and the chunk count is known, otherwise [LocalRecordingState.STOPPED].
     *
     * The transition is a single atomic UPDATE statement, so a crash can never leave the row half
     * reset. Nothing is deleted: chunk rows, WAV files on disk, per-chunk acked status, the server
     * recording id and the declared chunk count are all preserved, so a retry re-uploads only what
     * the backend has not acknowledged.
     *
     * @return the refreshed row, or `null` when the recording is unknown or not retryable
     *  (still capturing, or already completed).
     */
    suspend fun prepareRetry(clientId: String): RecordingEntity? {
        val updated = recordingDao.resetForRetry(clientId, clock())
        if (updated == 0) return null
        return recordingDao.get(clientId)
    }

    /**
     * Record that local capture ended abnormally **without** discarding the recording.
     *
     * The already-persisted chunks are still durable and still worth uploading, so the local
     * state is left on its upload path and only the reason is annotated for the UI.
     */
    suspend fun noteCaptureError(clientId: String, reason: String) {
        val r = recordingDao.get(clientId) ?: return
        recordingDao.update(r.copy(failureReason = reason, updatedAtEpochMs = clock()))
    }

    /** Number of chunks durably persisted for a recording (the source of truth on failure). */
    suspend fun persistedChunkCount(clientId: String): Int = chunkDao.forRecording(clientId).size

    /**
     * Delete every local trace of a recording. Only safe when the user explicitly cancelled or
     * nothing was ever persisted — never as a reaction to a capture error.
     */
    suspend fun discardRecording(clientId: String) {
        fileStore.deleteRecording(clientId)
        chunkDao.deleteForRecording(clientId)
        recordingDao.delete(clientId)
    }

    suspend fun recordingIdFor(clientId: String): String? = recordingDao.recordingIdFor(clientId)

    // ------------------------------------------------------------------ chunks
    /**
     * Persist a captured chunk: build the WAV, compute its digest, write the file to disk and
     * insert a PENDING chunk row. Idempotent on (clientId, index).
     */
    suspend fun persistChunk(
        clientId: String,
        raw: StreamingChunker.RawChunk,
        recordingStartEpochMs: Long,
    ): ChunkEntity {
        val existing = chunkDao.get(clientId, raw.index)
        if (existing != null) return existing

        val wav = WavWriter.wavBytes(raw.pcm)
        val digest = ContentDigest.contentDigestHeader(wav)
        val file = fileStore.write(clientId, raw.index, wav)
        val startedAtIso = Instant.ofEpochMilli(recordingStartEpochMs + raw.startOffsetMs).toString()

        val entity = ChunkEntity(
            clientRecordingId = clientId,
            index = raw.index,
            filePath = file.absolutePath,
            checksum = digest,
            sizeBytes = wav.size,
            durationMs = raw.durationMs,
            overlapMs = raw.overlapMs,
            startedAtIso = startedAtIso,
            uploadState = ChunkUploadState.PENDING.name,
            createdAtEpochMs = clock(),
        )
        val id = chunkDao.insertIfAbsent(entity)
        return if (id > 0) entity.copy(id = id) else (chunkDao.get(clientId, raw.index) ?: entity)
    }

    suspend fun getChunk(clientId: String, index: Int): ChunkEntity? = chunkDao.get(clientId, index)

    suspend fun chunksFor(clientId: String): List<ChunkEntity> = chunkDao.forRecording(clientId)

    /** Indices of chunks the backend has not acknowledged yet (PENDING or FAILED). */
    suspend fun pendingChunkIndices(clientId: String): List<Int> =
        chunkDao.forRecording(clientId)
            .filter { it.uploadState != ChunkUploadState.ACKED.name }
            .map { it.index }

    fun observePendingChunks(clientId: String): Flow<Int> = chunkDao.observePending(clientId)

    fun observeAllPendingChunks(): Flow<Int> = chunkDao.observeAllPending()

    suspend fun markChunkAcked(chunk: ChunkEntity) {
        chunkDao.update(chunk.copy(uploadState = ChunkUploadState.ACKED.name, filePath = null))
        // Persist the acknowledgement before deleting the WAV. Cancellation or process death
        // may leave an orphan file for cleanup, but can never leave a pending row without audio.
        fileStore.delete(chunk.filePath)
    }

    suspend fun markChunkFailed(chunk: ChunkEntity) {
        chunkDao.update(chunk.copy(uploadState = ChunkUploadState.FAILED.name))
    }

    /** All declared chunks present and acknowledged — precondition for sending `complete`. */
    suspend fun allChunksAcked(clientId: String, expectedCount: Int): Boolean {
        return chunkDao.ackedCount(clientId) >= expectedCount && chunkDao.pendingCount(clientId) == 0
    }

    // ----------------------------------------------------------------- cleanup
    /**
     * Delete local metadata + WAVs for recordings older than [olderThanMs] that are NOT pending
     * upload. Never removes unacknowledged audio purely on age.
     */
    suspend fun cleanup(retentionMs: Long = RETENTION_MS): Int {
        val cutoff = clock() - retentionMs
        var removed = 0
        for (r in recordingDao.recent(500)) {
            if (r.createdAtEpochMs >= cutoff) continue
            val pending = chunkDao.pendingCount(r.clientRecordingId)
            val terminal = r.localState == LocalRecordingState.COMPLETED.name ||
                r.localState == LocalRecordingState.FAILED.name
            // Safe to remove only when no chunk is awaiting upload.
            if (pending == 0 && (terminal || r.localState != LocalRecordingState.RECORDING.name)) {
                fileStore.deleteRecording(r.clientRecordingId)
                chunkDao.deleteForRecording(r.clientRecordingId)
                recordingDao.delete(r.clientRecordingId)
                removed++
            }
        }
        return removed
    }

    companion object {
        const val RETENTION_MS = 48L * 60 * 60 * 1000 // 48 hours
    }
}

package com.tomaskubica.voiceprompt.work

import com.tomaskubica.voiceprompt.auth.AuthTokenProvider
import com.tomaskubica.voiceprompt.auth.StoredToken
import com.tomaskubica.voiceprompt.auth.TokenResult
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.api.ApiResult
import com.tomaskubica.voiceprompt.data.api.ClientInfoDto
import com.tomaskubica.voiceprompt.data.api.CompleteRecordingRequestDto
import com.tomaskubica.voiceprompt.data.api.CreateRecordingRequestDto
import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import com.tomaskubica.voiceprompt.data.model.ChunkUploadState
import com.tomaskubica.voiceprompt.data.model.LocalRecordingState
import com.tomaskubica.voiceprompt.data.model.Outcome
import com.tomaskubica.voiceprompt.data.model.ServerRecordingState
import java.io.File
import java.time.Instant

/**
 * Outcome of one upload step, mapped to a WorkManager Result by the workers.
 *
 * [AUTH_REQUIRED] is distinct from [RETRY] so the workers can tell the user that sign-in
 * expired instead of retrying opaquely forever. Both keep the work retryable — captured
 * audio is never discarded because of auth.
 */
enum class StepOutcome { SUCCESS, RETRY, AUTH_REQUIRED, FAILURE }

/**
 * Pure(ish) orchestration of the durable upload steps, independent of WorkManager so it can be
 * unit tested with MockWebServer and an in-memory database:
 *
 *  - idempotent session create (retries through cold start / auth-not-ready),
 *  - idempotent chunk PUT with ack-then-delete,
 *  - complete-after-acks.
 *
 * Tokens come from an [AuthTokenProvider], which returns a cached *valid* ID token or silently
 * refreshes one from an already-authorized account; an expired token is never sent. Retry vs.
 * terminal is decided by [Outcome]: 401 -> AUTH_REQUIRED (after invalidating the cached token),
 * network / 429 / 5xx -> RETRY; 409 / 422 / other 4xx -> FAILURE. No audio bytes are ever logged.
 */
class UploadOrchestrator(
    private val repository: RecordingRepository,
    private val api: VoiceApiClient,
    private val tokens: AuthTokenProvider,
    private val appVersion: String = "1.0.0",
) {
    suspend fun createSession(clientId: String): StepOutcome {
        val recording = repository.getRecording(clientId) ?: return StepOutcome.FAILURE
        if (recording.recordingId != null) return StepOutcome.SUCCESS // idempotent replay

        val token = token() ?: return StepOutcome.AUTH_REQUIRED

        val request = CreateRecordingRequestDto(
            clientRecordingId = clientId,
            refineModel = recording.refineModel,
            language = recording.language,
            client = ClientInfoDto(platform = "android", appVersion = appVersion),
            startedAt = Instant.ofEpochMilli(recording.startedAtEpochMs).toString(),
        )
        return when (val res = api.createRecording(token.idToken, request)) {
            is ApiResult.Success -> {
                repository.setServerRecordingId(clientId, res.body.recordingId, res.body.state)
                StepOutcome.SUCCESS
            }
            else -> mapNonSuccess(res.outcome) { repository.markFailed(clientId, "create_failed") }
        }
    }

    suspend fun uploadChunk(clientId: String, index: Int): StepOutcome {
        val chunk = repository.getChunk(clientId, index) ?: return StepOutcome.FAILURE
        if (chunk.uploadState == ChunkUploadState.ACKED.name) return StepOutcome.SUCCESS

        val recordingId = repository.recordingIdFor(clientId) ?: return StepOutcome.RETRY
        val token = token() ?: return StepOutcome.AUTH_REQUIRED

        val path = chunk.filePath ?: return StepOutcome.SUCCESS
        val file = File(path)
        if (!file.exists()) {
            repository.markChunkFailed(chunk)
            repository.markFailed(clientId, "chunk_file_missing")
            return StepOutcome.FAILURE
        }
        val res = api.uploadChunk(
            token = token.idToken,
            recordingId = recordingId,
            index = index,
            wavBytes = file.readBytes(),
            contentDigest = chunk.checksum,
            durationMs = chunk.durationMs,
            overlapMs = chunk.overlapMs,
            startedAtIso = chunk.startedAtIso,
        )
        return when (res) {
            is ApiResult.Success -> {
                repository.markChunkAcked(chunk) // ack-then-delete
                StepOutcome.SUCCESS
            }
            else -> mapNonSuccess(res.outcome) {
                repository.markChunkFailed(chunk)
                repository.markFailed(clientId, "chunk_conflict")
            }
        }
    }

    suspend fun complete(clientId: String): StepOutcome {
        val recording = repository.getRecording(clientId) ?: return StepOutcome.FAILURE
        val chunkCount = recording.chunkCount ?: return StepOutcome.RETRY
        if (recording.serverState == ServerRecordingState.COMPLETED.name) return StepOutcome.SUCCESS
        // A terminal local failure (e.g. a chunk conflict) can never satisfy complete-after-acks;
        // stop here so the user sees a retryable failure instead of an endless silent retry.
        if (recording.localState == LocalRecordingState.FAILED.name) return StepOutcome.FAILURE

        if (!repository.allChunksAcked(clientId, chunkCount)) return StepOutcome.RETRY // after-acks

        val recordingId = repository.recordingIdFor(clientId) ?: return StepOutcome.RETRY
        val token = token() ?: return StepOutcome.AUTH_REQUIRED

        val body = CompleteRecordingRequestDto(
            chunkCount = chunkCount,
            stoppedAt = recording.stoppedAtEpochMs?.let { Instant.ofEpochMilli(it).toString() },
        )
        return when (val res = api.completeRecording(token.idToken, recordingId, body)) {
            is ApiResult.Success -> {
                repository.updateServerState(
                    clientId,
                    ServerRecordingState.fromWire(res.body.state).name,
                    res.body.transcriptId,
                    res.body.failureReason,
                )
                StepOutcome.SUCCESS
            }
            else -> mapNonSuccess(res.outcome) { repository.markFailed(clientId, "complete_failed") }
        }
    }

    /** A currently valid token, or null when the user must sign in again. */
    private suspend fun token(): StoredToken? = when (val result = tokens.idToken()) {
        is TokenResult.Valid -> result.token
        TokenResult.AuthNeeded, TokenResult.Unavailable -> null
    }

    private suspend inline fun mapNonSuccess(outcome: Outcome, onTerminal: () -> Unit): StepOutcome =
        when (outcome) {
            // The backend rejected a token we believed was valid: drop it so the next
            // attempt cannot replay the same token.
            Outcome.AUTH_NEEDED -> {
                tokens.invalidate()
                StepOutcome.AUTH_REQUIRED
            }
            Outcome.RETRYABLE -> StepOutcome.RETRY
            else -> {
                onTerminal()
                StepOutcome.FAILURE
            }
        }
}

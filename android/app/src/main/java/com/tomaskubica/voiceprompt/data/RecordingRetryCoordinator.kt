package com.tomaskubica.voiceprompt.data

import com.tomaskubica.voiceprompt.work.UploadWorkScheduler
import kotlinx.coroutines.CancellationException

/** Result of a user-initiated retry request. */
sealed interface RetryOutcome {
    /** The recording was reset and its upload chain rebuilt. */
    data object Scheduled : RetryOutcome

    /** Nothing to retry: unknown recording, capture still running, or already completed. */
    data object NotRetryable : RetryOutcome

    /** The reset or the scheduling failed; local audio and metadata are untouched. */
    data class Failed(val reason: String) : RetryOutcome
}

/**
 * Owns the user-initiated retry transition, which has to happen in one place because it spans
 * durable state and work scheduling.
 *
 * A terminal upload failure marks the recording [com.tomaskubica.voiceprompt.data.model
 * .LocalRecordingState.FAILED], and `UploadOrchestrator.complete` deliberately refuses to run for
 * a failed recording. Rebuilding the WorkManager chain alone therefore re-uploaded (and, via
 * ack-then-delete, removed) every WAV while `complete` kept returning failure forever — the
 * transcript could never be produced. So the durable state is reset *before* the chain is rebuilt:
 *
 *  1. atomically clear the sticky FAILED state + stale failure reason and stamp the retry marker,
 *  2. compute what the backend has not acknowledged yet,
 *  3. rebuild the unique per-recording chain.
 *
 * Step 1 preserves chunk rows, WAV files, per-chunk acked status, the server recording id and the
 * declared chunk count. If step 2/3 fails the recording is put back into the visible FAILED state
 * so the user can retry again — no captured audio is ever deleted.
 */
class RecordingRetryCoordinator(
    private val repository: RecordingRepository,
    private val scheduler: UploadWorkScheduler,
) {
    suspend fun retry(clientId: String): RetryOutcome {
        val recording = repository.prepareRetry(clientId) ?: return RetryOutcome.NotRetryable
        return try {
            val pending = repository.pendingChunkIndices(clientId)
            scheduler.retry(clientId, pending, hasComplete = recording.chunkCount != null)
            RetryOutcome.Scheduled
        } catch (ce: CancellationException) {
            throw ce
        } catch (t: Throwable) {
            // Keep the failure visible (and retryable) instead of silently pretending to retry.
            repository.markFailed(clientId, RETRY_SCHEDULE_FAILED)
            RetryOutcome.Failed(RETRY_SCHEDULE_FAILED)
        }
    }

    companion object {
        const val RETRY_SCHEDULE_FAILED = "retry_schedule_failed"
    }
}

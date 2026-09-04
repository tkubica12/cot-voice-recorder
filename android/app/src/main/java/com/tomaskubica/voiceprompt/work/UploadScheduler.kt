package com.tomaskubica.voiceprompt.work

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequest
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import java.time.Duration

/** Scheduling seam so capture logic can be unit tested without WorkManager. */
interface UploadWorkScheduler {
    fun startChain(clientId: String)

    fun enqueueChunk(clientId: String, index: Int)

    fun enqueueComplete(clientId: String)

    fun retry(clientId: String, pendingIndices: List<Int>, hasComplete: Boolean)
}

/**
 * Policy choices for the per-recording unique work chain, isolated so they can be asserted.
 *
 * `APPEND` was wrong: WorkManager immediately **cancels** work appended to a unique chain
 * whose existing work is `FAILED` or `CANCELLED`. One failed prerequisite therefore killed
 * every later chunk *and* the completion step for that recording, so audio captured after
 * the failure was never uploaded even though it was safely on disk.
 *
 * `APPEND_OR_REPLACE` keeps appending while the chain is healthy and rebuilds it when the
 * existing chain is dead. Because a rebuild drops the earlier steps, every appended segment
 * is prefixed with the *idempotent* `CreateSessionWorker` (an instant no-op once the server
 * recording id is known), so a rebuilt chain is still self-sufficient.
 */
internal object UploadWorkPolicy {
    /** Starting or restarting the chain for a new capture. */
    val START: ExistingWorkPolicy = ExistingWorkPolicy.APPEND_OR_REPLACE

    /** Appending a chunk or the completion step. */
    val APPEND: ExistingWorkPolicy = ExistingWorkPolicy.APPEND_OR_REPLACE

    /** Explicit user-initiated retry rebuilds the chain from scratch. */
    val RETRY: ExistingWorkPolicy = ExistingWorkPolicy.REPLACE
}

/**
 * Builds one deterministic, ordered WorkManager chain per recording:
 *
 *   create → chunk 0 → chunk 1 → … → complete
 *
 * The chain is keyed by a unique work name (`rec-<clientRecordingId>`). Chunks are appended in
 * capture order so uploads preserve ordering, and `complete` is appended last so it only runs
 * after every chunk worker has succeeded (complete-after-acks; `complete` also re-checks
 * `allChunksAcked` itself and retries). All workers require network and back off exponentially,
 * so cold start and transient loss are absorbed without losing work.
 */
class UploadScheduler(context: Context) : UploadWorkScheduler {
    private val wm = WorkManager.getInstance(context.applicationContext)

    private val networkConstraints = Constraints.Builder()
        .setRequiredNetworkType(NetworkType.CONNECTED)
        .build()

    private fun uniqueName(clientId: String) = "rec-$clientId"

    private inline fun <reified W : androidx.work.ListenableWorker> request(data: Data) =
        OneTimeWorkRequestBuilder<W>()
            .setConstraints(networkConstraints)
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, Duration.ofSeconds(10))
            .setInputData(data)
            .build()

    private fun createRequest(clientId: String): OneTimeWorkRequest =
        request<CreateSessionWorker>(Data.Builder().putString(KEY_CLIENT_ID, clientId).build())

    /** Start (or restart) the chain with the session-create step. */
    override fun startChain(clientId: String) {
        wm.beginUniqueWork(uniqueName(clientId), UploadWorkPolicy.START, createRequest(clientId))
            .enqueue()
    }

    /** Append a chunk upload to the recording's chain (runs after create + prior chunks). */
    override fun enqueueChunk(clientId: String, index: Int) {
        val work = request<UploadChunkWorker>(
            Data.Builder().putString(KEY_CLIENT_ID, clientId).putInt(KEY_INDEX, index).build(),
        )
        appendAfterCreate(clientId, work)
    }

    /** Append the completion step (runs after all chunk uploads succeed). */
    override fun enqueueComplete(clientId: String) {
        val work = request<CompleteWorker>(
            Data.Builder().putString(KEY_CLIENT_ID, clientId).build(),
        )
        appendAfterCreate(clientId, work)
    }

    /**
     * Append `create → work`. The create step is idempotent and returns immediately once the
     * server recording id is known, so it costs nothing on a healthy chain but guarantees the
     * segment can still run if `APPEND_OR_REPLACE` had to rebuild a dead chain.
     */
    private fun appendAfterCreate(clientId: String, work: OneTimeWorkRequest) {
        wm.beginUniqueWork(uniqueName(clientId), UploadWorkPolicy.APPEND, createRequest(clientId))
            .then(work)
            .enqueue()
    }

    /** User-initiated retry: rebuild the whole chain, replacing any cancelled/failed one. */
    override fun retry(clientId: String, pendingIndices: List<Int>, hasComplete: Boolean) {
        wm.beginUniqueWork(uniqueName(clientId), UploadWorkPolicy.RETRY, createRequest(clientId))
            .enqueue()
        pendingIndices.forEach { enqueueChunk(clientId, it) }
        if (hasComplete) enqueueComplete(clientId)
    }

    companion object {
        const val KEY_CLIENT_ID = "client_recording_id"
        const val KEY_INDEX = "index"
    }
}

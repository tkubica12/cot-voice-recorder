package com.tomaskubica.voiceprompt.work

import android.content.Context
import android.util.Log
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import com.google.common.truth.Truth.assertThat
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.util.UUID
import kotlinx.coroutines.runBlocking

class AlwaysFailWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result = Result.failure()
}

class AlwaysSucceedWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result = Result.success()
}

/**
 * Pins the unique-work policy used by [UploadScheduler].
 *
 * The first two tests capture the actual WorkManager semantics that caused the bug: work
 * appended with `APPEND` to a chain whose existing work has FAILED inherits that terminal
 * state and never runs, so one failed prerequisite silently killed every later chunk and the
 * completion step. The remaining tests pin the scheduler's own shape.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class UploadSchedulerPolicyTest {

    private lateinit var context: Context
    private lateinit var wm: WorkManager

    @Before fun setUp() {
        context = ApplicationProvider.getApplicationContext()
        val config = Configuration.Builder()
            .setMinimumLoggingLevel(Log.DEBUG)
            .setExecutor(SynchronousExecutor())
            .build()
        WorkManagerTestInitHelper.initializeTestWorkManager(context, config)
        wm = WorkManager.getInstance(context)
    }

    private fun state(id: UUID): WorkInfo.State? = wm.getWorkInfoById(id).get()?.state

    @Test fun append_cancels_work_added_after_a_failed_prerequisite() {
        val name = "chain-append"
        val failing = OneTimeWorkRequestBuilder<AlwaysFailWorker>().build()
        wm.beginUniqueWork(name, ExistingWorkPolicy.REPLACE, failing).enqueue().result.get()
        assertThat(state(failing.id)).isEqualTo(WorkInfo.State.FAILED)

        val later = OneTimeWorkRequestBuilder<AlwaysSucceedWorker>().build()
        wm.beginUniqueWork(name, ExistingWorkPolicy.APPEND, later).enqueue().result.get()

        // This is the cascade the app used to suffer from: work appended to a dead unique
        // chain inherits its terminal state (FAILED or CANCELLED depending on the WorkManager
        // version) and never executes — so later chunks and `complete` were silently dropped.
        assertThat(state(later.id))
            .isAnyOf(WorkInfo.State.FAILED, WorkInfo.State.CANCELLED)
        assertThat(state(later.id)).isNotEqualTo(WorkInfo.State.SUCCEEDED)
    }

    @Test fun append_terminal_state_does_not_cascade_with_append_or_replace() {
        val name = "chain-append-or-replace"
        val failing = OneTimeWorkRequestBuilder<AlwaysFailWorker>().build()
        wm.beginUniqueWork(name, ExistingWorkPolicy.REPLACE, failing).enqueue().result.get()
        assertThat(state(failing.id)).isEqualTo(WorkInfo.State.FAILED)

        val later = OneTimeWorkRequestBuilder<AlwaysSucceedWorker>().build()
        wm.beginUniqueWork(name, UploadWorkPolicy.APPEND, later).enqueue().result.get()

        assertThat(state(later.id)).isEqualTo(WorkInfo.State.SUCCEEDED)
    }

    @Test fun append_or_replace_still_appends_to_a_healthy_chain() {
        val name = "chain-healthy"
        val first = OneTimeWorkRequestBuilder<AlwaysSucceedWorker>().build()
        wm.beginUniqueWork(name, ExistingWorkPolicy.REPLACE, first).enqueue().result.get()

        val second = OneTimeWorkRequestBuilder<AlwaysSucceedWorker>().build()
        wm.beginUniqueWork(name, UploadWorkPolicy.APPEND, second).enqueue().result.get()

        // Both steps are retained: appending did not discard the earlier success.
        val infos = wm.getWorkInfosForUniqueWork(name).get()
        assertThat(infos.map { it.id }).containsAtLeast(first.id, second.id)
        assertThat(state(second.id)).isEqualTo(WorkInfo.State.SUCCEEDED)
    }

    @Test fun policies_are_the_non_cascading_ones() {
        assertThat(UploadWorkPolicy.START).isEqualTo(ExistingWorkPolicy.APPEND_OR_REPLACE)
        assertThat(UploadWorkPolicy.APPEND).isEqualTo(ExistingWorkPolicy.APPEND_OR_REPLACE)
        // A user-initiated retry deliberately rebuilds the chain from scratch.
        assertThat(UploadWorkPolicy.RETRY).isEqualTo(ExistingWorkPolicy.REPLACE)
    }

    @Test fun scheduler_builds_one_chain_per_recording_prefixed_with_idempotent_create() {
        val scheduler = UploadScheduler(context)
        val clientId = "cid-policy"

        scheduler.startChain(clientId)
        scheduler.enqueueChunk(clientId, 0)
        scheduler.enqueueChunk(clientId, 1)
        scheduler.enqueueComplete(clientId)

        val infos = wm.getWorkInfosForUniqueWork("rec-$clientId").get()
        // create + (create,chunk0) + (create,chunk1) + (create,complete): every appended
        // segment carries the no-op create so a rebuilt chain is still self-sufficient.
        assertThat(infos).hasSize(7)
        // Network-constrained work must not have run yet in the test harness.
        assertThat(infos.none { it.state == WorkInfo.State.CANCELLED }).isTrue()
    }

    @Test fun chains_of_different_recordings_are_independent() {
        val scheduler = UploadScheduler(context)
        scheduler.startChain("a")
        scheduler.startChain("b")

        assertThat(wm.getWorkInfosForUniqueWork("rec-a").get()).hasSize(1)
        assertThat(wm.getWorkInfosForUniqueWork("rec-b").get()).hasSize(1)
    }

    @Test fun retry_replaces_the_chain_and_re_enqueues_pending_work() = runBlocking {
        val scheduler = UploadScheduler(context)
        val clientId = "cid-retry"
        scheduler.startChain(clientId)
        scheduler.enqueueChunk(clientId, 0)

        scheduler.retry(clientId, pendingIndices = listOf(0, 1), hasComplete = true)

        val infos = wm.getWorkInfosForUniqueWork("rec-$clientId").get()
        // One atomic replacement: create -> chunk0 -> chunk1 -> complete, with fresh backoff.
        val pending = infos.filter { it.state != WorkInfo.State.CANCELLED }
        assertThat(pending).hasSize(4)
        assertThat(pending.map { it.runAttemptCount }).containsExactly(0, 0, 0, 0)
        assertThat(pending.count { it.state == WorkInfo.State.ENQUEUED }).isEqualTo(1)
        assertThat(pending.count { it.state == WorkInfo.State.BLOCKED }).isEqualTo(3)
    }
}

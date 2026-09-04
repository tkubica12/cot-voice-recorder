package com.tomaskubica.voiceprompt.data

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.audio.StreamingChunker
import com.tomaskubica.voiceprompt.data.db.AppDatabase
import com.tomaskubica.voiceprompt.data.model.ChunkUploadState
import com.tomaskubica.voiceprompt.data.model.LocalRecordingState
import com.tomaskubica.voiceprompt.testutil.RecordingScheduler
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class RecordingRetryCoordinatorTest {

    private lateinit var db: AppDatabase
    private lateinit var repo: RecordingRepository
    private lateinit var scheduler: RecordingScheduler
    private lateinit var coordinator: RecordingRetryCoordinator

    private val clientId = "cid-coord"

    @Before fun setUp() {
        val ctx = ApplicationProvider.getApplicationContext<android.content.Context>()
        db = Room.inMemoryDatabaseBuilder(ctx, AppDatabase::class.java)
            .allowMainThreadQueries().build()
        val dir = File(ctx.cacheDir, "retry-coord-test")
        dir.deleteRecursively()
        dir.mkdirs()
        repo = RecordingRepository(db.recordingDao(), db.chunkDao(), ChunkFileStore(dir))
        scheduler = RecordingScheduler()
        coordinator = RecordingRetryCoordinator(repo, scheduler)
    }

    @After fun tearDown() = db.close()

    private fun raw(index: Int) = StreamingChunker.RawChunk(
        index = index, startSample = index * 456_000L, lengthSamples = 480_000L,
        pcm = ByteArray(200) { it.toByte() }, hasPriorOverlap = index > 0,
    )

    private suspend fun seedFailedAfterStop() {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        val c0 = repo.persistChunk(clientId, raw(0), 1_000)
        repo.persistChunk(clientId, raw(1), 1_000)
        repo.markStopped(clientId, chunkCount = 2, stoppedAtEpochMs = 2_000)
        repo.markChunkAcked(c0)
        repo.markFailed(clientId, "chunk_conflict")
    }

    @Test fun retry_resets_the_state_before_rebuilding_the_chain() = runBlocking {
        seedFailedAfterStop()

        val outcome = coordinator.retry(clientId)

        assertThat(outcome).isEqualTo(RetryOutcome.Scheduled)
        val r = repo.getRecording(clientId)!!
        // The sticky FAILED state is gone, otherwise `complete` would refuse forever.
        assertThat(r.localState).isEqualTo(LocalRecordingState.COMPLETING.name)
        assertThat(r.failureReason).isNull()
        assertThat(r.retryRequestedAtEpochMs).isNotNull()
    }

    @Test fun retry_reschedules_only_unacknowledged_chunks_plus_complete() = runBlocking {
        seedFailedAfterStop()

        coordinator.retry(clientId)

        assertThat(scheduler.retries).hasSize(1)
        val (id, pending, hasComplete) = scheduler.retries.single()
        assertThat(id).isEqualTo(clientId)
        assertThat(pending).containsExactly(1)
        assertThat(hasComplete).isTrue()
    }

    @Test fun retry_without_a_declared_chunk_count_does_not_schedule_complete() = runBlocking {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        repo.persistChunk(clientId, raw(0), 1_000)
        repo.markFailed(clientId, "create_failed")

        assertThat(coordinator.retry(clientId)).isEqualTo(RetryOutcome.Scheduled)

        val (_, pending, hasComplete) = scheduler.retries.single()
        assertThat(pending).containsExactly(0)
        assertThat(hasComplete).isFalse()
    }

    @Test fun a_scheduling_failure_is_surfaced_and_never_deletes_captured_audio() = runBlocking {
        seedFailedAfterStop()
        val unsentPath = repo.getChunk(clientId, 1)!!.filePath!!
        scheduler.failWith = IllegalStateException("WorkManager unavailable")

        val outcome = coordinator.retry(clientId)

        assertThat(outcome)
            .isEqualTo(RetryOutcome.Failed(RecordingRetryCoordinator.RETRY_SCHEDULE_FAILED))
        // Visible + retryable again, and nothing was thrown away.
        val r = repo.getRecording(clientId)!!
        assertThat(r.localState).isEqualTo(LocalRecordingState.FAILED.name)
        assertThat(r.failureReason).isEqualTo(RecordingRetryCoordinator.RETRY_SCHEDULE_FAILED)
        assertThat(r.chunkCount).isEqualTo(2)
        assertThat(r.recordingId).isEqualTo("rid-1")
        assertThat(File(unsentPath).exists()).isTrue()
        assertThat(repo.chunksFor(clientId)).hasSize(2)
        assertThat(repo.getChunk(clientId, 0)!!.uploadState).isEqualTo(ChunkUploadState.ACKED.name)
    }

    @Test fun retrying_an_unknown_recording_schedules_nothing() = runBlocking {
        assertThat(coordinator.retry("nope")).isEqualTo(RetryOutcome.NotRetryable)
        assertThat(scheduler.retries).isEmpty()
    }
}

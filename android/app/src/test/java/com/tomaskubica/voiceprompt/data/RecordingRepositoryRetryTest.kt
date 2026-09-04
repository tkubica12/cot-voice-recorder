package com.tomaskubica.voiceprompt.data

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.audio.StreamingChunker
import com.tomaskubica.voiceprompt.data.db.AppDatabase
import com.tomaskubica.voiceprompt.data.model.ChunkUploadState
import com.tomaskubica.voiceprompt.data.model.LocalRecordingState
import com.tomaskubica.voiceprompt.data.model.ServerRecordingState
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File

/**
 * Regression coverage for the user-retry state transition.
 *
 * Before the fix a terminal upload failure left `localState = FAILED` forever: retrying rebuilt
 * the WorkManager chain, re-uploaded (and deleted) every WAV, and `complete` still refused
 * because of the sticky FAILED state — so the recording could never produce a transcript.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class RecordingRepositoryRetryTest {

    private lateinit var db: AppDatabase
    private lateinit var repo: RecordingRepository
    private lateinit var fileStore: ChunkFileStore
    private var now = 10_000L

    private val clientId = "cid-retry"

    @Before fun setUp() {
        val ctx = ApplicationProvider.getApplicationContext<android.content.Context>()
        db = Room.inMemoryDatabaseBuilder(ctx, AppDatabase::class.java)
            .allowMainThreadQueries().build()
        val dir = File(ctx.cacheDir, "retry-repo-test")
        dir.deleteRecursively()
        dir.mkdirs()
        fileStore = ChunkFileStore(dir)
        repo = RecordingRepository(db.recordingDao(), db.chunkDao(), fileStore) { now }
    }

    @After fun tearDown() = db.close()

    private fun raw(index: Int) = StreamingChunker.RawChunk(
        index = index, startSample = index * 456_000L, lengthSamples = 480_000L,
        pcm = ByteArray(200) { it.toByte() }, hasPriorOverlap = index > 0,
    )

    /** A stopped recording whose second chunk hit a terminal failure. */
    private suspend fun seedFailedAfterStop() {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        val c0 = repo.persistChunk(clientId, raw(0), 1_000)
        val c1 = repo.persistChunk(clientId, raw(1), 1_000)
        repo.markStopped(clientId, chunkCount = 2, stoppedAtEpochMs = 2_000)
        repo.markChunkAcked(c0)
        repo.markChunkFailed(c1)
        repo.markFailed(clientId, "chunk_conflict")
    }

    @Test fun prepareRetry_clears_sticky_failed_state_and_failure_reason() = runBlocking {
        seedFailedAfterStop()
        assertThat(repo.getRecording(clientId)!!.localState).isEqualTo(LocalRecordingState.FAILED.name)

        now = 20_000L
        val reset = repo.prepareRetry(clientId)

        assertThat(reset).isNotNull()
        assertThat(reset!!.localState).isEqualTo(LocalRecordingState.COMPLETING.name)
        assertThat(reset.failureReason).isNull()
        assertThat(reset.retryRequestedAtEpochMs).isEqualTo(20_000L)
    }

    @Test fun prepareRetry_preserves_server_id_chunk_count_and_stopped_at() = runBlocking {
        seedFailedAfterStop()

        val reset = repo.prepareRetry(clientId)!!

        assertThat(reset.recordingId).isEqualTo("rid-1")
        assertThat(reset.chunkCount).isEqualTo(2)
        assertThat(reset.stoppedAtEpochMs).isEqualTo(2_000L)
        assertThat(reset.serverState).isEqualTo("recording")
    }

    @Test fun prepareRetry_keeps_every_chunk_row_its_acked_status_and_the_unsent_wav() = runBlocking<Unit> {
        seedFailedAfterStop()
        val unsentPath = repo.getChunk(clientId, 1)!!.filePath
        assertThat(unsentPath).isNotNull()

        repo.prepareRetry(clientId)

        val chunks = repo.chunksFor(clientId)
        assertThat(chunks).hasSize(2)
        // Already acknowledged chunk stays acknowledged: it must not be re-uploaded.
        assertThat(chunks[0].uploadState).isEqualTo(ChunkUploadState.ACKED.name)
        // The chunk that failed still has its audio on disk — nothing was deleted.
        assertThat(chunks[1].filePath).isEqualTo(unsentPath)
        assertThat(File(unsentPath!!).exists()).isTrue()
        assertThat(repo.pendingChunkIndices(clientId)).containsExactly(1)
    }

    @Test fun markChunkAcked_persists_recoverable_state_and_removes_the_wav() = runBlocking {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        val chunk = repo.persistChunk(clientId, raw(0), 1_000)
        val originalPath = chunk.filePath!!
        assertThat(File(originalPath).exists()).isTrue()

        repo.markChunkAcked(chunk)

        val acknowledged = repo.getChunk(clientId, 0)!!
        assertThat(acknowledged.uploadState).isEqualTo(ChunkUploadState.ACKED.name)
        assertThat(acknowledged.filePath).isNull()
        assertThat(File(originalPath).exists()).isFalse()
        assertThat(repo.pendingChunkIndices(clientId)).isEmpty()
    }

    @Test fun prepareRetry_before_stop_returns_to_stopped_not_completing() = runBlocking {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        repo.persistChunk(clientId, raw(0), 1_000)
        repo.markFailed(clientId, "create_failed")

        val reset = repo.prepareRetry(clientId)!!

        // No chunkCount was declared yet, so `complete` is not applicable: STOPPED, not COMPLETING.
        assertThat(reset.localState).isEqualTo(LocalRecordingState.STOPPED.name)
        assertThat(reset.chunkCount).isNull()
    }

    @Test fun prepareRetry_does_not_disturb_a_capture_in_progress() = runBlocking {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)

        assertThat(repo.prepareRetry(clientId)).isNull()
        assertThat(repo.getRecording(clientId)!!.localState).isEqualTo(LocalRecordingState.RECORDING.name)
    }

    @Test fun prepareRetry_is_a_noop_for_completed_recordings() = runBlocking {
        seedFailedAfterStop()
        repo.updateServerState(clientId, ServerRecordingState.COMPLETED.name, "tid-1", null)

        assertThat(repo.prepareRetry(clientId)).isNull()
        val r = repo.getRecording(clientId)!!
        assertThat(r.localState).isEqualTo(LocalRecordingState.COMPLETED.name)
        assertThat(r.transcriptId).isEqualTo("tid-1")
    }

    @Test fun prepareRetry_of_an_unknown_recording_returns_null() = runBlocking {
        assertThat(repo.prepareRetry("nope")).isNull()
    }

    @Test fun retry_marker_is_cleared_when_the_upload_fails_again() = runBlocking {
        seedFailedAfterStop()
        assertThat(repo.prepareRetry(clientId)!!.retryRequestedAtEpochMs).isNotNull()

        repo.markFailed(clientId, "complete_failed")

        assertThat(repo.getRecording(clientId)!!.retryRequestedAtEpochMs).isNull()
    }

    @Test fun retry_marker_survives_progress_but_clears_on_completion() = runBlocking {
        seedFailedAfterStop()
        repo.prepareRetry(clientId)

        repo.updateServerState(clientId, ServerRecordingState.UPLOADING.name, null, null)
        assertThat(repo.getRecording(clientId)!!.retryRequestedAtEpochMs).isNotNull()

        repo.updateServerState(clientId, ServerRecordingState.COMPLETED.name, "tid-1", null)
        assertThat(repo.getRecording(clientId)!!.retryRequestedAtEpochMs).isNull()
    }
}

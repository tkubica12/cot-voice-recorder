package com.tomaskubica.voiceprompt.work

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.audio.StreamingChunker
import com.tomaskubica.voiceprompt.data.ChunkFileStore
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.RecordingRetryCoordinator
import com.tomaskubica.voiceprompt.data.RetryOutcome
import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import com.tomaskubica.voiceprompt.data.db.AppDatabase
import com.tomaskubica.voiceprompt.data.model.ChunkUploadState
import com.tomaskubica.voiceprompt.data.model.LocalRecordingState
import com.tomaskubica.voiceprompt.data.model.ServerRecordingState
import com.tomaskubica.voiceprompt.testutil.FakeAuthTokenProvider
import com.tomaskubica.voiceprompt.testutil.RecordingScheduler
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File

/**
 * End-to-end regression for the sticky-FAILED bug.
 *
 * A terminal chunk failure marks the recording FAILED. `UploadOrchestrator.complete` refuses to
 * run for a FAILED recording (deliberately — an unrecoverable failure must not retry silently
 * forever), but the user-facing retry used to rebuild the WorkManager chain **without** clearing
 * that state. The retried chunk uploads then acked and deleted every WAV while `complete` kept
 * returning FAILURE: the audio was gone and no transcript could ever be produced.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class UploadOrchestratorRetryTest {

    private lateinit var db: AppDatabase
    private lateinit var repo: RecordingRepository
    private lateinit var server: MockWebServer
    private lateinit var orchestrator: UploadOrchestrator
    private lateinit var scheduler: RecordingScheduler
    private lateinit var coordinator: RecordingRetryCoordinator

    private val clientId = "cid-retry-e2e"

    @Before fun setUp() {
        val ctx = ApplicationProvider.getApplicationContext<android.content.Context>()
        db = Room.inMemoryDatabaseBuilder(ctx, AppDatabase::class.java)
            .allowMainThreadQueries().build()
        val dir = File(ctx.cacheDir, "orch-retry-test")
        dir.deleteRecursively()
        dir.mkdirs()
        repo = RecordingRepository(db.recordingDao(), db.chunkDao(), ChunkFileStore(dir))
        server = MockWebServer().apply { start() }
        orchestrator = UploadOrchestrator(repo, VoiceApiClient({ server.url("/").toString() }), FakeAuthTokenProvider())
        scheduler = RecordingScheduler()
        coordinator = RecordingRetryCoordinator(repo, scheduler)
    }

    @After fun tearDown() {
        db.close()
        server.shutdown()
    }

    private fun recordingJson(state: String, transcriptId: String? = null) = """
        {"recording_id":"rid-1","client_recording_id":"$clientId","state":"$state",
         "refine_model":"gpt-5.6-luna","language":"cs",
         ${transcriptId?.let { "\"transcript_id\":\"$it\"," } ?: ""}
         "progress":{"received_chunk_count":0,"transcribed_chunk_count":0},
         "created_at":"2026-09-03T13:52:01Z","updated_at":"2026-09-03T13:52:01Z"}
    """.trimIndent()

    private fun chunkAcceptedJson(index: Int) = """
        {"recording_id":"rid-1","index":$index,"chunk_state":"accepted",
         "checksum":"sha-256=:x:","received_at":"2026-09-03T13:52:05Z"}
    """.trimIndent()

    private fun raw(index: Int) = StreamingChunker.RawChunk(
        index = index, startSample = index * 456_000L, lengthSamples = 480_000L,
        pcm = ByteArray(200) { it.toByte() }, hasPriorOverlap = index > 0,
    )

    /** Capture two chunks, ack the first, hit a terminal failure on the second. */
    private suspend fun captureThenFailTerminally(): String {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        repo.persistChunk(clientId, raw(0), 1_000)
        repo.persistChunk(clientId, raw(1), 1_000)
        repo.markStopped(clientId, chunkCount = 2, stoppedAtEpochMs = 2_000)

        server.enqueue(MockResponse().setResponseCode(202).setBody(chunkAcceptedJson(0)))
        assertThat(orchestrator.uploadChunk(clientId, 0)).isEqualTo(StepOutcome.SUCCESS)

        server.enqueue(MockResponse().setResponseCode(409).setBody("""{"title":"Chunk conflict","status":409}"""))
        assertThat(orchestrator.uploadChunk(clientId, 1)).isEqualTo(StepOutcome.FAILURE)
        assertThat(repo.getRecording(clientId)!!.localState).isEqualTo(LocalRecordingState.FAILED.name)

        return repo.getChunk(clientId, 1)!!.filePath!!
    }

    @Test fun complete_still_refuses_for_a_failed_recording_that_was_not_retried() = runBlocking {
        captureThenFailTerminally()

        // The guard is intentionally kept: without a user retry this stays terminal.
        assertThat(orchestrator.complete(clientId)).isEqualTo(StepOutcome.FAILURE)
    }

    @Test fun user_retry_resets_state_so_pending_upload_and_complete_finally_succeed() = runBlocking {
        val unsentWav = captureThenFailTerminally()
        assertThat(orchestrator.complete(clientId)).isEqualTo(StepOutcome.FAILURE) // sticky before retry

        // --- the operator/auth/network problem is fixed and the user taps Retry ---
        assertThat(coordinator.retry(clientId)).isEqualTo(RetryOutcome.Scheduled)
        assertThat(scheduler.retries.single().second).containsExactly(1) // only the unacked chunk

        // The audio that was never accepted is still on disk, ready to be re-sent.
        assertThat(File(unsentWav).exists()).isTrue()

        server.enqueue(MockResponse().setResponseCode(202).setBody(chunkAcceptedJson(1)))
        assertThat(orchestrator.uploadChunk(clientId, 1)).isEqualTo(StepOutcome.SUCCESS)

        server.enqueue(MockResponse().setResponseCode(202).setBody(recordingJson("transcribing")))
        assertThat(orchestrator.complete(clientId)).isEqualTo(StepOutcome.SUCCESS)

        val r = repo.getRecording(clientId)!!
        assertThat(r.localState).isNotEqualTo(LocalRecordingState.FAILED.name)
        assertThat(r.serverState).isEqualTo(ServerRecordingState.TRANSCRIBING.name)
        assertThat(r.failureReason).isNull()
    }

    @Test fun retry_never_loses_audio_and_never_re_uploads_acknowledged_chunks() = runBlocking {
        captureThenFailTerminally()
        val requestsBeforeRetry = server.requestCount

        coordinator.retry(clientId)

        // Every captured chunk row survived the reset.
        assertThat(repo.chunksFor(clientId)).hasSize(2)
        assertThat(repo.getChunk(clientId, 0)!!.uploadState).isEqualTo(ChunkUploadState.ACKED.name)

        // Replaying chunk 0 is a local no-op — no duplicate upload, no 409 storm.
        assertThat(orchestrator.uploadChunk(clientId, 0)).isEqualTo(StepOutcome.SUCCESS)
        assertThat(server.requestCount).isEqualTo(requestsBeforeRetry)

        server.enqueue(MockResponse().setResponseCode(202).setBody(chunkAcceptedJson(1)))
        assertThat(orchestrator.uploadChunk(clientId, 1)).isEqualTo(StepOutcome.SUCCESS)
        assertThat(repo.allChunksAcked(clientId, 2)).isTrue()
    }

    @Test fun completion_after_a_retry_clears_the_retrying_marker() = runBlocking {
        captureThenFailTerminally()
        coordinator.retry(clientId)
        assertThat(repo.getRecording(clientId)!!.retryRequestedAtEpochMs).isNotNull()

        server.enqueue(MockResponse().setResponseCode(202).setBody(chunkAcceptedJson(1)))
        orchestrator.uploadChunk(clientId, 1)
        server.enqueue(MockResponse().setResponseCode(202).setBody(recordingJson("completed", "tid-9")))
        assertThat(orchestrator.complete(clientId)).isEqualTo(StepOutcome.SUCCESS)

        val r = repo.getRecording(clientId)!!
        assertThat(r.localState).isEqualTo(LocalRecordingState.COMPLETED.name)
        assertThat(r.transcriptId).isEqualTo("tid-9")
        assertThat(r.retryRequestedAtEpochMs).isNull()
    }
}

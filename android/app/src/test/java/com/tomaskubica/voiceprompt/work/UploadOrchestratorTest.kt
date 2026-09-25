package com.tomaskubica.voiceprompt.work

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.audio.StreamingChunker
import com.tomaskubica.voiceprompt.data.ChunkFileStore
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import com.tomaskubica.voiceprompt.testutil.FakeAuthTokenProvider
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

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class UploadOrchestratorTest {

    private lateinit var db: com.tomaskubica.voiceprompt.data.db.AppDatabase
    private lateinit var repo: RecordingRepository
    private lateinit var server: MockWebServer
    private lateinit var tokens: FakeAuthTokenProvider
    private lateinit var orchestrator: UploadOrchestrator

    private val clientId = "cid-1"

    @Before fun setUp() {
        val ctx = ApplicationProvider.getApplicationContext<android.content.Context>()
        db = Room.inMemoryDatabaseBuilder(ctx, com.tomaskubica.voiceprompt.data.db.AppDatabase::class.java)
            .allowMainThreadQueries().build()
        val fileStore = ChunkFileStore(File(ctx.cacheDir, "orch-test").apply { mkdirs() })
        repo = RecordingRepository(db.recordingDao(), db.chunkDao(), fileStore)
        server = MockWebServer().apply { start() }
        tokens = FakeAuthTokenProvider()
        val api = VoiceApiClient({ server.url("/").toString() })
        orchestrator = UploadOrchestrator(repo, api, tokens)
    }

    @After fun tearDown() {
        db.close()
        server.shutdown()
    }

    private fun recordingJson(state: String) = """
        {"recording_id":"rid-1","client_recording_id":"$clientId","state":"$state",
         "refine_model":"gpt-5.6-luna","language":"cs",
         "progress":{"received_chunk_count":0,"transcribed_chunk_count":0},
         "created_at":"2026-09-03T13:52:01Z","updated_at":"2026-09-03T13:52:01Z"}
    """.trimIndent()

    private fun chunkAcceptedJson(index: Int) = """
        {"recording_id":"rid-1","index":$index,"chunk_state":"accepted",
         "checksum":"sha-256=:x:","received_at":"2026-09-03T13:52:05Z"}
    """.trimIndent()

    private suspend fun seedRecording() =
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)

    private fun raw(index: Int) = StreamingChunker.RawChunk(
        index = index, startSample = index * 456_000L, lengthSamples = 480_000L,
        pcm = ByteArray(200) { it.toByte() }, hasPriorOverlap = index > 0,
    )

    @Test fun create_is_idempotent_when_recording_id_already_set() = runBlocking {
        seedRecording()
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        val outcome = orchestrator.createSession(clientId)
        assertThat(outcome).isEqualTo(StepOutcome.SUCCESS)
        assertThat(server.requestCount).isEqualTo(0) // no network for replay
    }

    @Test fun create_retries_through_cold_start_then_succeeds() = runBlocking {
        seedRecording()
        server.enqueue(MockResponse().setResponseCode(503).setBody("""{"status":"not_ready"}"""))
        assertThat(orchestrator.createSession(clientId)).isEqualTo(StepOutcome.RETRY)

        server.enqueue(MockResponse().setResponseCode(201).setBody(recordingJson("recording")))
        assertThat(orchestrator.createSession(clientId)).isEqualTo(StepOutcome.SUCCESS)
        assertThat(repo.recordingIdFor(clientId)).isEqualTo("rid-1")
        assertThat(server.takeRequest().body.readUtf8()).contains("\"refine_model\":\"gpt-5.6-luna\"")
        assertThat(server.takeRequest().body.readUtf8()).contains("\"refine_model\":\"gpt-5.6-luna\"")
    }

    @Test fun create_without_token_reports_auth_required_and_makes_no_request() = runBlocking {
        seedRecording()
        tokens.result = com.tomaskubica.voiceprompt.auth.TokenResult.AuthNeeded
        assertThat(orchestrator.createSession(clientId)).isEqualTo(StepOutcome.AUTH_REQUIRED)
        assertThat(server.requestCount).isEqualTo(0)
    }

    @Test fun create_with_401_invalidates_the_token_and_reports_auth_required() = runBlocking {
        seedRecording()
        server.enqueue(MockResponse().setResponseCode(401).setBody("""{"title":"Unauthorized","status":401}"""))
        assertThat(orchestrator.createSession(clientId)).isEqualTo(StepOutcome.AUTH_REQUIRED)
        // The rejected token is dropped so the next attempt performs a real refresh.
        assertThat(tokens.invalidations).isEqualTo(1)
    }

    @Test fun upload_uses_the_refreshed_token_never_a_stale_one() = runBlocking {
        seedRecording()
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        repo.persistChunk(clientId, raw(0), 1_000)
        tokens.result = com.tomaskubica.voiceprompt.auth.TokenResult.Valid(
            com.tomaskubica.voiceprompt.auth.StoredToken("fresh-token", "owner@example.com", Long.MAX_VALUE),
        )

        server.enqueue(MockResponse().setResponseCode(202).setBody(chunkAcceptedJson(0)))
        assertThat(orchestrator.uploadChunk(clientId, 0)).isEqualTo(StepOutcome.SUCCESS)

        val sent = server.takeRequest()
        assertThat(sent.getHeader("Authorization")).contains("fresh-token")
    }

    @Test fun upload_replay_is_success_without_network_when_acked() = runBlocking {
        seedRecording()
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        val c = repo.persistChunk(clientId, raw(0), 1_000)
        repo.markChunkAcked(c)
        assertThat(orchestrator.uploadChunk(clientId, 0)).isEqualTo(StepOutcome.SUCCESS)
        assertThat(server.requestCount).isEqualTo(0)
    }

    @Test fun uploads_preserve_order_and_delete_after_ack() = runBlocking {
        seedRecording()
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        repo.persistChunk(clientId, raw(0), 1_000)
        repo.persistChunk(clientId, raw(1), 1_000)

        server.enqueue(MockResponse().setResponseCode(202).setBody(chunkAcceptedJson(0)))
        server.enqueue(MockResponse().setResponseCode(202).setBody(chunkAcceptedJson(1)))

        assertThat(orchestrator.uploadChunk(clientId, 0)).isEqualTo(StepOutcome.SUCCESS)
        assertThat(orchestrator.uploadChunk(clientId, 1)).isEqualTo(StepOutcome.SUCCESS)

        assertThat(server.takeRequest().path).isEqualTo("/v1/recordings/rid-1/chunks/0")
        assertThat(server.takeRequest().path).isEqualTo("/v1/recordings/rid-1/chunks/1")

        // ack-then-delete: local WAV removed and marked acked
        assertThat(repo.getChunk(clientId, 0)!!.filePath).isNull()
        assertThat(repo.getChunk(clientId, 1)!!.filePath).isNull()
    }

    @Test fun complete_waits_for_acks_then_succeeds() = runBlocking {
        seedRecording()
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        repo.persistChunk(clientId, raw(0), 1_000)
        repo.markStopped(clientId, chunkCount = 1, stoppedAtEpochMs = 2_000)

        // Chunk not yet acked -> complete must wait.
        assertThat(orchestrator.complete(clientId)).isEqualTo(StepOutcome.RETRY)

        server.enqueue(MockResponse().setResponseCode(202).setBody(chunkAcceptedJson(0)))
        assertThat(orchestrator.uploadChunk(clientId, 0)).isEqualTo(StepOutcome.SUCCESS)

        server.enqueue(MockResponse().setResponseCode(202).setBody(recordingJson("uploading")))
        assertThat(orchestrator.complete(clientId)).isEqualTo(StepOutcome.SUCCESS)

        // last request is the complete call
        server.takeRequest() // chunk PUT
        assertThat(server.takeRequest().path).isEqualTo("/v1/recordings/rid-1/complete")
    }

    @Test fun upload_conflict_is_terminal_and_fails_recording() = runBlocking {
        seedRecording()
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        repo.persistChunk(clientId, raw(0), 1_000)

        server.enqueue(MockResponse().setResponseCode(409).setBody("""{"title":"Chunk conflict","status":409}"""))
        assertThat(orchestrator.uploadChunk(clientId, 0)).isEqualTo(StepOutcome.FAILURE)
        assertThat(repo.getRecording(clientId)!!.localState).isEqualTo("FAILED")
    }
}

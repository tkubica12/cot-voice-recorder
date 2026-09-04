package com.tomaskubica.voiceprompt.data.api

import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.data.model.Outcome
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.After
import org.junit.Before
import org.junit.Test

class VoiceApiClientTest {

    private lateinit var server: MockWebServer
    private lateinit var client: VoiceApiClient

    @Before fun setUp() {
        server = MockWebServer()
        server.start()
        client = VoiceApiClient({ server.url("/").toString() })
    }

    @After fun tearDown() { server.shutdown() }

    private fun recordingJson(state: String) = """
        {"recording_id":"rid-1","client_recording_id":"cid-1","state":"$state",
         "refine_model":"gpt-5.6-luna","language":"cs",
         "progress":{"expected_chunk_count":null,"received_chunk_count":0,"transcribed_chunk_count":0},
         "created_at":"2026-09-03T13:52:01Z","updated_at":"2026-09-03T13:52:01Z"}
    """.trimIndent()

    @Test fun create_recording_success_parses_body_and_sends_bearer() {
        server.enqueue(MockResponse().setResponseCode(201).setBody(recordingJson("recording")))
        val res = client.createRecording("tok", CreateRecordingRequestDto(clientRecordingId = "cid-1"))
        assertThat(res).isInstanceOf(ApiResult.Success::class.java)
        assertThat((res as ApiResult.Success).body.recordingId).isEqualTo("rid-1")
        val req = server.takeRequest()
        assertThat(req.method).isEqualTo("POST")
        assertThat(req.path).isEqualTo("/v1/recordings")
        assertThat(req.getHeader("Authorization")).isEqualTo("Bearer tok")
        assertThat(req.body.readUtf8()).contains("\"client_recording_id\":\"cid-1\"")
    }

    @Test fun create_recording_idempotent_replay_200() {
        server.enqueue(MockResponse().setResponseCode(200).setBody(recordingJson("recording")))
        val res = client.createRecording("tok", CreateRecordingRequestDto(clientRecordingId = "cid-1"))
        assertThat((res as ApiResult.Success).code).isEqualTo(200)
    }

    @Test fun unauthorized_maps_to_auth_needed() {
        server.enqueue(MockResponse().setResponseCode(401).setBody("""{"title":"Unauthorized","status":401}"""))
        val res = client.createRecording("tok", CreateRecordingRequestDto(clientRecordingId = "cid-1"))
        assertThat(res.outcome).isEqualTo(Outcome.AUTH_NEEDED)
        assertThat((res as ApiResult.Failure).problem?.title).isEqualTo("Unauthorized")
    }

    @Test fun cold_start_503_is_retryable() {
        server.enqueue(MockResponse().setResponseCode(503).setBody("""{"status":"not_ready"}"""))
        val res = client.createRecording("tok", CreateRecordingRequestDto(clientRecordingId = "cid-1"))
        assertThat(res.outcome).isEqualTo(Outcome.RETRYABLE)
    }

    @Test fun upload_chunk_sends_raw_wav_with_digest_header() {
        server.enqueue(
            MockResponse().setResponseCode(202).setBody(
                """{"recording_id":"rid-1","index":0,"chunk_state":"accepted",
                    "checksum":"sha-256=:abc:","received_at":"2026-09-03T13:52:05Z"}""".trimIndent(),
            ),
        )
        val wav = ByteArray(64) { it.toByte() }
        val res = client.uploadChunk("tok", "rid-1", 0, wav, "sha-256=:abc:", 30000, 1500, "2026-09-03T13:52:00Z")
        assertThat(res).isInstanceOf(ApiResult.Success::class.java)
        val req = server.takeRequest()
        assertThat(req.method).isEqualTo("PUT")
        assertThat(req.path).isEqualTo("/v1/recordings/rid-1/chunks/0")
        assertThat(req.getHeader("Content-Digest")).isEqualTo("sha-256=:abc:")
        assertThat(req.getHeader("X-Chunk-Duration-Ms")).isEqualTo("30000")
        assertThat(req.getHeader("X-Chunk-Overlap-Ms")).isEqualTo("1500")
        assertThat(req.getHeader("Content-Type")).contains("audio/wav")
        assertThat(req.body.readByteArray()).isEqualTo(wav)
    }

    @Test fun upload_chunk_conflict_is_terminal() {
        server.enqueue(MockResponse().setResponseCode(409).setBody("""{"title":"Chunk conflict","status":409}"""))
        val res = client.uploadChunk("tok", "rid-1", 0, ByteArray(4), "sha-256=:x:", null, null, null)
        assertThat(res.outcome).isEqualTo(Outcome.TERMINAL_CONFLICT)
    }

    @Test fun complete_recording_success() {
        server.enqueue(MockResponse().setResponseCode(202).setBody(recordingJson("uploading")))
        val res = client.completeRecording("tok", "rid-1", CompleteRecordingRequestDto(chunkCount = 3))
        assertThat(res).isInstanceOf(ApiResult.Success::class.java)
        val req = server.takeRequest()
        assertThat(req.path).isEqualTo("/v1/recordings/rid-1/complete")
        assertThat(req.body.readUtf8()).contains("\"chunk_count\":3")
    }

    @Test fun health_ready_true_then_false() {
        server.enqueue(MockResponse().setResponseCode(200).setBody("""{"status":"ok"}"""))
        assertThat(client.healthReady()).isTrue()
        server.enqueue(MockResponse().setResponseCode(503).setBody("""{"status":"not_ready"}"""))
        assertThat(client.healthReady()).isFalse()
    }

    @Test fun network_error_when_server_down_is_retryable() {
        server.shutdown()
        val res = client.createRecording("tok", CreateRecordingRequestDto(clientRecordingId = "cid-1"))
        assertThat(res).isInstanceOf(ApiResult.NetworkError::class.java)
        assertThat(res.outcome).isEqualTo(Outcome.RETRYABLE)
    }
}

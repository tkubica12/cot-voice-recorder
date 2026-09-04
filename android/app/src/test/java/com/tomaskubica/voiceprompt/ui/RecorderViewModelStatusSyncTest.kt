package com.tomaskubica.voiceprompt.ui

import android.app.Application
import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.data.ChunkFileStore
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import com.tomaskubica.voiceprompt.data.db.AppDatabase
import com.tomaskubica.voiceprompt.data.model.LocalRecordingState
import com.tomaskubica.voiceprompt.data.model.ServerRecordingState
import com.tomaskubica.voiceprompt.service.RecorderState
import com.tomaskubica.voiceprompt.testutil.FakeAppDependencies
import com.tomaskubica.voiceprompt.testutil.FakeAuthTokenProvider
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import kotlinx.coroutines.withTimeout
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File

@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class RecorderViewModelStatusSyncTest {

    private lateinit var app: Application
    private lateinit var db: AppDatabase
    private lateinit var repository: RecordingRepository
    private lateinit var server: MockWebServer

    private val clientId = "cid-status-sync"

    @Before
    fun setUp() = runBlocking {
        Dispatchers.setMain(Dispatchers.Unconfined)
        app = ApplicationProvider.getApplicationContext()
        db = Room.inMemoryDatabaseBuilder(app, AppDatabase::class.java)
            .allowMainThreadQueries()
            .build()
        val dir = File(app.cacheDir, "vm-status-sync-test")
        dir.deleteRecursively()
        dir.mkdirs()
        repository = RecordingRepository(db.recordingDao(), db.chunkDao(), ChunkFileStore(dir))
        server = MockWebServer().apply { start() }

        repository.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        repository.setServerRecordingId(clientId, "rid-status-sync", "transcribing")
        repository.markStopped(clientId, chunkCount = 1, stoppedAtEpochMs = 2_000)
        RecorderState.onStart(clientId)
        RecorderState.onIdle()
    }

    @After
    fun tearDown() {
        db.close()
        server.shutdown()
        Dispatchers.resetMain()
    }

    @Test
    fun polling_serially_advances_transcribing_to_completed_and_stops() = runBlocking {
        server.enqueue(MockResponse().setResponseCode(200).setBody(recordingJson("transcribing")))
        server.enqueue(
            MockResponse().setResponseCode(200).setBody(
                recordingJson("completed", transcriptId = "tid-status-sync"),
            ),
        )
        val viewModel = RecorderViewModel(
            app,
            FakeAppDependencies(
                context = app,
                repository = repository,
                api = VoiceApiClient({ server.url("/").toString() }),
                authTokens = FakeAuthTokenProvider(),
            ),
            statusPollIntervalMs = 10,
        )

        val completed = withTimeout(5_000) {
            viewModel.uiState.first {
                it.activeRecording?.serverState == ServerRecordingState.COMPLETED.name
            }.activeRecording!!
        }

        assertThat(completed.localState).isEqualTo(LocalRecordingState.COMPLETED.name)
        assertThat(completed.transcriptId).isEqualTo("tid-status-sync")
        assertThat(server.requestCount).isEqualTo(2)
        assertThat(viewModel.uiState.value.elapsedMs).isEqualTo(1_000)
    }

    private fun recordingJson(state: String, transcriptId: String? = null): String {
        val transcript = transcriptId?.let { ",\"transcript_id\":\"$it\"" } ?: ""
        return """
            {"recording_id":"rid-status-sync","client_recording_id":"$clientId","state":"$state",
             "refine_model":"gpt-5.6-luna","language":"cs",
             "progress":{"expected_chunk_count":1,"received_chunk_count":1,"transcribed_chunk_count":1}
             $transcript,"created_at":"2026-09-04T05:32:42Z","updated_at":"2026-09-04T05:34:12Z"}
        """.trimIndent()
    }
}

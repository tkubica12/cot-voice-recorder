package com.tomaskubica.voiceprompt.ui

import android.app.Application
import android.content.ClipboardManager
import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.auth.AuthTokenProvider
import com.tomaskubica.voiceprompt.auth.RefreshingTokenProvider
import com.tomaskubica.voiceprompt.auth.SilentRefreshResult
import com.tomaskubica.voiceprompt.auth.StoredToken
import com.tomaskubica.voiceprompt.auth.TokenResult
import com.tomaskubica.voiceprompt.data.ChunkFileStore
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import com.tomaskubica.voiceprompt.data.db.AppDatabase
import com.tomaskubica.voiceprompt.data.model.ServerRecordingState
import com.tomaskubica.voiceprompt.service.RecorderState
import com.tomaskubica.voiceprompt.testutil.FakeAppDependencies
import com.tomaskubica.voiceprompt.testutil.FakeAuthTokenProvider
import com.tomaskubica.voiceprompt.testutil.FakeSilentRefresher
import com.tomaskubica.voiceprompt.testutil.FakeTokenProvider
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File
import java.util.concurrent.TimeUnit

/**
 * The foreground API calls must go through the refreshing token provider, exactly like the upload
 * workers do. Reading the raw token store would replay a cached-but-expired ID token (an instant
 * 401) instead of silently refreshing it.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class RecorderViewModelAuthTest {

    private lateinit var app: Application
    private lateinit var db: AppDatabase
    private lateinit var repo: RecordingRepository
    private lateinit var server: MockWebServer

    private val clientId = "cid-vm-auth"
    private val staleToken = StoredToken("stale-id-token", "owner@example.com", 0L)
    private val freshToken = StoredToken("fresh-id-token", "owner@example.com", Long.MAX_VALUE)

    @Before fun setUp() {
        Dispatchers.setMain(Dispatchers.Unconfined)
        app = ApplicationProvider.getApplicationContext()
        db = Room.inMemoryDatabaseBuilder(app, AppDatabase::class.java)
            .allowMainThreadQueries().build()
        val dir = File(app.cacheDir, "vm-auth-test")
        dir.deleteRecursively()
        dir.mkdirs()
        repo = RecordingRepository(db.recordingDao(), db.chunkDao(), ChunkFileStore(dir))
        server = MockWebServer().apply { start() }
        RecorderState.onIdle()
    }

    @After fun tearDown() {
        db.close()
        server.shutdown()
        Dispatchers.resetMain()
    }

    private fun viewModelWith(tokens: AuthTokenProvider) = RecorderViewModel(
        app,
        FakeAppDependencies(
            context = app,
            repository = repo,
            api = VoiceApiClient({ server.url("/").toString() }),
            authTokens = tokens,
        ),
        statusPollingEnabled = false,
    )

    /** A provider holding an expired token that can still be refreshed without any UI. */
    private fun refreshingProvider(refresher: FakeSilentRefresher) =
        RefreshingTokenProvider(FakeTokenProvider(staleToken) { 0L }, refresher)

    private fun authHeader(): String? =
        server.takeRequest(2, TimeUnit.SECONDS)?.getHeader("Authorization")

    private val transcriptPageJson = """
        {"items":[{"transcript_id":"tid-1","recording_id":"rid-1","preview":"hello",
         "completed_at":"2026-09-03T13:52:01Z","expires_at":"2026-09-10T13:52:01Z",
         "language":"cs","refine_model":"gpt-5.6-luna"}]}
    """.trimIndent()

    private val transcriptJson = """
        {"transcript_id":"tid-1","recording_id":"rid-1","body":"full text","preview":"full",
         "language":"cs","refine_model":"gpt-5.6-luna","completed_at":"2026-09-03T13:52:01Z",
         "expires_at":"2026-09-10T13:52:01Z","character_count":9}
    """.trimIndent()

    private val recordingJson = """
        {"recording_id":"rid-1","client_recording_id":"$clientId","state":"transcribing",
         "refine_model":"gpt-5.6-luna","language":"cs",
         "progress":{"received_chunk_count":1,"transcribed_chunk_count":0},
         "created_at":"2026-09-03T13:52:01Z","updated_at":"2026-09-03T13:52:01Z"}
    """.trimIndent()

    private suspend fun seedRecording() {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        repo.setServerRecordingId(clientId, "rid-1", "recording")
    }

    // ------------------------------------------- expired cached token + silent refresh
    @Test fun history_silently_refreshes_an_expired_token_and_never_sends_the_stale_one() = runBlocking {
        val refresher = FakeSilentRefresher().willReturn(SilentRefreshResult.Success(freshToken))
        val vm = viewModelWith(refreshingProvider(refresher))
        server.enqueue(MockResponse().setResponseCode(200).setBody(transcriptPageJson))

        vm.loadHistory().join()

        assertThat(refresher.attempts).isEqualTo(1)
        assertThat(authHeader()).isEqualTo("Bearer ${freshToken.idToken}")
        assertThat(vm.history.value.items).hasSize(1)
        assertThat(vm.history.value.error).isNull()
        assertThat(vm.history.value.loading).isFalse()
    }

    @Test fun copy_transcript_silently_refreshes_an_expired_token() = runBlocking {
        val refresher = FakeSilentRefresher().willReturn(SilentRefreshResult.Success(freshToken))
        val vm = viewModelWith(refreshingProvider(refresher))
        server.enqueue(MockResponse().setResponseCode(200).setBody(transcriptJson))
        var result: Boolean? = null

        vm.copyTranscript(app, "tid-1") { result = it }.join()

        assertThat(refresher.attempts).isEqualTo(1)
        assertThat(authHeader()).isEqualTo("Bearer ${freshToken.idToken}")
        assertThat(result).isTrue()
        val clip = app.getSystemService(ClipboardManager::class.java).primaryClip
        assertThat(clip?.getItemAt(0)?.text.toString()).isEqualTo("full text")
    }

    @Test fun status_refresh_silently_refreshes_an_expired_token() = runBlocking {
        seedRecording()
        val refresher = FakeSilentRefresher().willReturn(SilentRefreshResult.Success(freshToken))
        val vm = viewModelWith(refreshingProvider(refresher))
        server.enqueue(MockResponse().setResponseCode(200).setBody(recordingJson))

        vm.refreshRecordingStatus(clientId).join()

        assertThat(refresher.attempts).isEqualTo(1)
        assertThat(authHeader()).isEqualTo("Bearer ${freshToken.idToken}")
        assertThat(repo.getRecording(clientId)!!.serverState)
            .isEqualTo(ServerRecordingState.TRANSCRIBING.name)
    }

    // ---------------------------------------------------------------- auth needed
    @Test fun history_reports_auth_needed_without_calling_the_backend() = runBlocking {
        val vm = viewModelWith(FakeAuthTokenProvider(TokenResult.AuthNeeded))

        vm.loadHistory().join()

        assertThat(vm.history.value.error).isEqualTo("auth")
        assertThat(vm.history.value.loading).isFalse()
        assertThat(server.requestCount).isEqualTo(0)
    }

    @Test fun copy_transcript_fails_cleanly_when_sign_in_is_required() = runBlocking {
        val vm = viewModelWith(FakeAuthTokenProvider(TokenResult.AuthNeeded))
        var result: Boolean? = null

        vm.copyTranscript(app, "tid-1") { result = it }.join()

        assertThat(result).isFalse()
        assertThat(server.requestCount).isEqualTo(0)
    }

    @Test fun status_refresh_is_skipped_when_sign_in_is_required() = runBlocking {
        seedRecording()
        val before = repo.getRecording(clientId)!!.serverState
        val vm = viewModelWith(FakeAuthTokenProvider(TokenResult.AuthNeeded))

        vm.refreshRecordingStatus(clientId).join()

        assertThat(server.requestCount).isEqualTo(0)
        assertThat(repo.getRecording(clientId)!!.serverState).isEqualTo(before)
    }

    @Test fun an_unconfigured_build_never_reaches_the_backend() = runBlocking {
        val vm = viewModelWith(FakeAuthTokenProvider(TokenResult.Unavailable))

        vm.loadHistory().join()

        assertThat(vm.history.value.error).isEqualTo("auth")
        assertThat(server.requestCount).isEqualTo(0)
    }

    @Test fun a_silent_refresh_that_needs_interaction_is_reported_as_auth() = runBlocking {
        val refresher = FakeSilentRefresher().willReturn(SilentRefreshResult.InteractionRequired)
        val vm = viewModelWith(refreshingProvider(refresher))

        vm.loadHistory().join()

        assertThat(refresher.attempts).isEqualTo(1)
        assertThat(vm.history.value.error).isEqualTo("auth")
        assertThat(server.requestCount).isEqualTo(0)
    }
}

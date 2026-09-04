package com.tomaskubica.voiceprompt.ui

import android.app.Application
import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.audio.StreamingChunker
import com.tomaskubica.voiceprompt.data.ChunkFileStore
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.RecordingRetryCoordinator
import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import com.tomaskubica.voiceprompt.data.db.AppDatabase
import com.tomaskubica.voiceprompt.data.model.ChunkUploadState
import com.tomaskubica.voiceprompt.data.model.LocalRecordingState
import com.tomaskubica.voiceprompt.service.RecorderState
import com.tomaskubica.voiceprompt.testutil.FakeAppDependencies
import com.tomaskubica.voiceprompt.testutil.FakeAuthTokenProvider
import com.tomaskubica.voiceprompt.testutil.RecordingScheduler
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File

/** The retry path a user actually triggers from the failed-recording card. */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class RecorderViewModelRetryTest {

    private lateinit var db: AppDatabase
    private lateinit var repo: RecordingRepository
    private lateinit var scheduler: RecordingScheduler
    private lateinit var deps: FakeAppDependencies
    private lateinit var vm: RecorderViewModel

    private val clientId = "cid-vm-retry"

    @Before fun setUp() {
        Dispatchers.setMain(Dispatchers.Unconfined)
        val app = ApplicationProvider.getApplicationContext<Application>()
        db = Room.inMemoryDatabaseBuilder(app, AppDatabase::class.java)
            .allowMainThreadQueries().build()
        val dir = File(app.cacheDir, "vm-retry-test")
        dir.deleteRecursively()
        dir.mkdirs()
        repo = RecordingRepository(db.recordingDao(), db.chunkDao(), ChunkFileStore(dir))
        scheduler = RecordingScheduler()
        deps = FakeAppDependencies(
            context = app,
            repository = repo,
            api = VoiceApiClient({ "http://127.0.0.1:1/" }),
            authTokens = FakeAuthTokenProvider(),
            scheduler = scheduler,
        )
        RecorderState.onIdle()
        vm = RecorderViewModel(app, deps, statusPollingEnabled = false)
    }

    @After fun tearDown() {
        db.close()
        Dispatchers.resetMain()
    }

    private fun raw(index: Int) = StreamingChunker.RawChunk(
        index = index, startSample = index * 456_000L, lengthSamples = 480_000L,
        pcm = ByteArray(200) { it.toByte() }, hasPriorOverlap = index > 0,
    )

    /** A recording that died terminally after the user stopped it. */
    private suspend fun seedFailedRecording(): String {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        repo.setServerRecordingId(clientId, "rid-1", "recording")
        val c0 = repo.persistChunk(clientId, raw(0), 1_000)
        repo.persistChunk(clientId, raw(1), 1_000)
        repo.markStopped(clientId, chunkCount = 2, stoppedAtEpochMs = 2_000)
        repo.markChunkAcked(c0)
        repo.markFailed(clientId, "chunk_conflict")
        RecorderState.onStart(clientId)
        RecorderState.onIdle()
        return repo.getChunk(clientId, 1)!!.filePath!!
    }

    @Test fun retry_clears_the_sticky_failure_and_rebuilds_the_chain() = runBlocking {
        seedFailedRecording()

        vm.retryRecording(clientId).join()

        val r = repo.getRecording(clientId)!!
        assertThat(r.localState).isEqualTo(LocalRecordingState.COMPLETING.name)
        assertThat(r.failureReason).isNull()
        // Reset happens before scheduling, so the rebuilt chain can actually complete.
        assertThat(scheduler.retries.single())
            .isEqualTo(Triple(clientId, listOf(1), true))
    }

    @Test fun the_ui_shows_retrying_instead_of_a_sticky_failure() = runBlocking {
        seedFailedRecording()
        val before = withTimeout(5_000) { vm.uiState.first { it.activeRecording != null } }
        assertThat(Statuses.displayStatus(before.phase, before.activeRecording, before.pendingChunks))
            .isEqualTo(DisplayStatus.FAILED)

        vm.retryRecording(clientId).join()

        val after = withTimeout(5_000) {
            vm.uiState.first { it.activeRecording?.retryRequestedAtEpochMs != null }
        }
        assertThat(Statuses.displayStatus(after.phase, after.activeRecording, after.pendingChunks))
            .isEqualTo(DisplayStatus.RETRYING)
        assertThat(after.retryError).isNull()
    }

    @Test fun a_retry_that_cannot_be_scheduled_is_surfaced_and_keeps_the_audio() = runBlocking {
        val unsentWav = seedFailedRecording()
        scheduler.failWith = IllegalStateException("WorkManager unavailable")

        vm.retryRecording(clientId).join()

        val state = withTimeout(5_000) { vm.uiState.first { it.retryError != null } }
        assertThat(state.retryError).isEqualTo(RecordingRetryCoordinator.RETRY_SCHEDULE_FAILED)
        // Nothing was thrown away: the card stays retryable with all captured audio.
        assertThat(File(unsentWav).exists()).isTrue()
        assertThat(repo.chunksFor(clientId)).hasSize(2)
        assertThat(repo.getChunk(clientId, 0)!!.uploadState).isEqualTo(ChunkUploadState.ACKED.name)
        assertThat(repo.getRecording(clientId)!!.chunkCount).isEqualTo(2)
        assertThat(repo.getRecording(clientId)!!.recordingId).isEqualTo("rid-1")
    }

    @Test fun a_successful_retry_after_a_failed_one_clears_the_error() = runBlocking {
        seedFailedRecording()
        scheduler.failWith = IllegalStateException("boom")
        vm.retryRecording(clientId).join()
        assertThat(withTimeout(5_000) { vm.uiState.first { it.retryError != null } }.retryError)
            .isNotNull()

        scheduler.failWith = null
        vm.retryRecording(clientId).join()

        assertThat(withTimeout(5_000) { vm.uiState.first { it.retryError == null } }.retryError)
            .isNull()
        assertThat(scheduler.retries).hasSize(1)
    }

    @Test fun retrying_an_unknown_recording_does_nothing() = runBlocking {
        vm.retryRecording("nope").join()

        assertThat(scheduler.retries).isEmpty()
        assertThat(vm.uiState.value.retryError).isNull()
    }
}

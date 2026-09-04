package com.tomaskubica.voiceprompt.service

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.audio.StreamingChunker
import com.tomaskubica.voiceprompt.data.ChunkFileStore
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.db.AppDatabase
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

/**
 * Data-loss regression: a mid-recording exception must never destroy audio that was already
 * persisted. Previously any failure was collapsed to "0 chunks emitted", which deleted the
 * whole recording directory and the DB rows with it.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class CaptureFinalizerTest {

    private lateinit var db: AppDatabase
    private lateinit var repo: RecordingRepository
    private lateinit var fileStore: ChunkFileStore
    private lateinit var scheduler: RecordingScheduler
    private lateinit var finalizer: CaptureFinalizer

    private val clientId = "cid-capture"

    @Before fun setUp() {
        val ctx = ApplicationProvider.getApplicationContext<android.content.Context>()
        db = Room.inMemoryDatabaseBuilder(ctx, AppDatabase::class.java)
            .allowMainThreadQueries().build()
        fileStore = ChunkFileStore(File(ctx.cacheDir, "capture-test-${System.nanoTime()}"))
        repo = RecordingRepository(db.recordingDao(), db.chunkDao(), fileStore)
        scheduler = RecordingScheduler()
        finalizer = CaptureFinalizer(repo, scheduler) { 5_000L }
    }

    @After fun tearDown() = db.close()

    private fun raw(index: Int) = StreamingChunker.RawChunk(
        index = index,
        startSample = index * 456_000L,
        lengthSamples = 480_000L,
        pcm = ByteArray(200) { it.toByte() },
        hasPriorOverlap = index > 0,
    )

    private suspend fun seed(chunks: Int): List<String> {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)
        return (0 until chunks).map { repo.persistChunk(clientId, raw(it), 1_000).filePath!! }
    }

    @Test fun failure_after_partial_capture_keeps_files_rows_and_schedules_upload() = runBlocking<Unit> {
        val paths = seed(chunks = 3)
        assertThat(paths.map { File(it).exists() }).containsExactly(true, true, true)

        val result = finalizer.finish(
            clientId,
            outcome = Result.failure(IllegalStateException("AudioRecord died")),
            cancelled = false,
        )

        // 1. Nothing on disk or in the database was destroyed.
        assertThat(paths.map { File(it).exists() }).containsExactly(true, true, true)
        assertThat(repo.chunksFor(clientId)).hasSize(3)
        assertThat(repo.getRecording(clientId)).isNotNull()

        // 2. Upload work was scheduled for every durable chunk plus completion.
        assertThat(result).isEqualTo(CaptureResult.Uploading(3, captureFailed = true))
        assertThat(scheduler.chunks).containsExactly(clientId to 0, clientId to 1, clientId to 2)
        assertThat(scheduler.completes).containsExactly(clientId)

        // 3. Completion declares the real chunk count, as the backend contract requires.
        val recording = repo.getRecording(clientId)!!
        assertThat(recording.chunkCount).isEqualTo(3)
        assertThat(recording.localState).isEqualTo(LocalRecordingState.COMPLETING.name)
        assertThat(recording.failureReason).isEqualTo("capture_error")
    }

    @Test fun clean_stop_schedules_completion_without_a_capture_error() = runBlocking<Unit> {
        seed(chunks = 2)

        val result = finalizer.finish(clientId, outcome = Result.success(2), cancelled = false)

        assertThat(result).isEqualTo(CaptureResult.Uploading(2, captureFailed = false))
        assertThat(repo.getRecording(clientId)!!.failureReason).isNull()
        assertThat(scheduler.completes).containsExactly(clientId)
    }

    @Test fun explicit_cancel_discards_everything() = runBlocking<Unit> {
        val paths = seed(chunks = 2)

        val result = finalizer.finish(clientId, outcome = Result.success(2), cancelled = true)

        assertThat(result).isEqualTo(CaptureResult.Discarded("cancelled"))
        assertThat(paths.map { File(it).exists() }).containsExactly(false, false)
        assertThat(repo.getRecording(clientId)).isNull()
        assertThat(repo.chunksFor(clientId)).isEmpty()
        assertThat(scheduler.completes).isEmpty()
    }

    @Test fun failure_before_any_chunk_discards_and_does_not_schedule() = runBlocking<Unit> {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)

        val result = finalizer.finish(
            clientId,
            outcome = Result.failure(RuntimeException("mic unavailable")),
            cancelled = false,
        )

        assertThat(result).isEqualTo(CaptureResult.Discarded("capture_failed"))
        assertThat(repo.getRecording(clientId)).isNull()
        assertThat(scheduler.chunks).isEmpty()
        assertThat(scheduler.completes).isEmpty()
    }

    @Test fun silent_recording_with_no_chunks_is_discarded_as_empty() = runBlocking<Unit> {
        repo.createLocalRecording(clientId, "gpt-5.6-luna", "cs", 1_000)

        val result = finalizer.finish(clientId, outcome = Result.success(0), cancelled = false)

        assertThat(result).isEqualTo(CaptureResult.Discarded("empty"))
        assertThat(repo.getRecording(clientId)).isNull()
    }

    @Test fun already_acked_chunks_are_not_re_enqueued() = runBlocking<Unit> {
        seed(chunks = 3)
        repo.markChunkAcked(repo.getChunk(clientId, 0)!!)

        finalizer.finish(clientId, outcome = Result.failure(RuntimeException("boom")), cancelled = false)

        assertThat(scheduler.chunks).containsExactly(clientId to 1, clientId to 2)
        // The acked chunk still counts toward the declared total.
        assertThat(repo.getRecording(clientId)!!.chunkCount).isEqualTo(3)
    }

    @Test fun reported_count_comes_from_durable_rows_not_the_capture_counter() = runBlocking<Unit> {
        seed(chunks = 4)

        // Capture believed it emitted 9 chunks but only 4 are durable: trust the repository.
        val result = finalizer.finish(clientId, outcome = Result.success(9), cancelled = false)

        assertThat(result).isEqualTo(CaptureResult.Uploading(4, captureFailed = false))
        assertThat(repo.getRecording(clientId)!!.chunkCount).isEqualTo(4)
    }
}

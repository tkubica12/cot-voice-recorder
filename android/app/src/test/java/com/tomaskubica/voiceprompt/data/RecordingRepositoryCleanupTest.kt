package com.tomaskubica.voiceprompt.data

import androidx.room.Room
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.data.db.AppDatabase
import com.tomaskubica.voiceprompt.data.db.ChunkEntity
import com.tomaskubica.voiceprompt.data.db.RecordingEntity
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
class RecordingRepositoryCleanupTest {

    private lateinit var db: AppDatabase
    private lateinit var fileStore: ChunkFileStore
    private lateinit var repo: RecordingRepository
    private var now: Long = 1_000_000_000_000L
    private val fortyNineHours = 49L * 60 * 60 * 1000

    @Before fun setUp() {
        val ctx = ApplicationProvider.getApplicationContext<android.content.Context>()
        db = Room.inMemoryDatabaseBuilder(ctx, AppDatabase::class.java).allowMainThreadQueries().build()
        fileStore = ChunkFileStore(File(ctx.cacheDir, "cleanup-test").apply { mkdirs() })
        repo = RecordingRepository(db.recordingDao(), db.chunkDao(), fileStore) { now }
    }

    @After fun tearDown() { db.close() }

    private fun oldRecording(id: String, local: String, server: String = "UNKNOWN") = RecordingEntity(
        clientRecordingId = id,
        recordingId = "srv-$id",
        localState = local,
        serverState = server,
        refineModel = "gpt-5.6-luna",
        language = "cs",
        startedAtEpochMs = now - fortyNineHours,
        createdAtEpochMs = now - fortyNineHours,
        updatedAtEpochMs = now - fortyNineHours,
    )

    private fun chunk(id: String, index: Int, state: String, withFile: Boolean) = ChunkEntity(
        clientRecordingId = id,
        index = index,
        filePath = if (withFile) fileStore.write(id, index, ByteArray(10)).absolutePath else null,
        checksum = "sha-256=:x:",
        sizeBytes = 10,
        durationMs = 1000,
        overlapMs = 0,
        startedAtIso = null,
        uploadState = state,
        createdAtEpochMs = now - fortyNineHours,
    )

    @Test fun old_completed_recording_with_acked_chunk_is_removed() = runBlocking {
        db.recordingDao().upsert(oldRecording("done", "COMPLETED", "COMPLETED"))
        db.chunkDao().insertIfAbsent(chunk("done", 0, "ACKED", withFile = false))

        val removed = repo.cleanup()

        assertThat(removed).isAtLeast(1)
        assertThat(repo.getRecording("done")).isNull()
    }

    @Test fun old_recording_with_pending_chunk_is_preserved() = runBlocking {
        db.recordingDao().upsert(oldRecording("pending", "COMPLETING"))
        val c = chunk("pending", 0, "PENDING", withFile = true)
        db.chunkDao().insertIfAbsent(c)

        val removed = repo.cleanup()

        // Never delete unacknowledged audio based on age alone.
        assertThat(repo.getRecording("pending")).isNotNull()
        assertThat(File(c.filePath!!).exists()).isTrue()
        assertThat(removed).isEqualTo(0)
    }

    @Test fun recent_recording_is_preserved() = runBlocking {
        val recent = oldRecording("recent", "COMPLETED", "COMPLETED")
            .copy(createdAtEpochMs = now - 60_000) // 1 minute old
        db.recordingDao().upsert(recent)

        repo.cleanup()

        assertThat(repo.getRecording("recent")).isNotNull()
    }

    @Test fun old_still_recording_state_is_preserved() = runBlocking {
        db.recordingDao().upsert(oldRecording("live", "RECORDING"))
        repo.cleanup()
        assertThat(repo.getRecording("live")).isNotNull()
    }
}

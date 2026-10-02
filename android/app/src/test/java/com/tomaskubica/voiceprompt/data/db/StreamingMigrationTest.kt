package com.tomaskubica.voiceprompt.data.db

import android.content.Context
import androidx.room.Room
import androidx.sqlite.db.SupportSQLiteDatabase
import androidx.sqlite.db.SupportSQLiteOpenHelper
import androidx.sqlite.db.framework.FrameworkSQLiteOpenHelperFactory
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import kotlinx.coroutines.runBlocking
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class StreamingMigrationTest {
    @Test fun migration_preserves_pending_audio_rows_and_their_legacy_contract() = runBlocking {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val name = "streaming-migration.db"
        context.deleteDatabase(name)
        val helper = FrameworkSQLiteOpenHelperFactory().create(
            SupportSQLiteOpenHelper.Configuration.builder(context).name(name)
                .callback(object : SupportSQLiteOpenHelper.Callback(2) {
                    override fun onCreate(db: SupportSQLiteDatabase) {
                        db.execSQL("""CREATE TABLE recordings (
                            clientRecordingId TEXT NOT NULL PRIMARY KEY, recordingId TEXT,
                            localState TEXT NOT NULL, serverState TEXT NOT NULL, refineModel TEXT NOT NULL,
                            language TEXT NOT NULL, chunkCount INTEGER, transcriptId TEXT, failureReason TEXT,
                            retryRequestedAtEpochMs INTEGER, startedAtEpochMs INTEGER NOT NULL,
                            stoppedAtEpochMs INTEGER, createdAtEpochMs INTEGER NOT NULL, updatedAtEpochMs INTEGER NOT NULL)""")
                        db.execSQL("""CREATE TABLE chunks (
                            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, clientRecordingId TEXT NOT NULL,
                            `index` INTEGER NOT NULL, filePath TEXT, checksum TEXT NOT NULL,
                            sizeBytes INTEGER NOT NULL, durationMs INTEGER NOT NULL, overlapMs INTEGER NOT NULL,
                            startedAtIso TEXT, uploadState TEXT NOT NULL, createdAtEpochMs INTEGER NOT NULL)""")
                        db.execSQL("CREATE UNIQUE INDEX index_chunks_clientRecordingId_index ON chunks (clientRecordingId, `index`)")
                        db.execSQL("""INSERT INTO recordings (clientRecordingId,localState,serverState,refineModel,language,
                            startedAtEpochMs,createdAtEpochMs,updatedAtEpochMs)
                            VALUES ('pending','STOPPED','UNKNOWN','gpt-6-luna','cs',1,1,1)""")
                        db.execSQL("""INSERT INTO chunks (clientRecordingId,`index`,filePath,checksum,sizeBytes,durationMs,
                            overlapMs,uploadState,createdAtEpochMs)
                            VALUES ('pending',0,'saved-audio.wav','digest',100,30000,0,'PENDING',1)""")
                    }
                    override fun onUpgrade(db: SupportSQLiteDatabase, oldVersion: Int, newVersion: Int) = Unit
                }).build(),
        )
        helper.writableDatabase
        helper.close()
        val database = Room.databaseBuilder(context, AppDatabase::class.java, name)
            .addMigrations(AppDatabase.MIGRATION_2_3).allowMainThreadQueries().build()
        try {
            val record = database.recordingDao().get("pending")!!
            assertThat(record.transcriptionMode).isEqualTo("chunked")
            assertThat(record.audioLayout).isEqualTo("legacy_overlap")
            assertThat(record.refinementEnabled).isTrue()
            assertThat(database.chunkDao().forRecording("pending").single().filePath).isEqualTo("saved-audio.wav")
        } finally {
            database.close()
            context.deleteDatabase(name)
        }
    }
}

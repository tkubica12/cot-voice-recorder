package com.tomaskubica.voiceprompt.data.db

import android.content.Context
import androidx.room.Database
import androidx.room.Room
import androidx.room.RoomDatabase
import androidx.room.migration.Migration
import androidx.sqlite.db.SupportSQLiteDatabase

@Database(
    entities = [RecordingEntity::class, ChunkEntity::class],
    version = 3,
    exportSchema = false,
)
abstract class AppDatabase : RoomDatabase() {
    abstract fun recordingDao(): RecordingDao
    abstract fun chunkDao(): ChunkDao

    companion object {
        @Volatile
        private var instance: AppDatabase? = null

        /**
         * Adds the user-retry marker column. Must be a real migration: destructive fallback would
         * delete recordings whose audio has not been uploaded yet.
         */
        val MIGRATION_1_2 = object : Migration(1, 2) {
            override fun migrate(db: SupportSQLiteDatabase) {
                db.execSQL("ALTER TABLE recordings ADD COLUMN retryRequestedAtEpochMs INTEGER")
            }

        }

        val MIGRATION_2_3 = object : Migration(2, 3) {
            override fun migrate(db: SupportSQLiteDatabase) {
                db.execSQL("ALTER TABLE recordings ADD COLUMN transcriptionMode TEXT NOT NULL DEFAULT 'chunked'")
                db.execSQL("ALTER TABLE recordings ADD COLUMN audioLayout TEXT NOT NULL DEFAULT 'legacy_overlap'")
                db.execSQL("ALTER TABLE recordings ADD COLUMN refinementEnabled INTEGER NOT NULL DEFAULT 1")
                db.execSQL("ALTER TABLE recordings ADD COLUMN streamPreview TEXT NOT NULL DEFAULT ''")
                db.execSQL("ALTER TABLE recordings ADD COLUMN streamedAudioMs INTEGER NOT NULL DEFAULT 0")
                db.execSQL("ALTER TABLE recordings ADD COLUMN streamAttempt INTEGER NOT NULL DEFAULT 0")
                db.execSQL("ALTER TABLE recordings ADD COLUMN streamError TEXT")
            }
        }

        fun get(context: Context): AppDatabase =
            instance ?: synchronized(this) {
                instance ?: Room.databaseBuilder(
                    context.applicationContext,
                    AppDatabase::class.java,
                    "voiceprompt.db",
                ).addMigrations(MIGRATION_1_2, MIGRATION_2_3).build().also { instance = it }
            }
    }
}

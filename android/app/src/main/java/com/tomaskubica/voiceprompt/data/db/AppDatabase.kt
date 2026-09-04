package com.tomaskubica.voiceprompt.data.db

import android.content.Context
import androidx.room.Database
import androidx.room.Room
import androidx.room.RoomDatabase
import androidx.room.migration.Migration
import androidx.sqlite.db.SupportSQLiteDatabase

@Database(
    entities = [RecordingEntity::class, ChunkEntity::class],
    version = 2,
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

        fun get(context: Context): AppDatabase =
            instance ?: synchronized(this) {
                instance ?: Room.databaseBuilder(
                    context.applicationContext,
                    AppDatabase::class.java,
                    "voiceprompt.db",
                ).addMigrations(MIGRATION_1_2).build().also { instance = it }
            }
    }
}

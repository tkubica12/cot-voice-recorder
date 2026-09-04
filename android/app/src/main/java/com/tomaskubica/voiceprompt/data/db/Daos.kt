package com.tomaskubica.voiceprompt.data.db

import androidx.room.Dao
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.Query
import androidx.room.Update
import kotlinx.coroutines.flow.Flow

@Dao
interface RecordingDao {
    @Insert(onConflict = OnConflictStrategy.REPLACE)
    suspend fun upsert(recording: RecordingEntity)

    @Update
    suspend fun update(recording: RecordingEntity)

    @Query("SELECT * FROM recordings WHERE clientRecordingId = :clientId")
    suspend fun get(clientId: String): RecordingEntity?

    @Query("SELECT * FROM recordings WHERE clientRecordingId = :clientId")
    fun observe(clientId: String): Flow<RecordingEntity?>

    @Query("SELECT * FROM recordings ORDER BY createdAtEpochMs DESC LIMIT :limit")
    fun observeRecent(limit: Int): Flow<List<RecordingEntity>>

    @Query(
        """
        SELECT * FROM recordings
        ORDER BY createdAtEpochMs DESC
        LIMIT 1
        """,
    )
    fun observeLatest(): Flow<RecordingEntity?>

    @Query("SELECT * FROM recordings ORDER BY createdAtEpochMs DESC LIMIT :limit")
    suspend fun recent(limit: Int): List<RecordingEntity>

    @Query("SELECT recordingId FROM recordings WHERE clientRecordingId = :clientId")
    suspend fun recordingIdFor(clientId: String): String?

    /**
     * Atomically move a failed (or stalled) recording back onto its upload path so a user-initiated
     * retry can make progress: a single UPDATE statement clears the sticky FAILED state and the
     * stale failure reason, and stamps the user-retry marker.
     *
     * Chunk rows, WAV files, acked status, the server recording id and chunkCount are untouched.
     * Recordings that are still capturing (RECORDING) or already COMPLETED are left alone.
     */
    @Query(
        """
        UPDATE recordings
        SET localState = CASE
                WHEN chunkCount IS NOT NULL OR stoppedAtEpochMs IS NOT NULL THEN 'COMPLETING'
                ELSE 'STOPPED' END,
            failureReason = NULL,
            retryRequestedAtEpochMs = :now,
            updatedAtEpochMs = :now
        WHERE clientRecordingId = :clientId
          AND localState NOT IN ('COMPLETED', 'RECORDING')
        """,
    )
    suspend fun resetForRetry(clientId: String, now: Long): Int

    @Query("DELETE FROM recordings WHERE clientRecordingId = :clientId")
    suspend fun delete(clientId: String)
}

@Dao
interface ChunkDao {
    @Insert(onConflict = OnConflictStrategy.IGNORE)
    suspend fun insertIfAbsent(chunk: ChunkEntity): Long

    @Update
    suspend fun update(chunk: ChunkEntity)

    @Query("SELECT * FROM chunks WHERE clientRecordingId = :clientId AND `index` = :index LIMIT 1")
    suspend fun get(clientId: String, index: Int): ChunkEntity?

    @Query("SELECT * FROM chunks WHERE clientRecordingId = :clientId ORDER BY `index` ASC")
    suspend fun forRecording(clientId: String): List<ChunkEntity>

    @Query("SELECT COUNT(*) FROM chunks WHERE clientRecordingId = :clientId")
    fun observeCount(clientId: String): Flow<Int>

    @Query("SELECT COUNT(*) FROM chunks WHERE clientRecordingId = :clientId AND uploadState != 'ACKED'")
    fun observePending(clientId: String): Flow<Int>

    @Query("SELECT COUNT(*) FROM chunks WHERE uploadState != 'ACKED'")
    fun observeAllPending(): Flow<Int>

    @Query("SELECT COUNT(*) FROM chunks WHERE clientRecordingId = :clientId AND uploadState = 'ACKED'")
    suspend fun ackedCount(clientId: String): Int

    @Query("SELECT COUNT(*) FROM chunks WHERE clientRecordingId = :clientId AND uploadState != 'ACKED'")
    suspend fun pendingCount(clientId: String): Int

    @Query("DELETE FROM chunks WHERE clientRecordingId = :clientId")
    suspend fun deleteForRecording(clientId: String)
}

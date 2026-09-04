package com.tomaskubica.voiceprompt.work

import android.content.Context
import androidx.work.CoroutineWorker
import androidx.work.WorkerParameters
import com.tomaskubica.voiceprompt.VoicePromptApp

/**
 * Periodic safety-net cleanup of local metadata/WAVs older than the 48 h retention window that are
 * not pending upload. Never removes unacknowledged audio based on age alone.
 */
class CleanupWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        val repo = (applicationContext as VoicePromptApp).container.repository
        return try {
            repo.cleanup()
            Result.success()
        } catch (_: Exception) {
            Result.retry()
        }
    }
}

package com.tomaskubica.voiceprompt

import android.app.Application
import androidx.work.Configuration
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import com.tomaskubica.voiceprompt.work.CleanupWorker
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import java.time.Duration

/**
 * Application entry point. Startup is instant and never waits on the backend: the warmup probe is
 * fired asynchronously and cleanup is scheduled as periodic background work.
 */
class VoicePromptApp : Application(), Configuration.Provider {

    lateinit var container: AppContainer
        private set

    private val appScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    override val workManagerConfiguration: Configuration
        get() = Configuration.Builder().build()

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)

        // Fire-and-forget backend warmup; recording is enabled regardless of the result.
        appScope.launch { container.warmup.probe() }

        scheduleCleanup()
    }

    private fun scheduleCleanup() {
        val request = PeriodicWorkRequestBuilder<CleanupWorker>(Duration.ofHours(6)).build()
        WorkManager.getInstance(this).enqueueUniquePeriodicWork(
            "cleanup",
            ExistingPeriodicWorkPolicy.KEEP,
            request,
        )
    }
}

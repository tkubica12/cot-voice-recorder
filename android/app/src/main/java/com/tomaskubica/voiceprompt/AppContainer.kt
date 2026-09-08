package com.tomaskubica.voiceprompt

import android.content.Context
import com.tomaskubica.voiceprompt.auth.AuthManager
import com.tomaskubica.voiceprompt.auth.AuthTokenProvider
import com.tomaskubica.voiceprompt.auth.EncryptedTokenStore
import com.tomaskubica.voiceprompt.auth.InteractionRequiredTokenRefresher
import com.tomaskubica.voiceprompt.auth.RefreshingTokenProvider
import com.tomaskubica.voiceprompt.auth.TokenProvider
import com.tomaskubica.voiceprompt.data.ChunkFileStore
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.RecordingRetryCoordinator
import com.tomaskubica.voiceprompt.data.SettingsStore
import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import com.tomaskubica.voiceprompt.data.db.AppDatabase
import com.tomaskubica.voiceprompt.work.AndroidAuthNotifier
import com.tomaskubica.voiceprompt.work.AuthNotifier
import com.tomaskubica.voiceprompt.work.UploadScheduler
import com.tomaskubica.voiceprompt.work.UploadWorkScheduler
import com.tomaskubica.voiceprompt.warmup.WarmupManager

/**
 * Minimal service locator wiring the singletons. Kept deliberately simple (no DI framework) but
 * built from abstractions so tests can substitute fakes.
 */
class AppContainer(context: Context) : AppDependencies {
    private val app = context.applicationContext

    override val settings: SettingsStore by lazy { SettingsStore(app) }

    val database: AppDatabase by lazy { AppDatabase.get(app) }

    val fileStore: ChunkFileStore by lazy { ChunkFileStore(app.filesDir) }

    override val repository: RecordingRepository by lazy {
        RecordingRepository(database.recordingDao(), database.chunkDao(), fileStore)
    }

    override val api: VoiceApiClient by lazy { VoiceApiClient({ settings.backendUrl }) }

    val tokenStore: TokenProvider by lazy { EncryptedTokenStore.create(app) }

    /**
     * Token source for background workers: cached-valid first, then an explicit auth-needed result.
     * Credential Manager is intentionally Activity-only because its provider may open a chooser
     * even when auto-select is requested.
     */
    override val authTokens: AuthTokenProvider by lazy {
        RefreshingTokenProvider(
            store = tokenStore,
            refresher = InteractionRequiredTokenRefresher,
            isConfigured = { authManager.isConfigured },
        )
    }

    override val authNotifier: AuthNotifier by lazy { AndroidAuthNotifier(app) }

    override val authManager: AuthManager by lazy {
        AuthManager(app, tokenStore)
    }

    val scheduler: UploadWorkScheduler by lazy { UploadScheduler(app) }

    override val retryCoordinator: RecordingRetryCoordinator by lazy {
        RecordingRetryCoordinator(repository, scheduler)
    }

    override val warmup: WarmupManager by lazy { WarmupManager(api) }
}

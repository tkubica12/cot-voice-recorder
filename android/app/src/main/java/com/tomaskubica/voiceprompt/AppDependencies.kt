package com.tomaskubica.voiceprompt

import com.tomaskubica.voiceprompt.auth.AuthController
import com.tomaskubica.voiceprompt.auth.AuthTokenProvider
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.RecordingRetryCoordinator
import com.tomaskubica.voiceprompt.data.SettingsStore
import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import com.tomaskubica.voiceprompt.warmup.WarmupManager
import com.tomaskubica.voiceprompt.work.AuthNotifier

/**
 * The slice of [AppContainer] the UI layer depends on.
 *
 * Declared as an interface so view models can be unit tested with fakes instead of the real
 * container, which pulls in the Android Keystore, Credential Manager and WorkManager.
 */
interface AppDependencies {
    val settings: SettingsStore
    val repository: RecordingRepository
    val api: VoiceApiClient

    /** Refreshing token source — the UI must never read the raw token store. */
    val authTokens: AuthTokenProvider
    val authManager: AuthController
    val authNotifier: AuthNotifier
    val retryCoordinator: RecordingRetryCoordinator
    val warmup: WarmupManager
}

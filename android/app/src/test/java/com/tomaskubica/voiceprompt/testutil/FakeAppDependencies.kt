package com.tomaskubica.voiceprompt.testutil

import android.content.Context
import com.tomaskubica.voiceprompt.AppDependencies
import com.tomaskubica.voiceprompt.auth.AuthController
import com.tomaskubica.voiceprompt.auth.AuthState
import com.tomaskubica.voiceprompt.auth.AuthTokenProvider
import com.tomaskubica.voiceprompt.data.RecordingRepository
import com.tomaskubica.voiceprompt.data.RecordingRetryCoordinator
import com.tomaskubica.voiceprompt.data.SettingsStore
import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import com.tomaskubica.voiceprompt.warmup.WarmupManager
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import com.tomaskubica.voiceprompt.work.AuthNotifier
import com.tomaskubica.voiceprompt.work.NoOpAuthNotifier

/** [AuthController] fake: no Credential Manager, no Android Keystore. */
class FakeAuthController(
    initial: AuthState = AuthState.SignedIn("owner@example.com"),
    override val isConfigured: Boolean = true,
) : AuthController {
    private val _state = MutableStateFlow(initial)
    override val state: StateFlow<AuthState> = _state.asStateFlow()

    var silentSignIns: Int = 0
        private set
    var explicitSignIns: Int = 0
        private set
    var signOuts: Int = 0
        private set
    var refreshes: Int = 0
        private set
    var refreshedState: AuthState? = null
    var explicitResult = true
    var beforeSignIn: suspend () -> Unit = {}

    override fun refreshState() {
        refreshes++
        refreshedState?.let { _state.value = it }
    }

    override suspend fun trySilentSignIn(context: Context): Boolean {
        silentSignIns++
        return _state.value is AuthState.SignedIn
    }

    override suspend fun explicitSignIn(context: Context): Boolean {
        explicitSignIns++
        beforeSignIn()
        if (explicitResult) _state.value = AuthState.SignedIn("owner@example.com")
        return explicitResult
    }

    override suspend fun signOut() {
        signOuts++
        _state.value = AuthState.SignedOut
    }
}

/**
 * Test double for the container the view model depends on.
 *
 * The real [com.tomaskubica.voiceprompt.AppContainer] reaches for the Android Keystore,
 * Credential Manager and WorkManager, none of which are usable in a JVM unit test.
 */
class FakeAppDependencies(
    context: Context,
    override val repository: RecordingRepository,
    override val api: VoiceApiClient,
    override val authTokens: AuthTokenProvider,
    val scheduler: RecordingScheduler = RecordingScheduler(),
    override val authManager: AuthController = FakeAuthController(),
    override val authNotifier: AuthNotifier = NoOpAuthNotifier,
) : AppDependencies {
    override val settings: SettingsStore = SettingsStore(context)
    override val retryCoordinator: RecordingRetryCoordinator =
        RecordingRetryCoordinator(repository, scheduler)
    override val warmup: WarmupManager = WarmupManager(api)
}

package com.tomaskubica.voiceprompt.auth

import android.content.Context
import androidx.credentials.CredentialManager
import androidx.credentials.CustomCredential
import androidx.credentials.GetCredentialRequest
import androidx.credentials.exceptions.GetCredentialException
import androidx.credentials.exceptions.NoCredentialException
import com.google.android.libraries.identity.googleid.GetGoogleIdOption
import com.google.android.libraries.identity.googleid.GetSignInWithGoogleOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import com.tomaskubica.voiceprompt.BuildConfig
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/** Observable auth state surfaced to the UI. */
sealed interface AuthState {
    /** Web client id not configured — sign-in is safely unavailable (never a fake token). */
    data object Unavailable : AuthState
    data object SignedOut : AuthState
    data class SignedIn(val email: String?) : AuthState
    data class Error(val message: String) : AuthState
}

/**
 * The auth surface the UI depends on. Extracted as an interface so screens/view models can be
 * unit tested without Credential Manager or the Android Keystore.
 */
interface AuthController {
    val state: StateFlow<AuthState>
    val isConfigured: Boolean

    suspend fun trySilentSignIn(context: Context): Boolean

    suspend fun explicitSignIn(context: Context): Boolean

    suspend fun signOut()
}

/**
 * Google authentication via the modern Credential Manager + Sign in with Google.
 *
 * Requires a **Web/server** OAuth client id (the ID token audience the backend allowlists),
 * injected at build time as [BuildConfig.GOOGLE_WEB_CLIENT_ID]. When it is blank the manager
 * reports [AuthState.Unavailable] and refuses to mint any token — it never fabricates one.
 */
class AuthManager(
    private val appContext: Context,
    private val tokenStore: TokenProvider,
    private val webClientId: String = BuildConfig.GOOGLE_WEB_CLIENT_ID,
    private val credentialManager: CredentialManager = CredentialManager.create(appContext),
    private val credentialGetter: suspend (Context, GetCredentialRequest) ->
        androidx.credentials.GetCredentialResponse = { context, request ->
            credentialManager.getCredential(context, request)
        },
) : AuthController {
    private val _state = MutableStateFlow<AuthState>(initialState())
    override val state: StateFlow<AuthState> = _state.asStateFlow()
    private val startupRestoreGate = Mutex()
    private var startupRestoreAttempted = false

    override val isConfigured: Boolean get() = webClientId.isNotBlank()

    private fun initialState(): AuthState = when {
        webClientId.isBlank() -> AuthState.Unavailable
        else -> tokenStore.peek()?.let { AuthState.SignedIn(it.email) } ?: AuthState.SignedOut
    }

    /** Recompute state from the stored token (e.g. after expiry). */
    fun refreshState() {
        if (!isConfigured) { _state.value = AuthState.Unavailable; return }
        val token = tokenStore.currentValidToken()
        _state.value = if (token != null) AuthState.SignedIn(token.email) else AuthState.SignedOut
    }

    /**
     * Restore an already-authorized account at startup. A still-valid cached ID token is reused
     * without touching Credential Manager. Otherwise Credential Manager gets one foreground-only
     * opportunity to auto-select an authorized account; providers may still show their chooser.
     *
     * The attempt is single-flighted and limited to once per process so Activity recreation or
     * rotation cannot stack credential sheets. Background workers use a separate no-UI provider.
     */
    override suspend fun trySilentSignIn(context: Context): Boolean {
        if (!isConfigured) { _state.value = AuthState.Unavailable; return false }
        tokenStore.currentValidToken()?.let {
            _state.value = AuthState.SignedIn(it.email)
            return true
        }

        return startupRestoreGate.withLock {
            tokenStore.currentValidToken()?.let {
                _state.value = AuthState.SignedIn(it.email)
                return@withLock true
            }

            if (startupRestoreAttempted) {
                refreshState()
                return@withLock false
            }
            startupRestoreAttempted = true

            val option = GetGoogleIdOption.Builder()
                .setFilterByAuthorizedAccounts(true)
                .setServerClientId(webClientId)
                .setAutoSelectEnabled(true)
                .build()
            runCredentialRequest(context, option)
        }
    }

    /**
     * Explicit, user-initiated sign-in (shows the account chooser). Must be called with an
     * Activity context.
     */
    override suspend fun explicitSignIn(context: Context): Boolean {
        if (!isConfigured) { _state.value = AuthState.Unavailable; return false }
        val option = GetSignInWithGoogleOption.Builder(webClientId).build()
        return runCredentialRequest(context, option)
    }

    private suspend fun runCredentialRequest(
        context: Context,
        option: androidx.credentials.CredentialOption,
    ): Boolean {
        val request = GetCredentialRequest.Builder().addCredentialOption(option).build()
        return try {
            val response = credentialGetter(context, request)
            val cred = response.credential
            if (cred is CustomCredential &&
                cred.type == GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL
            ) {
                val stored = GoogleIdTokens.toStoredToken(
                    GoogleIdTokenCredential.createFrom(cred.data),
                )
                tokenStore.save(stored)
                _state.value = AuthState.SignedIn(stored.email)
                true
            } else {
                _state.value = AuthState.Error("Unexpected credential type")
                false
            }
        } catch (_: NoCredentialException) {
            // No stored/authorized account; not an error for silent path.
            if (tokenStore.peek() == null) _state.value = AuthState.SignedOut
            false
        } catch (e: GetCredentialException) {
            _state.value = AuthState.Error(e.javaClass.simpleName)
            false
        } catch (e: Exception) {
            _state.value = AuthState.Error(e.javaClass.simpleName)
            false
        }
    }

    /** Sign out: clear the stored token and Credential Manager selection state. */
    override suspend fun signOut() {
        tokenStore.clear()
        runCatching {
            credentialManager.clearCredentialState(
                androidx.credentials.ClearCredentialStateRequest(),
            )
        }
        _state.value = if (isConfigured) AuthState.SignedOut else AuthState.Unavailable
    }
}

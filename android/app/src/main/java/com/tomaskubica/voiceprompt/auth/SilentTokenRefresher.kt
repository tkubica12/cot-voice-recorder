package com.tomaskubica.voiceprompt.auth

import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential

/** Outcome of a non-interactive ID-token refresh attempt. */
sealed interface SilentRefreshResult {
    data class Success(val token: StoredToken) : SilentRefreshResult

    /** The user must complete a sign-in UI; a background worker must not start one. */
    data object InteractionRequired : SilentRefreshResult

    /** Sign-in is not configured in this build. */
    data object Unavailable : SilentRefreshResult
}

/** Non-interactive ID-token refresh. Implementations must never open UI. */
interface SilentTokenRefresher {
    suspend fun refreshSilently(): SilentRefreshResult
}

/**
 * Production fallback for background work. Android Credential Manager may open provider UI even
 * with auto-select enabled, so workers must stop and ask the user to sign in from the Activity.
 */
object InteractionRequiredTokenRefresher : SilentTokenRefresher {
    override suspend fun refreshSilently(): SilentRefreshResult =
        SilentRefreshResult.InteractionRequired
}

/** Shared mapping from a Google credential to the persisted token snapshot. */
object GoogleIdTokens {
    /** Conservative fallback when the JWT carries no readable `exp`. */
    private const val FALLBACK_LIFETIME_MS = 55L * 60 * 1000

    fun toStoredToken(
        credential: GoogleIdTokenCredential,
        now: Long = System.currentTimeMillis(),
    ): StoredToken = fromIdToken(credential.idToken, credential.id, now)

    fun fromIdToken(
        idToken: String,
        fallbackEmail: String?,
        now: Long = System.currentTimeMillis(),
    ): StoredToken = StoredToken(
        idToken = idToken,
        email = JwtUtils.email(idToken) ?: fallbackEmail,
        expiresAtEpochMs = JwtUtils.expiryEpochMs(idToken) ?: (now + FALLBACK_LIFETIME_MS),
    )
}

package com.tomaskubica.voiceprompt.auth

import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Result of asking for a usable Google ID token.
 *
 * Deliberately explicit so background upload workers can distinguish "wait and retry"
 * (transient) from "the user has to sign in again" (needs a visible prompt) instead of
 * retrying opaquely forever.
 */
sealed interface TokenResult {
    /** A token that is valid right now (expiry checked with a safety skew). */
    data class Valid(val token: StoredToken) : TokenResult

    /** No usable token can be obtained without user interaction. */
    data object AuthNeeded : TokenResult

    /** Sign-in is not configured in this build; uploading can never succeed. */
    data object Unavailable : TokenResult
}

/**
 * The seam upload workers use to obtain an ID token.
 *
 * Implementations must never hand out an expired token, and must be safe to call from a
 * WorkManager [androidx.work.CoroutineWorker] — i.e. from an application context, with no
 * Activity and no UI.
 */
interface AuthTokenProvider {
    suspend fun idToken(): TokenResult

    /**
     * Drop the cached token after the backend rejected it (401), so the next [idToken] call
     * never replays the same bad token.
     */
    suspend fun invalidate()
}

/**
 * Cached token source with an optional headless refresh seam.
 *
 * 1. Return the stored token when it is still valid (no I/O, no Credential Manager call).
 * 2. Otherwise attempt one non-interactive refresh through [SilentTokenRefresher] and persist it.
 * 3. If that cannot complete without user interaction, report [TokenResult.AuthNeeded] so
 *    the caller can retry *and* tell the user to sign in.
 *
 * The refresh is single-flighted: concurrent chunk workers share one refresh attempt.
 */
class RefreshingTokenProvider(
    private val store: TokenProvider,
    private val refresher: SilentTokenRefresher,
    private val isConfigured: () -> Boolean = { true },
    private val skewMs: Long = TokenProvider.DEFAULT_SKEW_MS,
) : AuthTokenProvider {

    private val refreshLock = Mutex()

    override suspend fun idToken(): TokenResult {
        if (!isConfigured()) return TokenResult.Unavailable

        store.currentValidToken(skewMs)?.let { return TokenResult.Valid(it) }

        return refreshLock.withLock {
            // Another coroutine may have refreshed while this one waited for the lock.
            val cached = store.currentValidToken(skewMs)
            if (cached != null) {
                TokenResult.Valid(cached)
            } else {
                when (val outcome = refresher.refreshSilently()) {
                    is SilentRefreshResult.Success -> persist(outcome.token)
                    SilentRefreshResult.InteractionRequired -> TokenResult.AuthNeeded
                    SilentRefreshResult.Unavailable -> TokenResult.Unavailable
                }
            }
        }
    }

    private fun persist(token: StoredToken): TokenResult {
        store.save(token)
        // Guard against a refreshed-but-already-stale token: never send one upstream.
        val fresh = store.currentValidToken(skewMs)
        return if (fresh != null) TokenResult.Valid(fresh) else TokenResult.AuthNeeded
    }

    override suspend fun invalidate() {
        refreshLock.withLock { store.clear() }
    }
}

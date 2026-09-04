package com.tomaskubica.voiceprompt.testutil

import com.tomaskubica.voiceprompt.auth.AuthTokenProvider
import com.tomaskubica.voiceprompt.auth.SilentRefreshResult
import com.tomaskubica.voiceprompt.auth.SilentTokenRefresher
import com.tomaskubica.voiceprompt.auth.StoredToken
import com.tomaskubica.voiceprompt.auth.TokenProvider
import com.tomaskubica.voiceprompt.auth.TokenResult

/** In-memory [TokenProvider] for tests, with a controllable clock for expiry checks. */
class FakeTokenProvider(
    private var token: StoredToken? = StoredToken("test-token", "owner@example.com", Long.MAX_VALUE),
    private val clock: () -> Long = { 0L },
) : TokenProvider {
    var saveCount: Int = 0
        private set

    override fun currentValidToken(skewMs: Long): StoredToken? =
        token?.takeIf { it.expiresAtEpochMs - skewMs > clock() }

    override fun peek(): StoredToken? = token

    override fun save(token: StoredToken) {
        saveCount++
        this.token = token
    }

    override fun clear() {
        token = null
    }

    override fun isExpired(skewMs: Long): Boolean = currentValidToken(skewMs) == null
}

/** Scripted [SilentTokenRefresher] that records how often a refresh was attempted. */
class FakeSilentRefresher(
    private val results: MutableList<SilentRefreshResult> = mutableListOf(),
    private val fallback: SilentRefreshResult = SilentRefreshResult.InteractionRequired,
) : SilentTokenRefresher {
    var attempts: Int = 0
        private set

    fun willReturn(vararg outcomes: SilentRefreshResult) = apply { results.addAll(outcomes) }

    override suspend fun refreshSilently(): SilentRefreshResult {
        attempts++
        return if (results.isEmpty()) fallback else results.removeAt(0)
    }
}

/** [AuthTokenProvider] returning scripted results; records invalidations. */
class FakeAuthTokenProvider(
    var result: TokenResult = TokenResult.Valid(
        StoredToken("test-token", "owner@example.com", Long.MAX_VALUE),
    ),
) : AuthTokenProvider {
    var invalidations: Int = 0
        private set

    var calls: Int = 0
        private set

    override suspend fun idToken(): TokenResult {
        calls++
        return result
    }

    override suspend fun invalidate() {
        invalidations++
        result = TokenResult.AuthNeeded
    }
}

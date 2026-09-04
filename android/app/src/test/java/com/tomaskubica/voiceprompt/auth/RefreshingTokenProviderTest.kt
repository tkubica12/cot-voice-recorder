package com.tomaskubica.voiceprompt.auth

import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.testutil.FakeSilentRefresher
import com.tomaskubica.voiceprompt.testutil.FakeTokenProvider
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import org.junit.Test

/**
 * Background uploads must be able to keep going across an ID-token expiry without any UI.
 * These tests pin the four outcomes that matter: cached-valid, silent-refresh success,
 * silent-refresh failure (auth needed), and "never send a stale token".
 */
class RefreshingTokenProviderTest {

    private val now = 1_000_000L

    private fun token(expiresAt: Long, value: String = "tok") =
        StoredToken(value, "owner@example.com", expiresAt)

    private fun provider(
        store: FakeTokenProvider,
        refresher: FakeSilentRefresher,
        configured: Boolean = true,
    ) = RefreshingTokenProvider(store, refresher, isConfigured = { configured })

    @Test fun uses_the_cached_token_when_it_is_still_valid() = runBlocking {
        val store = FakeTokenProvider(token(now + 10 * 60_000, "cached"), clock = { now })
        val refresher = FakeSilentRefresher()

        val result = provider(store, refresher).idToken()

        assertThat(result).isInstanceOf(TokenResult.Valid::class.java)
        assertThat((result as TokenResult.Valid).token.idToken).isEqualTo("cached")
        assertThat(refresher.attempts).isEqualTo(0) // no Credential Manager call at all
    }

    @Test fun refreshes_silently_when_the_cached_token_expired() = runBlocking {
        val store = FakeTokenProvider(token(now - 1, "expired"), clock = { now })
        val refresher = FakeSilentRefresher()
            .willReturn(SilentRefreshResult.Success(token(now + 30 * 60_000, "fresh")))

        val result = provider(store, refresher).idToken()

        assertThat(result).isInstanceOf(TokenResult.Valid::class.java)
        assertThat((result as TokenResult.Valid).token.idToken).isEqualTo("fresh")
        assertThat(refresher.attempts).isEqualTo(1)
        assertThat(store.peek()!!.idToken).isEqualTo("fresh") // persisted for the next worker
    }

    @Test fun refreshes_silently_when_no_token_is_stored() = runBlocking {
        val store = FakeTokenProvider(null, clock = { now })
        val refresher = FakeSilentRefresher()
            .willReturn(SilentRefreshResult.Success(token(now + 30 * 60_000, "fresh")))

        val result = provider(store, refresher).idToken()

        assertThat(result).isEqualTo(TokenResult.Valid(token(now + 30 * 60_000, "fresh")))
    }

    @Test fun reports_auth_needed_when_silent_refresh_requires_interaction() = runBlocking {
        val store = FakeTokenProvider(token(now - 1, "expired"), clock = { now })
        val refresher = FakeSilentRefresher()
            .willReturn(SilentRefreshResult.InteractionRequired)

        assertThat(provider(store, refresher).idToken()).isEqualTo(TokenResult.AuthNeeded)
        assertThat(refresher.attempts).isEqualTo(1)
    }

    @Test fun never_returns_a_stale_token_even_if_the_refresh_returns_one() = runBlocking {
        val store = FakeTokenProvider(token(now - 5_000, "expired"), clock = { now })
        // A refresh that hands back an already-expired token must not be passed upstream.
        val refresher = FakeSilentRefresher()
            .willReturn(SilentRefreshResult.Success(token(now - 1, "also-expired")))

        assertThat(provider(store, refresher).idToken()).isEqualTo(TokenResult.AuthNeeded)
    }

    @Test fun treats_a_token_inside_the_skew_window_as_expired() = runBlocking {
        // 30 s of validity left, but the provider's default skew is 60 s.
        val store = FakeTokenProvider(token(now + 30_000, "almost-expired"), clock = { now })
        val refresher = FakeSilentRefresher()
            .willReturn(SilentRefreshResult.Success(token(now + 30 * 60_000, "fresh")))

        val result = provider(store, refresher).idToken()

        assertThat((result as TokenResult.Valid).token.idToken).isEqualTo("fresh")
    }

    @Test fun reports_unavailable_when_sign_in_is_not_configured() = runBlocking {
        val store = FakeTokenProvider(null, clock = { now })
        val refresher = FakeSilentRefresher()

        val result = provider(store, refresher, configured = false).idToken()

        assertThat(result).isEqualTo(TokenResult.Unavailable)
        assertThat(refresher.attempts).isEqualTo(0)
    }

    @Test fun concurrent_workers_share_a_single_refresh() = runBlocking {
        val store = FakeTokenProvider(token(now - 1, "expired"), clock = { now })
        val refresher = FakeSilentRefresher()
            .willReturn(SilentRefreshResult.Success(token(now + 30 * 60_000, "fresh")))
        val sut = provider(store, refresher)

        val results = (1..8).map { async { sut.idToken() } }.awaitAll()

        assertThat(refresher.attempts).isEqualTo(1)
        assertThat(results.all { it is TokenResult.Valid }).isTrue()
    }

    @Test fun invalidate_forces_a_refresh_on_the_next_call() = runBlocking {
        val store = FakeTokenProvider(token(now + 30 * 60_000, "cached"), clock = { now })
        val refresher = FakeSilentRefresher()
            .willReturn(SilentRefreshResult.Success(token(now + 30 * 60_000, "fresh")))
        val sut = provider(store, refresher)

        assertThat(refresher.attempts).isEqualTo(0)
        sut.invalidate()
        val result = sut.idToken()

        assertThat(refresher.attempts).isEqualTo(1)
        assertThat((result as TokenResult.Valid).token.idToken).isEqualTo("fresh")
    }
}

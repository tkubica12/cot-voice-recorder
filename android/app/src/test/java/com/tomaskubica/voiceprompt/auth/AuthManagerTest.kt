package com.tomaskubica.voiceprompt.auth

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.testutil.FakeTokenProvider
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.util.concurrent.atomic.AtomicInteger
import androidx.credentials.exceptions.NoCredentialException
import kotlinx.coroutines.CancellationException

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class AuthManagerTest {
    private val app: Application = ApplicationProvider.getApplicationContext()

    @Test fun expired_cached_token_is_not_reported_as_signed_in_on_startup() {
        val manager = AuthManager(
            app, FakeTokenProvider(StoredToken("expired", "owner@example.com", 0L)),
            webClientId = "web-client",
        )
        assertThat(manager.state.value).isEqualTo(AuthState.SignedOut)
    }

    @Test fun foreground_check_detects_expiry_without_opening_credentials() {
        var now = 0L
        val store = FakeTokenProvider(StoredToken("token", "owner@example.com", 120_000L)) { now }
        val manager = AuthManager(app, store, webClientId = "web-client", credentialGetter = { _, _ ->
            error("Foreground validity checks must not show a chooser")
        })
        assertThat(manager.state.value).isInstanceOf(AuthState.SignedIn::class.java)
        now = 60_000L
        manager.refreshState()
        assertThat(manager.state.value).isEqualTo(AuthState.SignedOut)
    }

    @Test fun failed_restore_of_expired_token_leaves_ui_signed_out() = runBlocking {
        val manager = AuthManager(
            app, FakeTokenProvider(StoredToken("expired", "owner@example.com", 0L)),
            webClientId = "web-client",
            credentialGetter = { _, _ -> throw NoCredentialException() },
        )
        assertThat(manager.trySilentSignIn(app)).isFalse()
        assertThat(manager.state.value).isEqualTo(AuthState.SignedOut)
    }

    @Test fun cancelled_credential_request_propagates_cancellation() = runBlocking {
        val manager = AuthManager(app, FakeTokenProvider(null), webClientId = "web-client",
            credentialGetter = { _, _ -> throw CancellationException("activity stopped") })
        val request = async { manager.explicitSignIn(app) }
        request.join()
        assertThat(request.isCancelled).isTrue()
        assertThat(manager.state.value).isEqualTo(AuthState.SignedOut)
    }

    @Test
    fun valid_cached_token_survives_repeated_startup_restores_without_credential_requests() =
        runBlocking {
            val requests = AtomicInteger()
            val token = StoredToken("cached", "owner@example.com", Long.MAX_VALUE)
            val manager = AuthManager(
                appContext = app,
                tokenStore = FakeTokenProvider(token),
                webClientId = "web-client",
                credentialGetter = { _, _ ->
                    requests.incrementAndGet()
                    error("Credential Manager must not be called")
                },
            )

            val restored = listOf(
                async { manager.trySilentSignIn(app) },
                async { manager.trySilentSignIn(app) },
                async { manager.trySilentSignIn(app) },
            ).awaitAll()

            assertThat(restored).containsExactly(true, true, true)
            assertThat(requests.get()).isEqualTo(0)
            assertThat(manager.state.value).isEqualTo(AuthState.SignedIn("owner@example.com"))
        }

    @Test
    fun concurrent_activity_recreations_share_one_credential_request() = runBlocking {
        val requests = AtomicInteger()
        val requestStarted = CompletableDeferred<Unit>()
        val releaseRequest = CompletableDeferred<Unit>()
        val manager = AuthManager(
            appContext = app,
            tokenStore = FakeTokenProvider(null),
            webClientId = "web-client",
            credentialGetter = { _, _ ->
                requests.incrementAndGet()
                requestStarted.complete(Unit)
                releaseRequest.await()
                error("provider failure")
            },
        )

        val first = async { manager.trySilentSignIn(app) }
        requestStarted.await()
        val recreated = async { manager.trySilentSignIn(app) }
        releaseRequest.complete(Unit)

        awaitAll(first, recreated)

        assertThat(requests.get()).isEqualTo(1)
    }
}

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

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class AuthManagerTest {
    private val app: Application = ApplicationProvider.getApplicationContext()

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

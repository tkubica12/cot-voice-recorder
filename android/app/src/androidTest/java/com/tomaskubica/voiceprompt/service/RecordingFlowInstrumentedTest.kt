package com.tomaskubica.voiceprompt.service

import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import com.tomaskubica.voiceprompt.VoicePromptApp
import com.tomaskubica.voiceprompt.ui.MainActivity
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * Emulator instrumentation: launches the app, starts/stops the foreground recording service via
 * the toggle control, and verifies a short recording produces a persisted chunk (queue entry).
 *
 * Uses the real AudioRecord source (the emulator provides a silent virtual mic), exercising the
 * full capture path without needing live network/auth.
 */
class RecordingFlowInstrumentedTest {

    @get:Rule
    val permissions: GrantPermissionRule = GrantPermissionRule.grant(
        android.Manifest.permission.RECORD_AUDIO,
        android.Manifest.permission.POST_NOTIFICATIONS,
    )

    @get:Rule
    val compose = createAndroidComposeRule<MainActivity>()

    @Test fun app_launches_and_shows_two_controls() {
        compose.onNodeWithTag("holdToTalkButton").assertExists()
        compose.onNodeWithTag("toggleButton").assertExists()
    }

    @Test fun toggle_start_then_stop_captures_a_chunk() {
        // Start
        compose.onNodeWithTag("toggleButton").performClick()
        compose.waitUntil(timeoutMillis = 10_000) {
            RecorderState.phase.value == CapturePhase.RECORDING
        }
        // Let it capture a couple of seconds
        compose.waitUntil(timeoutMillis = 10_000) {
            RecorderState.elapsedMs.value >= 2_000
        }
        // Stop
        compose.onNodeWithTag("toggleButton").performClick()
        compose.waitUntil(timeoutMillis = 15_000) {
            RecorderState.phase.value == CapturePhase.IDLE
        }

        val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as VoicePromptApp
        val clientId = RecorderState.activeClientId.value
        assertTrue("expected an active client id", clientId != null)

        val chunks = runBlocking { app.container.repository.chunksFor(clientId!!) }
        assertTrue("expected at least one captured chunk (queue entry)", chunks.isNotEmpty())
    }
}

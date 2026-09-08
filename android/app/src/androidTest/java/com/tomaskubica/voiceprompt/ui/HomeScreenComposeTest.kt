package com.tomaskubica.voiceprompt.ui

import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.tomaskubica.voiceprompt.auth.AuthState
import com.tomaskubica.voiceprompt.data.db.RecordingEntity
import com.tomaskubica.voiceprompt.service.CapturePhase
import com.tomaskubica.voiceprompt.ui.screens.HomeScreen
import com.tomaskubica.voiceprompt.ui.theme.VoicePromptTheme
import com.tomaskubica.voiceprompt.warmup.WarmupState
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class HomeScreenComposeTest {

    @get:Rule val rule = createComposeRule()

    private fun render(state: HomeUiState, onSignIn: () -> Unit = {}, onToggle: () -> Unit = {}) {
        rule.setContent {
            VoicePromptTheme {
                HomeScreen(
                    state = state,
                    onHoldStart = {}, onHoldRelease = {}, onHoldCancel = {},
                    onToggle = onToggle, onRetry = {}, onOpenSettings = {}, onOpenHistory = {},
                    onSignIn = onSignIn, onResumeUploads = {},
                )
            }
        }
    }

    @Test fun shows_exactly_two_recording_controls() {
        render(HomeUiState())
        // Hold-to-talk is a press/hold gesture (no OnClick); the toggle is a click control.
        rule.onNodeWithTag("holdToTalkButton").assertIsDisplayed()
        rule.onNodeWithTag("toggleButton").assertIsDisplayed().assertHasClickAction()
    }

    @Test fun shows_ready_status_when_idle() {
        render(HomeUiState(warmup = WarmupState.READY, auth = AuthState.SignedOut))
        rule.onNodeWithTag("statusText").assertTextEquals("Ready")
    }

    @Test fun shows_recording_status_elapsed_and_compact_progress() {
        render(
            HomeUiState(
                phase = CapturePhase.RECORDING,
                elapsedMs = 65_400,
                pendingChunks = 3,
            ),
        )
        rule.onNodeWithTag("statusText").assertTextEquals("Recording")
        rule.onNodeWithTag("elapsedText").assertTextEquals("1:05.4")
        rule.onNodeWithTag("chunkProgress").assertIsDisplayed()
    }

    @Test fun backend_is_an_icon_and_account_details_stay_off_home() {
        render(
            HomeUiState(
                warmup = WarmupState.CHECKING,
                auth = AuthState.SignedIn("private@example.com"),
            ),
        )
        rule.onNodeWithTag("backendIndicator").assertIsDisplayed()
        rule.onNodeWithContentDescription("Backend connecting").assertIsDisplayed()
        rule.onAllNodesWithText("private@example.com").assertCountEquals(0)
    }

    @Test fun toggle_invokes_callback() {
        var toggled = false
        render(HomeUiState()) { toggled = true }
        rule.onNodeWithTag("toggleButton").performClick()
        assertTrue(toggled)
    }

    @Test fun top_bar_actions_present_for_settings_and_history() {
        render(HomeUiState())
        rule.onNodeWithTag("settingsAction").assertIsDisplayed()
        rule.onNodeWithTag("historyAction").assertIsDisplayed()
    }

    private fun recording(server: String) = RecordingEntity(
        clientRecordingId = "c", localState = "COMPLETING", serverState = server,
        refineModel = "gpt-5.6-luna", language = "cs",
        startedAtEpochMs = 0, createdAtEpochMs = 0, updatedAtEpochMs = 0,
    )

    @Test fun transcribing_state_is_reflected() {
        render(HomeUiState(phase = CapturePhase.IDLE, activeRecording = recording("TRANSCRIBING")))
        rule.onNodeWithTag("statusText").assertTextEquals("Transcribing")
        rule.onNodeWithTag("processingIndicator").assertIsDisplayed()
    }

    @Test fun expired_sign_in_replaces_upload_spinner_with_actionable_prompt() {
        var signInRequested = false
        render(
            HomeUiState(
                auth = AuthState.SignedOut, pendingChunks = 35,
                activeRecording = recording("RECORDING"),
            ),
            onSignIn = { signInRequested = true },
        )
        rule.onNodeWithTag("statusText").assertTextEquals("Sign-in required")
        rule.onNodeWithTag("processingIndicator").assertDoesNotExist()
        rule.onNodeWithTag("uploadSignInButton").assertIsDisplayed().performClick()
        assertTrue(signInRequested)
        rule.onNodeWithTag("toggleButton").assertIsEnabled()
    }

    @Test fun expiry_does_not_hide_or_disable_ongoing_recording_controls() {
        render(HomeUiState(auth = AuthState.SignedOut, phase = CapturePhase.RECORDING, pendingChunks = 1))
        rule.onNodeWithTag("statusText").assertTextEquals("Recording")
        rule.onNodeWithTag("uploadAuthBanner").assertIsDisplayed()
        rule.onNodeWithTag("toggleButton").assertIsEnabled().performClick()
    }
}

package com.tomaskubica.voiceprompt.ui

import android.Manifest
import android.app.Application
import android.app.KeyguardManager
import android.content.Intent
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.service.RecorderState
import com.tomaskubica.voiceprompt.service.RecordingService
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = Application::class)
class QuickRecordActivityTest {
    private val app: Application = ApplicationProvider.getApplicationContext()

    @Before fun setUp() {
        RecorderState.onIdle()
        shadowOf(app).grantPermissions(Manifest.permission.RECORD_AUDIO)
    }

    @After fun tearDown() {
        RecorderState.onIdle()
    }

    @Test fun explicit_action_starts_only_after_resume_even_when_locked() {
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setKeyguardLocked(true)
        val controller = Robolectric.buildActivity(QuickRecordActivity::class.java, QuickRecordActivity.intent(app))
            .create().start()
        assertThat(shadowOf(app).nextStartedService).isNull()
        controller.resume().visible()
        assertThat(shadowOf(app).nextStartedService.action).isEqualTo(RecordingService.ACTION_START)
        assertThat(app.getSystemService(KeyguardManager::class.java).isKeyguardLocked).isTrue()
        controller.pause().stop().destroy()
    }

    @Test fun notification_open_without_action_does_not_start_recording() {
        val controller = Robolectric.buildActivity(
            QuickRecordActivity::class.java, Intent(app, QuickRecordActivity::class.java),
        ).setup()
        assertThat(shadowOf(app).nextStartedService).isNull()
        controller.pause().stop().destroy()
    }

    @Test fun missing_microphone_permission_does_not_start_or_open_private_ui() {
        shadowOf(app).denyPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(QuickRecordActivity::class.java, QuickRecordActivity.intent(app)).setup()
        assertThat(shadowOf(app).nextStartedService).isNull()
        assertThat(shadowOf(app).nextStartedActivity).isNull()
        controller.pause().stop().destroy()
    }

    @Test fun repeat_shortcut_does_not_stop_or_duplicate_existing_recording() {
        RecorderState.onStart("already-recording")
        val controller = Robolectric.buildActivity(QuickRecordActivity::class.java, QuickRecordActivity.intent(app)).setup()
        assertThat(shadowOf(app).nextStartedService).isNull()
        controller.newIntent(QuickRecordActivity.intent(app))
        assertThat(shadowOf(app).nextStartedService).isNull()
        controller.pause().stop().destroy()
    }

    @Test fun recreation_and_screen_off_resume_do_not_restart_recording() {
        val controller = Robolectric.buildActivity(QuickRecordActivity::class.java, QuickRecordActivity.intent(app)).setup()
        assertThat(shadowOf(app).nextStartedService.action).isEqualTo(RecordingService.ACTION_START)
        controller.recreate()
        controller.pause().stop().start().resume()
        assertThat(shadowOf(app).nextStartedService).isNull()
        controller.pause().stop().destroy()
    }
}

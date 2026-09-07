package com.tomaskubica.voiceprompt.ui

import android.Manifest
import android.app.Application
import android.app.NotificationManager
import android.content.pm.ShortcutManager
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import com.tomaskubica.voiceprompt.data.SettingsStore
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = Application::class)
class QuickRecordEntryPointsTest {
    private val app: Application = ApplicationProvider.getApplicationContext()

    @Test fun shortcut_targets_quick_screen_with_explicit_start_action() {
        QuickRecordEntryPoints.registerShortcut(app)
        val shortcut = app.getSystemService(ShortcutManager::class.java).dynamicShortcuts.single()
        assertThat(shortcut.intent!!.component!!.className).isEqualTo(QuickRecordActivity::class.java.name)
        assertThat(shortcut.intent!!.action).isEqualTo(QuickRecordActivity.ACTION_QUICK_RECORD)
    }

    @Test fun notification_is_opt_in_and_can_be_disabled() {
        shadowOf(app).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        val manager = app.getSystemService(NotificationManager::class.java)
        QuickRecordEntryPoints.refreshNotification(app)
        assertThat(manager.activeNotifications).isEmpty()
        SettingsStore(app).quickRecordNotification = true
        QuickRecordEntryPoints.refreshNotification(app)
        val notification = manager.activeNotifications.single().notification
        val intent = shadowOf(notification.contentIntent).savedIntent
        assertThat(intent.component!!.className).isEqualTo(QuickRecordActivity::class.java.name)
        assertThat(intent.action).isEqualTo(QuickRecordActivity.ACTION_QUICK_RECORD)
        SettingsStore(app).quickRecordNotification = false
        QuickRecordEntryPoints.refreshNotification(app)
        assertThat(manager.activeNotifications).isEmpty()
    }

    @Test fun denied_notifications_do_not_crash_or_post() {
        shadowOf(app).denyPermissions(Manifest.permission.POST_NOTIFICATIONS)
        SettingsStore(app).quickRecordNotification = true
        QuickRecordEntryPoints.refreshNotification(app)
        assertThat(app.getSystemService(NotificationManager::class.java).activeNotifications).isEmpty()
    }
}

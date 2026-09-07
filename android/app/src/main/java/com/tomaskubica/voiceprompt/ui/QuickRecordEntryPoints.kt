package com.tomaskubica.voiceprompt.ui

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.pm.PackageManager
import android.content.pm.ShortcutInfo
import android.content.pm.ShortcutManager
import android.graphics.drawable.Icon
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import com.tomaskubica.voiceprompt.R
import com.tomaskubica.voiceprompt.data.SettingsStore

object QuickRecordEntryPoints {
    private const val CHANNEL_ID = "quick_record"
    private const val NOTIFICATION_ID = 43
    private const val SHORTCUT_ID = "quick_record"

    fun registerShortcut(context: Context) {
        context.getSystemService(ShortcutManager::class.java).addDynamicShortcuts(
            listOf(
                ShortcutInfo.Builder(context, SHORTCUT_ID)
                    .setShortLabel(context.getString(R.string.quick_record))
                    .setIcon(Icon.createWithResource(context, R.drawable.ic_notification))
                    .setIntent(QuickRecordActivity.intent(context))
                    .build(),
            ),
        )
    }

    fun refreshNotification(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        if (!SettingsStore(context).quickRecordNotification) {
            manager.cancel(NOTIFICATION_ID)
            return
        }
        manager.createNotificationChannel(NotificationChannel(
            CHANNEL_ID,
            context.getString(R.string.quick_notification_channel),
            NotificationManager.IMPORTANCE_LOW,
        ))
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) return
        val start = PendingIntent.getActivity(
            context, 2, QuickRecordActivity.intent(context),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        manager.notify(
            NOTIFICATION_ID,
            NotificationCompat.Builder(context, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(context.getString(R.string.quick_record))
                .setContentText(context.getString(R.string.quick_notification_text))
                .setContentIntent(start)
                .addAction(0, context.getString(R.string.toggle_record), start)
                .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
                .setOngoing(true)
                .setSilent(true)
                .build(),
        )
    }
}

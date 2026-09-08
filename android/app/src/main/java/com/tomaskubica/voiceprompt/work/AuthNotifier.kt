package com.tomaskubica.voiceprompt.work

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import com.tomaskubica.voiceprompt.R
import com.tomaskubica.voiceprompt.ui.MainActivity

/**
 * Surfaces "you must sign in again" from background upload work.
 *
 * Background workers must never launch an Activity, so an expired session is reported as a
 * tappable notification instead: uploads keep retrying (nothing is lost) but the user is
 * told why progress has stalled, rather than facing an opaque infinite retry.
 *
 * Posting is idempotent — the same notification id is reused — and the notification is
 * cleared as soon as a token is obtained again.
 */
interface AuthNotifier {
    fun signInRequired()

    fun clear()
}

class AndroidAuthNotifier(context: Context) : AuthNotifier {
    private val appContext = context.applicationContext

    override fun signInRequired() {
        // Runtime check kept inline so both the framework and lint can see it.
        if (ContextCompat.checkSelfPermission(appContext, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        ensureChannel()
        val contentIntent = PendingIntent.getActivity(
            appContext,
            AUTH_REQUEST_CODE,
            Intent(appContext, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(appContext, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(appContext.getString(R.string.auth_needed))
            .setContentText(appContext.getString(R.string.auth_needed_body))
            .setStyle(
                NotificationCompat.BigTextStyle()
                    .bigText(appContext.getString(R.string.auth_needed_body)),
            )
            .setContentIntent(contentIntent)
            .setAutoCancel(true)
            .setOnlyAlertOnce(true)
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .build()
        // The permission can still be revoked between the check and the call.
        runCatching {
            NotificationManagerCompat.from(appContext).notify(NOTIFICATION_ID, notification)
        }
    }

    override fun clear() {
        runCatching { NotificationManagerCompat.from(appContext).cancel(NOTIFICATION_ID) }
    }

    private fun ensureChannel() {
        val manager = appContext.getSystemService(NotificationManager::class.java) ?: return
        val channel = NotificationChannel(
            CHANNEL_ID,
            appContext.getString(R.string.auth_channel_name),
            NotificationManager.IMPORTANCE_DEFAULT,
        ).apply { description = appContext.getString(R.string.auth_channel_desc) }
        manager.createNotificationChannel(channel)
    }

    companion object {
        const val CHANNEL_ID = "auth"
        const val NOTIFICATION_ID = 44
        private const val AUTH_REQUEST_CODE = 2
    }
}

/** No-op notifier for tests and for contexts where notifications are irrelevant. */
object NoOpAuthNotifier : AuthNotifier {
    override fun signInRequired() = Unit

    override fun clear() = Unit
}

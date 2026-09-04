package com.tomaskubica.voiceprompt.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import com.tomaskubica.voiceprompt.R
import com.tomaskubica.voiceprompt.VoicePromptApp
import com.tomaskubica.voiceprompt.audio.AudioRecordSource
import com.tomaskubica.voiceprompt.audio.PcmSource
import com.tomaskubica.voiceprompt.ui.MainActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import java.util.UUID

/**
 * Foreground service that owns microphone capture and chunk enqueueing. Started while the activity
 * is visible; it keeps recording when the screen locks, holding a partial wake lock only while
 * recording and a `microphone` foreground-service type. A persistent notification exposes Stop.
 */
class RecordingService : Service() {

    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var recordJob: Job? = null

    @Volatile private var stopRequested = false
    @Volatile private var cancelRequested = false

    private var wakeLock: PowerManager.WakeLock? = null
    private var audioFocusRequest: AudioFocusRequest? = null

    private val container get() = (application as VoicePromptApp).container

    /** Overridable for instrumentation: default is the real AudioRecord source. */
    private val sourceFactory: () -> PcmSource = { AudioRecordSource() }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> handleStart()
            ACTION_STOP -> handleStop(cancel = false)
            ACTION_CANCEL -> handleStop(cancel = true)
        }
        return START_NOT_STICKY
    }

    private fun handleStart() {
        if (recordJob != null) return // already recording
        stopRequested = false
        cancelRequested = false

        val clientId = UUID.randomUUID().toString()
        val startEpoch = System.currentTimeMillis()

        startForegroundNotification()
        acquireWakeLock()
        requestAudioFocus()
        RecorderState.onStart(clientId)

        recordJob = scope.launch {
            val refineModel = container.settings.refineModel
            container.repository.createLocalRecording(
                clientRecordingId = clientId,
                refineModel = refineModel,
                language = "cs",
                startedAtEpochMs = startEpoch,
            )
            val controller = RecordingController(
                repository = container.repository,
                scheduler = container.scheduler,
                sourceFactory = sourceFactory,
            )
            // NOTE: never collapse a failure into "0 chunks". CaptureFinalizer asks the
            // repository what is actually on disk so a mid-recording error preserves and
            // uploads every persisted WAV instead of deleting the recording.
            val outcome = runCatching {
                controller.record(clientId, startEpoch) { stopRequested }
            }
            CaptureFinalizer(
                repository = container.repository,
                scheduler = container.scheduler,
            ).finish(clientId, outcome, cancelled = cancelRequested)
            finish()
        }
    }

    private fun handleStop(cancel: Boolean) {
        cancelRequested = cancel
        stopRequested = true
        RecorderState.onStopping()
    }

    private fun finish() {
        releaseWakeLock()
        abandonAudioFocus()
        recordJob = null
        RecorderState.onIdle()
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        // Graceful shutdown: request stop so capture finalizes and audio is not lost.
        stopRequested = true
        releaseWakeLock()
        abandonAudioFocus()
        super.onDestroy()
    }

    // ------------------------------------------------------------ foreground
    private fun startForegroundNotification() {
        val notification = buildNotification()
        val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        } else {
            0
        }
        ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, type)
    }

    private fun buildNotification(): Notification {
        val contentIntent = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val stopIntent = PendingIntent.getService(
            this, 1, Intent(this, RecordingService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(getString(R.string.notification_text))
            .setOngoing(true)
            .setSilent(true)
            .setContentIntent(contentIntent)
            .addAction(0, getString(R.string.notification_stop), stopIntent)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    private fun createChannel() {
        val mgr = getSystemService(NotificationManager::class.java)
        val channel = NotificationChannel(
            CHANNEL_ID,
            getString(R.string.notification_channel_name),
            NotificationManager.IMPORTANCE_LOW,
        ).apply { description = getString(R.string.notification_channel_desc) }
        mgr.createNotificationChannel(channel)
    }

    // ------------------------------------------------------------ wake lock
    private fun acquireWakeLock() {
        val pm = getSystemService(PowerManager::class.java)
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, WAKE_TAG).apply {
            setReferenceCounted(false)
            acquire(MAX_WAKE_MS)
        }
    }

    private fun releaseWakeLock() {
        wakeLock?.let { if (it.isHeld) runCatching { it.release() } }
        wakeLock = null
    }

    // ------------------------------------------------------------ audio focus
    private fun requestAudioFocus() {
        val am = getSystemService(AudioManager::class.java)
        val attrs = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()
        val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE)
            .setAudioAttributes(attrs)
            .setWillPauseWhenDucked(false)
            .build()
        audioFocusRequest = req
        runCatching { am.requestAudioFocus(req) }
    }

    private fun abandonAudioFocus() {
        val am = getSystemService(AudioManager::class.java)
        audioFocusRequest?.let { runCatching { am.abandonAudioFocusRequest(it) } }
        audioFocusRequest = null
    }

    companion object {
        const val ACTION_START = "com.tomaskubica.voiceprompt.action.START"
        const val ACTION_STOP = "com.tomaskubica.voiceprompt.action.STOP"
        const val ACTION_CANCEL = "com.tomaskubica.voiceprompt.action.CANCEL"

        private const val CHANNEL_ID = "recording"
        private const val NOTIFICATION_ID = 42
        private const val WAKE_TAG = "voiceprompt:recording"
        private const val MAX_WAKE_MS = 4L * 60 * 60 * 1000 // safety cap

        fun start(context: Context) {
            val intent = Intent(context, RecordingService::class.java).setAction(ACTION_START)
            context.startForegroundService(intent)
        }

        fun stop(context: Context) {
            context.startService(Intent(context, RecordingService::class.java).setAction(ACTION_STOP))
        }

        fun cancel(context: Context) {
            context.startService(Intent(context, RecordingService::class.java).setAction(ACTION_CANCEL))
        }
    }
}

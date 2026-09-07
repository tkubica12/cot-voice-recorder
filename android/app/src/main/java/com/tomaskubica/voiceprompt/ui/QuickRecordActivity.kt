package com.tomaskubica.voiceprompt.ui

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.tomaskubica.voiceprompt.R
import com.tomaskubica.voiceprompt.service.CapturePhase
import com.tomaskubica.voiceprompt.service.RecorderState
import com.tomaskubica.voiceprompt.service.RecordingService
import com.tomaskubica.voiceprompt.ui.theme.VoicePromptTheme
import java.util.Locale

/** Deliberately has no navigation into private app data and never dismisses the keyguard. */
class QuickRecordActivity : ComponentActivity() {
    private var pendingStart = false
    private var error by mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        pendingStart = savedInstanceState?.getBoolean(KEY_PENDING_START)
            ?: (intent.action == ACTION_QUICK_RECORD)
        setContent {
            val phase by RecorderState.phase.collectAsStateWithLifecycle()
            val elapsed by RecorderState.elapsedMs.collectAsStateWithLifecycle()
            VoicePromptTheme {
                Surface {
                    Column(
                        modifier = Modifier.fillMaxSize().padding(24.dp),
                        verticalArrangement = Arrangement.spacedBy(24.dp, Alignment.CenterVertically),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Text(stringResource(R.string.quick_record), style = MaterialTheme.typography.headlineMedium)
                        Text(stringResource(when (phase) {
                            CapturePhase.RECORDING -> R.string.state_recording
                            CapturePhase.STOPPING -> R.string.quick_stopping
                            CapturePhase.IDLE -> R.string.state_idle
                        }))
                        Text(
                            String.format(Locale.ROOT, "%02d:%02d", elapsed / 60_000, elapsed / 1_000 % 60),
                            style = MaterialTheme.typography.displayMedium,
                        )
                        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                        Button(
                            onClick = {
                                if (phase == CapturePhase.RECORDING) {
                                    RecordingService.stop(this@QuickRecordActivity)
                                } else {
                                    startCapture()
                                }
                            },
                            enabled = phase != CapturePhase.STOPPING,
                            modifier = Modifier.testTag("quickRecordToggle"),
                        ) {
                            Text(stringResource(
                                if (phase == CapturePhase.RECORDING) R.string.toggle_stop else R.string.toggle_record,
                            ))
                        }
                        Text(stringResource(R.string.quick_private_hint))
                        TextButton(onClick = { finish() }) {
                            Text(stringResource(
                                if (phase == CapturePhase.RECORDING) R.string.quick_close_recording else R.string.quick_close,
                            ))
                        }
                    }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        pendingStart = intent.action == ACTION_QUICK_RECORD
    }

    // A microphone foreground service must start only after this activity becomes visible.
    override fun onPostResume() {
        super.onPostResume()
        if (pendingStart) {
            pendingStart = false
            startCapture()
        }
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean(KEY_PENDING_START, pendingStart)
        super.onSaveInstanceState(outState)
    }

    private fun startCapture() {
        error = null
        if (RecorderState.phase.value != CapturePhase.IDLE) return
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            error = getString(R.string.quick_permission_needed)
            return
        }
        try {
            RecordingService.start(this, confirmStart = true)
        } catch (e: SecurityException) {
            error = getString(R.string.quick_start_failed, e.localizedMessage ?: e.javaClass.simpleName)
        } catch (e: IllegalStateException) {
            error = getString(R.string.quick_start_failed, e.localizedMessage ?: e.javaClass.simpleName)
        }
    }

    companion object {
        const val ACTION_QUICK_RECORD = "com.tomaskubica.voiceprompt.action.QUICK_RECORD"
        private const val KEY_PENDING_START = "pending_start"

        fun intent(context: Context): Intent =
            Intent(context, QuickRecordActivity::class.java)
                .setAction(ACTION_QUICK_RECORD)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    }
}

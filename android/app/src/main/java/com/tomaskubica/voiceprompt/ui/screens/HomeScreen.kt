package com.tomaskubica.voiceprompt.ui.screens

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.paddingFromBaseline
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Mic
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.outlined.Cloud
import androidx.compose.material.icons.outlined.CloudUpload
import androidx.compose.material.icons.outlined.GraphicEq
import androidx.compose.material.icons.outlined.History
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.tomaskubica.voiceprompt.ui.DisplayStatus
import com.tomaskubica.voiceprompt.ui.HomeUiState
import com.tomaskubica.voiceprompt.ui.Statuses
import com.tomaskubica.voiceprompt.ui.theme.Orange
import com.tomaskubica.voiceprompt.warmup.WarmupState

/**
 * Main screen: two prominent recording controls with a minimal status hero. Configuration and
 * account details stay in Settings instead of competing with the primary recording workflow.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(
    state: HomeUiState,
    onHoldStart: () -> Unit,
    onHoldRelease: () -> Unit,
    onHoldCancel: () -> Unit,
    onToggle: () -> Unit,
    onRetry: (String) -> Unit,
    onOpenSettings: () -> Unit,
    onOpenHistory: () -> Unit,
) {
    val status = Statuses.displayStatus(state.phase, state.activeRecording, state.pendingChunks)
    val recording = state.isRecording
    val stopping = state.isStopping

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        "VoicePrompt",
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                },
                actions = {
                    BackendIndicator(state.warmup)
                    IconButton(onClick = onOpenHistory, modifier = Modifier.testTag("historyAction")) {
                        Icon(Icons.Outlined.History, contentDescription = "History")
                    }
                    IconButton(onClick = onOpenSettings, modifier = Modifier.testTag("settingsAction")) {
                        Icon(Icons.Outlined.Settings, contentDescription = "Settings")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 24.dp, vertical = 12.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            StatusHero(state, status)

            Spacer(Modifier.weight(1f))

            HoldToTalkButton(
                enabled = !stopping,
                recording = recording,
                onStart = onHoldStart,
                onRelease = onHoldRelease,
                onCancel = onHoldCancel,
            )

            Spacer(Modifier.size(22.dp))

            ToggleButton(
                enabled = !stopping,
                recording = recording,
                onToggle = onToggle,
            )

            if (status == DisplayStatus.FAILED && state.activeRecording != null) {
                val clientId = state.activeRecording.clientRecordingId
                Surface(
                    color = Orange.copy(alpha = 0.08f),
                    shape = MaterialTheme.shapes.large,
                    border = BorderStroke(1.dp, Orange.copy(alpha = 0.24f)),
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 14.dp),
                ) {
                    Column(
                        modifier = Modifier.padding(14.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Text(
                            if (state.retryError == null) {
                                "Upload failed. Your audio is safe."
                            } else {
                                "Retry could not start. Your audio is safe."
                            },
                            color = Orange,
                            textAlign = TextAlign.Center,
                        )
                        TextButton(
                            onClick = { onRetry(clientId) },
                            modifier = Modifier.testTag("retryButton"),
                        ) {
                            Text("Retry upload")
                        }
                    }
                }
            }

            Spacer(Modifier.weight(0.45f))
        }
    }
}

@Composable
private fun StatusHero(state: HomeUiState, status: DisplayStatus) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(top = 18.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = statusLabel(status),
            color = Orange,
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.SemiBold,
            letterSpacing = 1.2.sp,
            modifier = Modifier.testTag("statusText"),
        )
        Text(
            text = Statuses.formatElapsed(state.elapsedMs),
            style = MaterialTheme.typography.displayMedium,
            fontWeight = FontWeight.Light,
            letterSpacing = (-1.5).sp,
            modifier = Modifier
                .paddingFromBaseline(top = 66.dp, bottom = 10.dp)
                .testTag("elapsedText"),
        )

        if (status in PROCESSING_STATES) {
            LinearProgressIndicator(
                color = Orange,
                trackColor = Orange.copy(alpha = 0.14f),
                modifier = Modifier
                    .size(width = 74.dp, height = 3.dp)
                    .testTag("processingIndicator"),
            )
        } else {
            Spacer(Modifier.size(height = 3.dp, width = 74.dp))
        }

        if (state.capturedChunks > 0 || state.pendingChunks > 0) {
            Row(
                modifier = Modifier
                    .padding(top = 16.dp)
                    .semantics {
                        contentDescription =
                            "${state.capturedChunks} chunks captured, ${state.pendingChunks} queued"
                    }
                    .testTag("chunkProgress"),
                horizontalArrangement = Arrangement.spacedBy(18.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                CompactCount(Icons.Outlined.GraphicEq, state.capturedChunks)
                CompactCount(Icons.Outlined.CloudUpload, state.pendingChunks)
            }
        }
    }
}

@Composable
private fun CompactCount(icon: androidx.compose.ui.graphics.vector.ImageVector, count: Int) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            icon,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(17.dp),
        )
        Text(
            count.toString(),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            style = MaterialTheme.typography.labelLarge,
        )
    }
}

@Composable
private fun BackendIndicator(warmup: WarmupState) {
    val connected = warmup == WarmupState.READY
    val description = when (warmup) {
        WarmupState.CHECKING -> "Backend connecting"
        WarmupState.READY -> "Backend ready"
        WarmupState.OFFLINE -> "Backend offline"
    }
    Icon(
        Icons.Outlined.Cloud,
        contentDescription = description,
        tint = if (connected) Orange else MaterialTheme.colorScheme.outline.copy(alpha = 0.55f),
        modifier = Modifier
            .padding(horizontal = 8.dp)
            .size(23.dp)
            .testTag("backendIndicator"),
    )
}

@Composable
private fun HoldToTalkButton(
    enabled: Boolean,
    recording: Boolean,
    onStart: () -> Unit,
    onRelease: () -> Unit,
    onCancel: () -> Unit,
) {
    val haptics = LocalHapticFeedback.current
    Box(
        modifier = Modifier
            .size(216.dp)
            .alpha(if (enabled) 1f else 0.45f)
            .border(
                1.dp,
                Orange.copy(alpha = if (recording) 0.7f else 0.28f),
                CircleShape,
            )
            .padding(8.dp)
            .semantics { contentDescription = "Hold to talk. Press and hold to record, release to finish." }
            .testTag("holdToTalkButton")
            .pointerInput(enabled) {
                if (!enabled) return@pointerInput
                detectTapGestures(
                    onPress = {
                        haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                        onStart()
                        val released = tryAwaitRelease()
                        if (released) onRelease() else onCancel()
                    },
                )
            },
        contentAlignment = Alignment.Center,
    ) {
        Surface(
            color = Orange,
            shape = CircleShape,
            shadowElevation = 8.dp,
            modifier = Modifier
                .fillMaxSize()
                .clip(CircleShape),
        ) {
            Column(
                Modifier.fillMaxSize(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.Center,
            ) {
                Icon(
                    Icons.Default.Mic,
                    contentDescription = null,
                    tint = Color.White,
                    modifier = Modifier.size(58.dp),
                )
                Spacer(Modifier.size(10.dp))
                Text(
                    "Hold to talk",
                    color = Color.White,
                    fontSize = 16.sp,
                    fontWeight = FontWeight.SemiBold,
                )
            }
        }
    }
}

@Composable
private fun ToggleButton(
    enabled: Boolean,
    recording: Boolean,
    onToggle: () -> Unit,
) {
    if (recording) {
        Button(
            onClick = onToggle,
            enabled = enabled,
            colors = ButtonDefaults.buttonColors(
                containerColor = Orange,
                contentColor = Color.White,
            ),
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 64.dp)
                .testTag("toggleButton")
                .semantics { contentDescription = "Stop recording" },
        ) {
            Icon(Icons.Default.Stop, contentDescription = null)
            Spacer(Modifier.size(8.dp))
            Text("Stop", fontSize = 18.sp)
        }
    } else {
        OutlinedButton(
            onClick = onToggle,
            enabled = enabled,
            border = BorderStroke(1.dp, Orange.copy(alpha = 0.55f)),
            colors = ButtonDefaults.outlinedButtonColors(contentColor = Orange),
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 64.dp)
                .testTag("toggleButton")
                .semantics { contentDescription = "Start recording. Tap to start; you may lock your phone. Tap again to stop." },
        ) {
            Icon(Icons.Default.Mic, contentDescription = null)
            Spacer(Modifier.size(8.dp))
            Text("Tap to record", fontSize = 18.sp, fontWeight = FontWeight.Medium)
        }
    }
}

private fun statusLabel(status: DisplayStatus): String = when (status) {
    DisplayStatus.IDLE -> "Ready"
    DisplayStatus.RECORDING -> "Recording"
    DisplayStatus.UPLOADING -> "Uploading"
    DisplayStatus.RETRYING -> "Retrying…"
    DisplayStatus.TRANSCRIBING -> "Transcribing"
    DisplayStatus.REFINING -> "Refining"
    DisplayStatus.COMPLETED -> "Completed"
    DisplayStatus.FAILED -> "Failed"
}

private val PROCESSING_STATES = setOf(
    DisplayStatus.UPLOADING,
    DisplayStatus.RETRYING,
    DisplayStatus.TRANSCRIBING,
    DisplayStatus.REFINING,
)

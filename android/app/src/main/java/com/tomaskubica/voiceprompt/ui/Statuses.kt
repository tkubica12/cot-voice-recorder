package com.tomaskubica.voiceprompt.ui

import com.tomaskubica.voiceprompt.data.db.RecordingEntity
import com.tomaskubica.voiceprompt.service.CapturePhase

/** Coarse status shown to the user, derived from capture phase + server state. */
enum class DisplayStatus { IDLE, RECORDING, UPLOADING, RETRYING, TRANSCRIBING, REFINING, COMPLETED, FAILED }

/** Pure derivation of the display status so it can be unit tested without Compose. */
object Statuses {
    fun displayStatus(
        phase: CapturePhase,
        recording: RecordingEntity?,
        pendingChunks: Int,
    ): DisplayStatus {
        if (phase == CapturePhase.RECORDING) return DisplayStatus.RECORDING
        if (phase == CapturePhase.STOPPING) return DisplayStatus.UPLOADING
        if (recording == null) return DisplayStatus.IDLE
        // A user-initiated retry is in flight: show progress rather than a sticky failure. The
        // marker is cleared as soon as the upload settles (completed or failed again).
        if (recording.retryRequestedAtEpochMs != null) return DisplayStatus.RETRYING
        if (recording.localState == "FAILED") return DisplayStatus.FAILED
        return when (recording.serverState) {
            "COMPLETED" -> DisplayStatus.COMPLETED
            "FAILED" -> DisplayStatus.FAILED
            "REFINING" -> DisplayStatus.REFINING
            "TRANSCRIBING" -> DisplayStatus.TRANSCRIBING
            "UPLOADING" -> DisplayStatus.UPLOADING
            else -> if (pendingChunks > 0) DisplayStatus.UPLOADING else DisplayStatus.IDLE
        }
    }

    /** Format elapsed milliseconds as m:ss.d (tenths). */
    fun formatElapsed(elapsedMs: Long): String {
        val totalTenths = elapsedMs / 100
        val tenths = totalTenths % 10
        val totalSeconds = totalTenths / 10
        val seconds = totalSeconds % 60
        val minutes = totalSeconds / 60
        return "%d:%02d.%d".format(minutes, seconds, tenths)
    }
}

package com.tomaskubica.voiceprompt.service

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** High-level capture phase surfaced to the UI. */
enum class CapturePhase { IDLE, RECORDING, STOPPING }

/**
 * Process-wide, observable capture state shared between the foreground service (writer) and the
 * UI (reader). Kept as a singleton so the activity reflects capture even across recreation.
 */
object RecorderState {
    private val _phase = MutableStateFlow(CapturePhase.IDLE)
    val phase: StateFlow<CapturePhase> = _phase.asStateFlow()

    private val _activeClientId = MutableStateFlow<String?>(null)
    val activeClientId: StateFlow<String?> = _activeClientId.asStateFlow()

    private val _elapsedMs = MutableStateFlow(0L)
    val elapsedMs: StateFlow<Long> = _elapsedMs.asStateFlow()

    private val _capturedChunks = MutableStateFlow(0)
    val capturedChunks: StateFlow<Int> = _capturedChunks.asStateFlow()

    fun onStart(clientId: String) {
        _activeClientId.value = clientId
        _elapsedMs.value = 0L
        _capturedChunks.value = 0
        _phase.value = CapturePhase.RECORDING
    }

    fun onElapsed(ms: Long) { _elapsedMs.value = ms }

    fun onChunkCaptured(count: Int) { _capturedChunks.value = count }

    fun onStopping() { _phase.value = CapturePhase.STOPPING }

    fun onIdle() {
        _phase.value = CapturePhase.IDLE
        _elapsedMs.value = 0L
    }
}

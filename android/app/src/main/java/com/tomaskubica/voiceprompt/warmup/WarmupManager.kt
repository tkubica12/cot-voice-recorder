package com.tomaskubica.voiceprompt.warmup

import com.tomaskubica.voiceprompt.data.api.VoiceApiClient
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.withContext

/** Backend warmup state. Recording never waits on this. */
enum class WarmupState { CHECKING, READY, OFFLINE }

/**
 * Fires an asynchronous, unauthenticated `/health/ready` warmup probe and exposes the result.
 * The UI shows cold-start (CHECKING) / READY / OFFLINE, but recording is always enabled.
 */
class WarmupManager(private val api: VoiceApiClient) {
    private val _state = MutableStateFlow(WarmupState.CHECKING)
    val state: StateFlow<WarmupState> = _state.asStateFlow()

    /** Perform one warmup probe (call from a coroutine). */
    suspend fun probe() {
        _state.value = WarmupState.CHECKING
        val ok = withContext(Dispatchers.IO) { api.healthReady() }
        _state.value = if (ok) WarmupState.READY else WarmupState.OFFLINE
    }
}

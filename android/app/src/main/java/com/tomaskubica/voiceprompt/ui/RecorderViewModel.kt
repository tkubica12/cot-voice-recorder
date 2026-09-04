package com.tomaskubica.voiceprompt.ui

import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.tomaskubica.voiceprompt.AppDependencies
import com.tomaskubica.voiceprompt.VoicePromptApp
import com.tomaskubica.voiceprompt.auth.AuthState
import com.tomaskubica.voiceprompt.auth.StoredToken
import com.tomaskubica.voiceprompt.auth.TokenResult
import com.tomaskubica.voiceprompt.data.RetryOutcome
import com.tomaskubica.voiceprompt.data.api.ApiResult
import com.tomaskubica.voiceprompt.data.api.TranscriptSummaryDto
import com.tomaskubica.voiceprompt.data.db.RecordingEntity
import com.tomaskubica.voiceprompt.data.model.LocalRecordingState
import com.tomaskubica.voiceprompt.data.model.ServerRecordingState
import com.tomaskubica.voiceprompt.service.CapturePhase
import com.tomaskubica.voiceprompt.service.RecorderState
import com.tomaskubica.voiceprompt.service.RecordingService
import com.tomaskubica.voiceprompt.warmup.WarmupState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class HomeUiState(
    val warmup: WarmupState = WarmupState.CHECKING,
    val auth: AuthState = AuthState.SignedOut,
    val phase: CapturePhase = CapturePhase.IDLE,
    val elapsedMs: Long = 0L,
    val capturedChunks: Int = 0,
    val pendingChunks: Int = 0,
    val activeRecording: RecordingEntity? = null,
    /** Set when a user-initiated retry could not even be scheduled; no audio was lost. */
    val retryError: String? = null,
) {
    val isRecording: Boolean get() = phase == CapturePhase.RECORDING
    val isStopping: Boolean get() = phase == CapturePhase.STOPPING
}

data class HistoryUiState(
    val loading: Boolean = false,
    val items: List<TranscriptSummaryDto> = emptyList(),
    val error: String? = null,
)

@OptIn(ExperimentalCoroutinesApi::class)
class RecorderViewModel @JvmOverloads constructor(
    app: Application,
    private val container: AppDependencies = (app as VoicePromptApp).container,
    private val statusPollIntervalMs: Long = STATUS_POLL_INTERVAL_MS,
    private val statusPollingEnabled: Boolean = true,
) : AndroidViewModel(app) {

    private val backendUrlState = MutableStateFlow(container.settings.backendUrl)
    val backendUrl: StateFlow<String> = backendUrlState.asStateFlow()

    private val retryErrorState = MutableStateFlow<String?>(null)

    private val captureInfo = combine(
        RecorderState.phase,
        RecorderState.elapsedMs,
        RecorderState.capturedChunks,
    ) { phase, elapsed, captured -> Triple(phase, elapsed, captured) }

    private val activeClientIdFlow = combine(
        RecorderState.activeClientId,
        container.repository.observeLatest(),
    ) { runtimeId, persisted ->
        runtimeId ?: persisted?.clientRecordingId
    }.distinctUntilChanged()

    private val activeRecordingFlow = activeClientIdFlow.flatMapLatest { id ->
        if (id == null) flowOf(null) else container.repository.observeRecording(id)
    }

    private val statusPollTarget = combine(
        RecorderState.phase,
        activeRecordingFlow,
    ) { phase, recording ->
        recording?.clientRecordingId?.takeIf {
            phase == CapturePhase.IDLE &&
                recording.recordingId != null &&
                recording.localState !in TERMINAL_LOCAL_STATES &&
                recording.serverState !in TERMINAL_SERVER_STATES
        }
    }.distinctUntilChanged()

    val uiState: StateFlow<HomeUiState> = combine(
        container.warmup.state,
        container.authManager.state,
        captureInfo,
        activeRecordingFlow,
        container.repository.observeAllPendingChunks(),
    ) { warmup, auth, capture, active, pending ->
        HomeUiState(
            warmup = warmup,
            auth = auth,
            phase = capture.first,
            elapsedMs = if (capture.first == CapturePhase.IDLE) {
                active?.stoppedAtEpochMs
                    ?.minus(active.startedAtEpochMs)
                    ?.coerceAtLeast(0L)
                    ?: capture.second
            } else {
                capture.second
            },
            capturedChunks = capture.third,
            pendingChunks = pending,
            activeRecording = active,
        )
    }.combine(retryErrorState) { state, retryError -> state.copy(retryError = retryError) }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5000), HomeUiState())

    private val _history = MutableStateFlow(HistoryUiState())
    val history: StateFlow<HistoryUiState> = _history.asStateFlow()

    init {
        if (statusPollingEnabled) {
            viewModelScope.launch {
                statusPollTarget.collectLatest { clientId ->
                    if (clientId == null) return@collectLatest
                    while (currentCoroutineContext().isActive) {
                        if (refreshRecordingStatusNow(clientId)) break
                        delay(statusPollIntervalMs)
                    }
                }
            }
        }
    }

    // -------------------------------------------------------------- recording
    fun startRecording(context: Context) = RecordingService.start(context)
    fun stopRecording(context: Context) = RecordingService.stop(context)
    fun cancelRecording(context: Context) = RecordingService.cancel(context)

    // ---------------------------------------------------------------- warmup
    fun refreshWarmup() = viewModelScope.launch { container.warmup.probe() }

    // ------------------------------------------------------------------ auth
    fun trySilentSignIn(context: Context) = viewModelScope.launch {
        container.authManager.trySilentSignIn(context)
    }

    fun signIn(activityContext: Context) = viewModelScope.launch {
        container.authManager.explicitSignIn(activityContext)
    }

    fun signOut() = viewModelScope.launch { container.authManager.signOut() }

    val isAuthConfigured: Boolean get() = container.authManager.isConfigured

    // -------------------------------------------------------------- settings
    fun saveBackendUrl(url: String) {
        container.settings.backendUrl = url
        backendUrlState.value = container.settings.backendUrl
        refreshWarmup()
    }

    // --------------------------------------------------------- status polling
    fun refreshRecordingStatus(clientId: String) = viewModelScope.launch {
        refreshRecordingStatusNow(clientId)
    }

    /** @return true once the backend reports a terminal state, so no extra poll is sent. */
    private suspend fun refreshRecordingStatusNow(clientId: String): Boolean = withContext(Dispatchers.IO) {
        // Always go through the token provider: a cached-but-expired ID token is never sent.
        val token = idToken() ?: return@withContext false
        val recordingId = container.repository.recordingIdFor(clientId) ?: return@withContext false
        when (val res = container.api.getRecording(token.idToken, recordingId)) {
            is ApiResult.Success -> {
                val state = ServerRecordingState.fromWire(res.body.state).name
                container.repository.updateServerState(
                    clientId,
                    state,
                    res.body.transcriptId,
                    res.body.failureReason,
                )
                state in TERMINAL_SERVER_STATES
            }
            else -> false
        }
    }

    /**
     * User-initiated retry of a failed upload.
     *
     * Delegates to [com.tomaskubica.voiceprompt.data.RecordingRetryCoordinator] so the durable
     * state is reset *before* the work chain is rebuilt — otherwise the recording stays
     * `FAILED` and `complete` refuses forever while the retried chunk uploads delete the WAVs.
     */
    fun retryRecording(clientId: String) = viewModelScope.launch {
        retryErrorState.value = null
        when (val outcome = container.retryCoordinator.retry(clientId)) {
            RetryOutcome.Scheduled -> Unit
            RetryOutcome.NotRetryable -> Unit
            is RetryOutcome.Failed -> retryErrorState.value = outcome.reason
        }
    }

    // --------------------------------------------------------------- history
    fun loadHistory() = viewModelScope.launch {
        _history.value = _history.value.copy(loading = true, error = null)
        val token = idToken()
        if (token == null) {
            _history.value = HistoryUiState(loading = false, error = "auth")
            return@launch
        }
        when (val res = container.api.listTranscripts(token.idToken, cursor = null, limit = 50)) {
            is ApiResult.Success ->
                _history.value = HistoryUiState(loading = false, items = res.body.items)
            is ApiResult.Failure ->
                _history.value = HistoryUiState(loading = false, error = "http_${res.code}")
            is ApiResult.NetworkError ->
                _history.value = HistoryUiState(loading = false, error = "network")
        }
    }

    fun copyTranscript(context: Context, transcriptId: String, onDone: (Boolean) -> Unit) =
        viewModelScope.launch {
            val token = idToken()
            if (token == null) { onDone(false); return@launch }
            when (val res = container.api.getTranscript(token.idToken, transcriptId)) {
                is ApiResult.Success -> {
                    val clip = context.getSystemService(ClipboardManager::class.java)
                    clip.setPrimaryClip(ClipData.newPlainText("transcript", res.body.body))
                    onDone(true)
                }
                else -> onDone(false)
            }
        }

    /**
     * A currently valid ID token, or null when the user must sign in again (or sign-in is not
     * configured). Never returns a stale token.
     */
    private suspend fun idToken(): StoredToken? = when (val result = container.authTokens.idToken()) {
        is TokenResult.Valid -> result.token
        TokenResult.AuthNeeded, TokenResult.Unavailable -> null
    }

    private companion object {
        const val STATUS_POLL_INTERVAL_MS = 4_000L
        val TERMINAL_SERVER_STATES = setOf(
            ServerRecordingState.COMPLETED.name,
            ServerRecordingState.FAILED.name,
        )
        val TERMINAL_LOCAL_STATES = setOf(
            LocalRecordingState.COMPLETED.name,
            LocalRecordingState.FAILED.name,
        )
    }
}

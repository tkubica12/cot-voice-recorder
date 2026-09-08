package com.tomaskubica.voiceprompt.work

import android.content.Context
import androidx.work.CoroutineWorker
import androidx.work.WorkerParameters
import com.tomaskubica.voiceprompt.AppContainer
import com.tomaskubica.voiceprompt.VoicePromptApp
import com.tomaskubica.voiceprompt.auth.AuthState

/** Shared base: resolves the container-backed [UploadOrchestrator] and maps outcomes to Results. */
abstract class BaseUploadWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {

    protected val container: AppContainer
        get() = (applicationContext as VoicePromptApp).container

    protected val orchestrator: UploadOrchestrator
        get() = UploadOrchestrator(
            repository = container.repository,
            api = container.api,
            tokens = container.authTokens,
            appVersion = com.tomaskubica.voiceprompt.BuildConfig.VERSION_NAME,
        )

    protected fun clientId(): String? = inputData.getString(UploadScheduler.KEY_CLIENT_ID)

    /**
     * Auth failures stay retryable so nothing captured is thrown away, but they are also
     * surfaced as a notification: the user is told to open the app and sign in instead of
     * watching an opaque, endless retry. Only success with a valid sign-in clears the warning.
     */
    protected fun StepOutcome.toResult(): Result {
        val notifier = container.authNotifier
        return when (this) {
            StepOutcome.SUCCESS -> {
                container.authManager.refreshState()
                if (container.authManager.state.value is AuthState.SignedIn) notifier.clear()
                Result.success()
            }
            StepOutcome.RETRY -> Result.retry()
            StepOutcome.AUTH_REQUIRED -> {
                notifier.signInRequired()
                Result.retry()
            }
            StepOutcome.FAILURE -> Result.failure()
        }
    }
}

/** Idempotently creates the backend recording session. */
class CreateSessionWorker(context: Context, params: WorkerParameters) :
    BaseUploadWorker(context, params) {
    override suspend fun doWork(): Result {
        val clientId = clientId() ?: return Result.failure()
        return orchestrator.createSession(clientId).toResult()
    }
}

/** Uploads one WAV chunk idempotently; deletes the local WAV only after acknowledgement. */
class UploadChunkWorker(context: Context, params: WorkerParameters) :
    BaseUploadWorker(context, params) {
    override suspend fun doWork(): Result {
        val clientId = clientId() ?: return Result.failure()
        val index = inputData.getInt(UploadScheduler.KEY_INDEX, -1)
        if (index < 0) return Result.failure()
        return orchestrator.uploadChunk(clientId, index).toResult()
    }
}

/** Sends `complete` only after all declared chunks are acknowledged. */
class CompleteWorker(context: Context, params: WorkerParameters) :
    BaseUploadWorker(context, params) {
    override suspend fun doWork(): Result {
        val clientId = clientId() ?: return Result.failure()
        return orchestrator.complete(clientId).toResult()
    }
}

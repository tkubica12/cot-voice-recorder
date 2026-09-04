package com.tomaskubica.voiceprompt.data.api

import com.tomaskubica.voiceprompt.data.model.ErrorClassifier
import com.tomaskubica.voiceprompt.data.model.Outcome

/**
 * Result of an API call carrying enough detail to classify retry vs. terminal behaviour and to
 * surface RFC 9457 Problem Details.
 */
sealed interface ApiResult<out T> {
    data class Success<T>(val code: Int, val body: T) : ApiResult<T>
    data class Failure(val code: Int, val problem: ProblemDto?) : ApiResult<Nothing>
    data class NetworkError(val cause: Throwable) : ApiResult<Nothing>

    val outcome: Outcome
        get() = when (this) {
            is Success -> Outcome.SUCCESS
            is Failure -> ErrorClassifier.classifyHttp(code)
            is NetworkError -> ErrorClassifier.networkFailure()
        }
}

/** Convenience extractor for the success body or null. */
fun <T> ApiResult<T>.bodyOrNull(): T? = (this as? ApiResult.Success)?.body

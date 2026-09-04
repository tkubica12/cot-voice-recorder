package com.tomaskubica.voiceprompt.data.model

/**
 * Classification of an API/network outcome that drives retry vs. terminal handling.
 *
 * Contract mapping (see openapi/voice-recorder.yaml + docs/architecture.md):
 *  - 401                       -> AUTH_NEEDED  (token missing/expired/invalid audience)
 *  - 409                       -> TERMINAL_CONFLICT (digest/complete conflict = corruption)
 *  - 422                       -> TERMINAL_VALIDATION
 *  - 400 / 403 / 404 / 413 / 415 -> TERMINAL_OTHER (client-side, not fixable by retrying)
 *  - 429 / 5xx / network / timeout -> RETRYABLE
 */
enum class Outcome {
    SUCCESS,
    AUTH_NEEDED,
    TERMINAL_CONFLICT,
    TERMINAL_VALIDATION,
    TERMINAL_OTHER,
    RETRYABLE,
    ;

    val isTerminal: Boolean
        get() = this == TERMINAL_CONFLICT || this == TERMINAL_VALIDATION || this == TERMINAL_OTHER
}

object ErrorClassifier {
    /** Classify an HTTP status code (used for real responses). */
    fun classifyHttp(status: Int): Outcome = when {
        status in 200..299 -> Outcome.SUCCESS
        status == 401 -> Outcome.AUTH_NEEDED
        status == 409 -> Outcome.TERMINAL_CONFLICT
        status == 422 -> Outcome.TERMINAL_VALIDATION
        status == 429 -> Outcome.RETRYABLE
        status in 500..599 -> Outcome.RETRYABLE
        status in 400..499 -> Outcome.TERMINAL_OTHER // 400/403/404/413/415 etc.
        else -> Outcome.RETRYABLE
    }

    /** Network failures (no HTTP status) and timeouts are always retryable. */
    fun networkFailure(): Outcome = Outcome.RETRYABLE
}

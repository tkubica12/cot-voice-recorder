package com.tomaskubica.voiceprompt.data.model

import com.google.common.truth.Truth.assertThat
import org.junit.Test

class ErrorClassifierTest {

    @Test fun success_range() {
        assertThat(ErrorClassifier.classifyHttp(200)).isEqualTo(Outcome.SUCCESS)
        assertThat(ErrorClassifier.classifyHttp(202)).isEqualTo(Outcome.SUCCESS)
    }

    @Test fun auth_needed_is_401() {
        assertThat(ErrorClassifier.classifyHttp(401)).isEqualTo(Outcome.AUTH_NEEDED)
    }

    @Test fun conflict_is_terminal() {
        assertThat(ErrorClassifier.classifyHttp(409)).isEqualTo(Outcome.TERMINAL_CONFLICT)
    }

    @Test fun validation_is_terminal() {
        assertThat(ErrorClassifier.classifyHttp(422)).isEqualTo(Outcome.TERMINAL_VALIDATION)
    }

    @Test fun other_client_errors_terminal() {
        for (code in intArrayOf(400, 403, 404, 413, 415)) {
            assertThat(ErrorClassifier.classifyHttp(code)).isEqualTo(Outcome.TERMINAL_OTHER)
        }
    }

    @Test fun rate_limit_and_server_errors_retryable() {
        for (code in intArrayOf(429, 500, 502, 503, 504)) {
            assertThat(ErrorClassifier.classifyHttp(code)).isEqualTo(Outcome.RETRYABLE)
        }
    }

    @Test fun network_failure_is_retryable() {
        assertThat(ErrorClassifier.networkFailure()).isEqualTo(Outcome.RETRYABLE)
    }

    @Test fun terminal_flag() {
        assertThat(Outcome.TERMINAL_CONFLICT.isTerminal).isTrue()
        assertThat(Outcome.RETRYABLE.isTerminal).isFalse()
        assertThat(Outcome.AUTH_NEEDED.isTerminal).isFalse()
    }
}

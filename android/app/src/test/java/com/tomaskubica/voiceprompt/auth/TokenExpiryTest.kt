package com.tomaskubica.voiceprompt.auth

import com.google.common.truth.Truth.assertThat
import org.junit.Test

class TokenExpiryTest {

    @Test fun valid_when_expiry_beyond_now_plus_skew() {
        assertThat(TokenExpiry.isValid(expiresAtEpochMs = 1_000, nowMs = 0, skewMs = 100)).isTrue()
    }

    @Test fun expired_within_skew_window() {
        // 1000 - 100 = 900 <= 950 -> treated as expired early.
        assertThat(TokenExpiry.isValid(expiresAtEpochMs = 1_000, nowMs = 950, skewMs = 100)).isFalse()
        assertThat(TokenExpiry.isExpired(expiresAtEpochMs = 1_000, nowMs = 950, skewMs = 100)).isTrue()
    }

    @Test fun expired_when_past() {
        assertThat(TokenExpiry.isExpired(expiresAtEpochMs = 1_000, nowMs = 2_000, skewMs = 0)).isTrue()
    }
}

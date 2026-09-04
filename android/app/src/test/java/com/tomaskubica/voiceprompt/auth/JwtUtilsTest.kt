package com.tomaskubica.voiceprompt.auth

import com.google.common.truth.Truth.assertThat
import java.util.Base64
import org.junit.Test

class JwtUtilsTest {

    private fun jwt(payloadJson: String): String {
        val enc = Base64.getUrlEncoder().withoutPadding()
        val header = enc.encodeToString("""{"alg":"RS256","typ":"JWT"}""".toByteArray())
        val payload = enc.encodeToString(payloadJson.toByteArray())
        return "$header.$payload.sig"
    }

    @Test fun reads_exp_in_millis() {
        val token = jwt("""{"exp":1700000000,"email":"a@b.com"}""")
        assertThat(JwtUtils.expiryEpochMs(token)).isEqualTo(1_700_000_000_000L)
    }

    @Test fun reads_email() {
        val token = jwt("""{"exp":1700000000,"email":"owner@example.com","email_verified":true}""")
        assertThat(JwtUtils.email(token)).isEqualTo("owner@example.com")
    }

    @Test fun returns_null_for_garbage() {
        assertThat(JwtUtils.expiryEpochMs("not-a-jwt")).isNull()
        assertThat(JwtUtils.email("still.not.valid")).isNull()
    }
}

package com.tomaskubica.voiceprompt.audio

import com.google.common.truth.Truth.assertThat
import org.junit.Test

class ContentDigestTest {

    @Test fun known_vector_for_abc() {
        // SHA-256("abc") base64 is a well-known value.
        val header = ContentDigest.contentDigestHeader("abc".toByteArray())
        assertThat(header).isEqualTo("sha-256=:ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=:")
    }

    @Test fun format_has_prefix_and_wrapping_colons() {
        val header = ContentDigest.contentDigestHeader(ByteArray(10))
        assertThat(header).startsWith("sha-256=:")
        assertThat(header).endsWith(":")
    }

    @Test fun same_bytes_same_digest_different_bytes_differ() {
        val a = ContentDigest.contentDigestHeader(byteArrayOf(1, 2, 3))
        val b = ContentDigest.contentDigestHeader(byteArrayOf(1, 2, 3))
        val c = ContentDigest.contentDigestHeader(byteArrayOf(1, 2, 4))
        assertThat(a).isEqualTo(b)
        assertThat(a).isNotEqualTo(c)
    }
}

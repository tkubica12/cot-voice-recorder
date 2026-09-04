package com.tomaskubica.voiceprompt.audio

import java.security.MessageDigest
import java.util.Base64

/**
 * RFC 9530 Content-Digest formatting used as the chunk upload idempotency checksum.
 *
 * Format: `sha-256=:<base64(sha-256(bytes))>:`
 *
 * Uses [java.util.Base64] so it is pure-JVM unit testable without Android/Robolectric.
 */
object ContentDigest {
    fun sha256(bytes: ByteArray): ByteArray =
        MessageDigest.getInstance("SHA-256").digest(bytes)

    /** Standard base64 (with padding) of the raw digest. */
    fun base64(digest: ByteArray): String =
        Base64.getEncoder().encodeToString(digest)

    /** Full `sha-256=:<base64>:` header value for the given content. */
    fun contentDigestHeader(bytes: ByteArray): String {
        val b64 = base64(sha256(bytes))
        return "sha-256=:$b64:"
    }
}

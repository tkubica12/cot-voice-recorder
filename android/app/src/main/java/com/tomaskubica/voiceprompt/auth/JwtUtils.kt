package com.tomaskubica.voiceprompt.auth

import java.util.Base64

/**
 * Minimal, dependency-free reader of the non-sensitive claims of a Google ID token (a JWT).
 *
 * Only used to learn the token's expiry (`exp`) and account (`email`) so uploads never use a
 * stale token. Signature validation is the backend's responsibility; this never trusts the token
 * for auth decisions beyond expiry bookkeeping.
 */
object JwtUtils {
    private val expRegex = Regex("\"exp\"\\s*:\\s*(\\d+)")
    private val emailRegex = Regex("\"email\"\\s*:\\s*\"([^\"]+)\"")

    fun decodePayloadJson(jwt: String): String? {
        val parts = jwt.split('.')
        if (parts.size < 2) return null
        return try {
            String(Base64.getUrlDecoder().decode(padded(parts[1])), Charsets.UTF_8)
        } catch (_: Exception) {
            null
        }
    }

    /** Expiry in epoch milliseconds, or null if not present/parseable. */
    fun expiryEpochMs(jwt: String): Long? {
        val json = decodePayloadJson(jwt) ?: return null
        val secs = expRegex.find(json)?.groupValues?.get(1)?.toLongOrNull() ?: return null
        return secs * 1000L
    }

    fun email(jwt: String): String? {
        val json = decodePayloadJson(jwt) ?: return null
        return emailRegex.find(json)?.groupValues?.get(1)
    }

    private fun padded(s: String): String {
        val rem = s.length % 4
        return if (rem == 0) s else s + "=".repeat(4 - rem)
    }
}

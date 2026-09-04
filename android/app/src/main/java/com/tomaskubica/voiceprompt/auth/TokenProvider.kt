package com.tomaskubica.voiceprompt.auth

/** Snapshot of the stored Google ID token. */
data class StoredToken(
    val idToken: String,
    val email: String?,
    val expiresAtEpochMs: Long,
)

/**
 * Abstraction over the encrypted token store so WorkManager uploads and tests can obtain the
 * current ID token without depending on the Android Keystore directly.
 */
interface TokenProvider {
    /** Returns a non-expired token, or null when absent/expired. */
    fun currentValidToken(skewMs: Long = DEFAULT_SKEW_MS): StoredToken?

    /** Raw stored token regardless of expiry (for diagnostics/UI). */
    fun peek(): StoredToken?

    fun save(token: StoredToken)

    fun clear()

    fun isExpired(skewMs: Long = DEFAULT_SKEW_MS): Boolean

    companion object {
        const val DEFAULT_SKEW_MS = 60_000L // treat as expired 60 s early
    }
}

package com.tomaskubica.voiceprompt.auth

/** Pure expiry decision so token validity is unit-testable without Android. */
object TokenExpiry {
    fun isValid(expiresAtEpochMs: Long, nowMs: Long, skewMs: Long): Boolean =
        expiresAtEpochMs - skewMs > nowMs

    fun isExpired(expiresAtEpochMs: Long, nowMs: Long, skewMs: Long): Boolean =
        !isValid(expiresAtEpochMs, nowMs, skewMs)
}

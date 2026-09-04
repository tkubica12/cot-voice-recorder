package com.tomaskubica.voiceprompt.auth

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * Android Keystore–backed encrypted implementation of [TokenProvider].
 *
 * The ID token, its expiry and the account email are held in [EncryptedSharedPreferences] whose
 * master key lives in the AndroidKeyStore (AES-256-GCM). Expiry is checked so ongoing uploads
 * never silently use a stale token.
 */
class EncryptedTokenStore private constructor(
    private val prefs: SharedPreferences,
    private val clock: () -> Long,
) : TokenProvider {

    override fun currentValidToken(skewMs: Long): StoredToken? {
        val token = peek() ?: return null
        return if (TokenExpiry.isValid(token.expiresAtEpochMs, clock(), skewMs)) token else null
    }

    override fun peek(): StoredToken? {
        val idToken = prefs.getString(KEY_TOKEN, null) ?: return null
        val exp = prefs.getLong(KEY_EXP, 0L)
        val email = prefs.getString(KEY_EMAIL, null)
        return StoredToken(idToken, email, exp)
    }

    override fun save(token: StoredToken) {
        prefs.edit()
            .putString(KEY_TOKEN, token.idToken)
            .putLong(KEY_EXP, token.expiresAtEpochMs)
            .putString(KEY_EMAIL, token.email)
            .apply()
    }

    override fun clear() {
        prefs.edit().clear().apply()
    }

    override fun isExpired(skewMs: Long): Boolean {
        val token = peek() ?: return true
        return TokenExpiry.isExpired(token.expiresAtEpochMs, clock(), skewMs)
    }

    companion object {
        private const val FILE = "vp_secure_token"
        private const val KEY_TOKEN = "id_token"
        private const val KEY_EXP = "expires_at"
        private const val KEY_EMAIL = "email"

        fun create(context: Context, clock: () -> Long = System::currentTimeMillis): EncryptedTokenStore {
            val master = MasterKey.Builder(context)
                .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                .build()
            val prefs = EncryptedSharedPreferences.create(
                context,
                FILE,
                master,
                EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
            )
            return EncryptedTokenStore(prefs, clock)
        }
    }
}

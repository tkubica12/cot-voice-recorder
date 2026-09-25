package com.tomaskubica.voiceprompt.data

import android.content.Context
import com.tomaskubica.voiceprompt.BuildConfig
import com.tomaskubica.voiceprompt.data.model.RefineModel

/**
 * Small editable settings surface: the backend base URL (defaults to the deployed release URL
 * but stays user-editable) and the refine model.
 */
class SettingsStore(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences("vp_settings", Context.MODE_PRIVATE)

    init {
        if (prefs.getString(KEY_REFINE, null) == RefineModel.LEGACY_LUNA.wire) {
            prefs.edit().putString(KEY_REFINE, RefineModel.DEFAULT.wire).apply()
        }
    }

    var backendUrl: String
        get() = prefs.getString(KEY_BACKEND, null)?.takeIf { it.isNotBlank() }
            ?: BuildConfig.DEFAULT_BACKEND_URL
        set(value) {
            prefs.edit().putString(KEY_BACKEND, value.trim()).apply()
        }

    var refineModel: String
        get() = prefs.getString(KEY_REFINE, null) ?: RefineModel.DEFAULT.wire
        set(value) { prefs.edit().putString(KEY_REFINE, value).apply() }

    var quickRecordNotification: Boolean
        get() = prefs.getBoolean("quick_record_notification", false)
        set(value) { prefs.edit().putBoolean("quick_record_notification", value).apply() }

    companion object {
        private const val KEY_BACKEND = "backend_url"
        private const val KEY_REFINE = "refine_model"
    }
}

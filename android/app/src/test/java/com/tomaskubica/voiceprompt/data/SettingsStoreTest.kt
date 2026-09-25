package com.tomaskubica.voiceprompt.data

import android.app.Application
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = Application::class)
class SettingsStoreTest {
    private val app: Application = ApplicationProvider.getApplicationContext()
    private val prefs = app.getSharedPreferences("vp_settings", Context.MODE_PRIVATE)

    @Before fun clearSettings() {
        prefs.edit().clear().commit()
    }

    @Test fun new_install_uses_gpt_6_luna() {
        assertThat(SettingsStore(app).refineModel).isEqualTo("gpt-6-luna")
    }

    @Test fun upgrades_saved_legacy_default_for_future_recordings() {
        prefs.edit().putString("refine_model", "gpt-5.6-luna").commit()

        assertThat(SettingsStore(app).refineModel).isEqualTo("gpt-6-luna")
        assertThat(prefs.getString("refine_model", null)).isEqualTo("gpt-6-luna")
    }

    @Test fun preserves_other_saved_model() {
        prefs.edit().putString("refine_model", "gpt-5.6-terra").commit()

        assertThat(SettingsStore(app).refineModel).isEqualTo("gpt-5.6-terra")
    }
}

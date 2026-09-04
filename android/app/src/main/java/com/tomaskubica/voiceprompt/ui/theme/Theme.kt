package com.tomaskubica.voiceprompt.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable

/**
 * Simple, fast monochrome black/white/gray theme with a single orange accent, matching
 * assets/voice-cloud.svg. The same identity is used in dark and light modes.
 */
private val LightColors = lightColorScheme(
    primary = Orange,
    onPrimary = White,
    secondary = Gray,
    onSecondary = White,
    background = White,
    onBackground = Ink,
    surface = White,
    onSurface = Ink,
    surfaceVariant = Light,
    onSurfaceVariant = Ink,
    error = Orange,
    outline = Gray,
)

private val DarkColors = darkColorScheme(
    primary = Orange,
    onPrimary = Ink,
    secondary = Gray,
    onSecondary = Ink,
    background = DarkBg,
    onBackground = White,
    surface = DarkSurface,
    onSurface = White,
    surfaceVariant = Ink,
    onSurfaceVariant = Light,
    error = Orange,
    outline = Gray,
)

@Composable
fun VoicePromptTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    MaterialTheme(
        colorScheme = if (darkTheme) DarkColors else LightColors,
        typography = Typography(),
        content = content,
    )
}

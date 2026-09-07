package com.tomaskubica.voiceprompt.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.Switch
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.tomaskubica.voiceprompt.auth.AuthState
import com.tomaskubica.voiceprompt.R

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    backendUrl: String,
    auth: AuthState,
    authConfigured: Boolean,
    onSaveBackendUrl: (String) -> Unit,
    onSignIn: () -> Unit,
    onSignOut: () -> Unit,
    onBack: () -> Unit,
    quickRecordNotification: Boolean = false,
    onQuickRecordNotificationChange: (Boolean) -> Unit = {},
) {
    var url by remember(backendUrl) { mutableStateOf(backendUrl) }
    var quickNotification by remember(quickRecordNotification) { mutableStateOf(quickRecordNotification) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Settings") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text("Backend", style = MaterialTheme.typography.titleMedium)
            OutlinedTextField(
                value = url,
                onValueChange = { url = it },
                label = { Text("Backend URL") },
                singleLine = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag("backendUrlField"),
            )
            Button(
                onClick = { onSaveBackendUrl(url) },
                modifier = Modifier.testTag("saveBackendUrl"),
            ) { Text("Save") }

            Text("Account", style = MaterialTheme.typography.titleMedium)
            when {
                !authConfigured -> Text(
                    "Google sign-in is not configured in this build. Recording works offline; " +
                        "uploads will start once a Web client ID is provided.",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                auth is AuthState.SignedIn -> {
                    Text("Signed in${auth.email?.let { " as $it" } ?: ""}")
                    OutlinedButton(onClick = onSignOut, modifier = Modifier.testTag("signOut")) {
                        Text("Sign out")
                    }
                }
                else -> {
                    Text("Signed out")
                    Button(onClick = onSignIn, modifier = Modifier.testTag("signIn")) {
                        Text("Sign in with Google")
                    }
                }
            }
            Text(stringResource(R.string.quick_record), style = MaterialTheme.typography.titleMedium)
            Text(stringResource(R.string.quick_settings_hint))
            Switch(
                checked = quickNotification,
                onCheckedChange = {
                    quickNotification = it
                    onQuickRecordNotificationChange(it)
                },
                modifier = Modifier.testTag("quickRecordNotification"),
            )
        }
    }
}

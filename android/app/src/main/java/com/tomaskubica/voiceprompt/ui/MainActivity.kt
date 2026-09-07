package com.tomaskubica.voiceprompt.ui

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import com.tomaskubica.voiceprompt.data.SettingsStore
import com.tomaskubica.voiceprompt.ui.screens.HistoryScreen
import com.tomaskubica.voiceprompt.ui.screens.HomeScreen
import com.tomaskubica.voiceprompt.ui.screens.SettingsScreen
import com.tomaskubica.voiceprompt.ui.theme.VoicePromptTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        QuickRecordEntryPoints.registerShortcut(this)
        setContent {
            VoicePromptTheme {
                Surface(color = MaterialTheme.colorScheme.background) {
                    AppRoot()
                }
            }
        }
    }

    override fun onResume() {
        super.onResume()
        QuickRecordEntryPoints.refreshNotification(this)
    }
}

@Composable
private fun AppRoot(vm: RecorderViewModel = viewModel()) {
    val context = LocalContext.current
    val navController = rememberNavController()
    val ui by vm.uiState.collectAsStateWithLifecycle()
    val backendUrl by vm.backendUrl.collectAsState()
    val historyState by vm.history.collectAsState()

    var showPermanentDenied by remember { mutableStateOf(false) }
    var pendingStart by remember { mutableStateOf(false) }

    fun hasMicPermission(): Boolean =
        ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
            PackageManager.PERMISSION_GRANTED

    val micPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted && pendingStart) {
            vm.startRecording(context)
        } else if (!granted) {
            val activity = context as? ComponentActivity
            val canAsk = activity?.shouldShowRequestPermissionRationale(Manifest.permission.RECORD_AUDIO) ?: true
            if (!canAsk) showPermanentDenied = true
        }
        pendingStart = false
    }

    val notificationsLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { QuickRecordEntryPoints.refreshNotification(context) }

    // Ask for notifications once (Android 13+), restore auth, and refresh warmup on entry.
    LaunchedEffect(Unit) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            notificationsLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        vm.trySilentSignIn(context)
        vm.refreshWarmup()
    }

    fun ensureStart() {
        if (hasMicPermission()) {
            vm.startRecording(context)
        } else {
            pendingStart = true
            micPermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
        }
    }

    NavHost(navController = navController, startDestination = "home") {
        composable("home") {
            HomeScreen(
                state = ui,
                onHoldStart = { ensureStart() },
                onHoldRelease = { vm.stopRecording(context) },
                onHoldCancel = { vm.cancelRecording(context) },
                onToggle = {
                    if (ui.isRecording) vm.stopRecording(context) else ensureStart()
                },
                onRetry = { vm.retryRecording(it) },
                onOpenSettings = { navController.navigate("settings") },
                onOpenHistory = { navController.navigate("history") },
            )
        }
        composable("settings") {
            SettingsScreen(
                backendUrl = backendUrl,
                auth = ui.auth,
                authConfigured = vm.isAuthConfigured,
                onSaveBackendUrl = { vm.saveBackendUrl(it) },
                onSignIn = { vm.signIn(context) },
                onSignOut = { vm.signOut() },
                onBack = { navController.popBackStack() },
                quickRecordNotification = SettingsStore(context).quickRecordNotification,
                onQuickRecordNotificationChange = { enabled ->
                    SettingsStore(context).quickRecordNotification = enabled
                    if (enabled && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                        ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
                        PackageManager.PERMISSION_GRANTED
                    ) {
                        notificationsLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
                    }
                    QuickRecordEntryPoints.refreshNotification(context)
                },
            )
        }
        composable("history") {
            val ctx = LocalContext.current
            HistoryScreen(
                state = historyState,
                onLoad = { vm.loadHistory() },
                onCopy = { id -> vm.copyTranscript(ctx, id) { } },
                onBack = { navController.popBackStack() },
            )
        }
    }

    if (showPermanentDenied) {
        AlertDialog(
            onDismissRequest = { showPermanentDenied = false },
            title = { Text("Microphone access needed") },
            text = { Text("Microphone permission is permanently denied. Enable it in system settings to record.") },
            confirmButton = {
                TextButton(onClick = {
                    showPermanentDenied = false
                    val intent = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
                        data = Uri.fromParts("package", context.packageName, null)
                    }
                    context.startActivity(intent)
                }) { Text("Open settings") }
            },
            dismissButton = {
                TextButton(onClick = { showPermanentDenied = false }) { Text("Cancel") }
            },
        )
    }
}

# android/ — VoicePrompt Android client

**Owner:** Android component.
**Tech:** Kotlin · Jetpack Compose · Material 3 · foreground service · Room · WorkManager.

An extremely fast capture client with a **two-control** UI (**hold-to-talk** and **toggle
start/stop**) that records 16 kHz mono PCM WAV in a foreground service, chunks it into exact
30.0-second windows with 1.5-second overlap, and uploads chunks through a durable, idempotent,
retrying WorkManager chain. Startup is instant and never waits on the backend.

- **Package / application ID:** `com.tomaskubica.voiceprompt` (fixed).
- **minSdk 29**, **targetSdk / compileSdk 35**.
- **Contract:** talks to the backend exactly per [`../openapi/voice-recorder.yaml`](../openapi/voice-recorder.yaml)
  and follows [`../docs/architecture.md`](../docs/architecture.md) (cold-start, idempotency,
  ack-then-delete). App identity/colors reuse [`../assets/voice-cloud.svg`](../assets/voice-cloud.svg).
- New recordings in version 1.2.1 default to `gpt-6-luna` for the final whole-transcript
  cleanup after MAI transcription. Upgrading migrates the previous saved `gpt-5.6-luna`
  default, but recordings already captured keep their original model for retries.
  Earlier APKs explicitly request `gpt-5.6-luna` until updated.

## Toolchain versions

| Tool | Version |
|------|---------|
| JDK | 17 |
| Gradle | 8.11.1 (wrapper) |
| Android Gradle Plugin | 8.7.3 |
| Kotlin | 2.0.21 (+ Compose compiler plugin) |
| KSP | 2.0.21-1.0.28 |
| Compose BOM | 2024.10.01 |
| Room | 2.6.1 · WorkManager 2.9.1 · OkHttp 4.12 · Moshi 1.15.1 (codegen) |
| Credentials (Sign in with Google) | 1.3.0 · googleid 1.1.1 |

## Prerequisites

- **JDK 17** on `PATH` (or `JAVA_HOME`).
- **Android SDK** with `platforms;android-35`, `build-tools;35.0.0`, `platform-tools`, and (for
  emulator tests) `emulator` + `system-images;android-35;google_apis;x86_64`.

Install the SDK non-interactively (example used on this machine):

```powershell
# 1) command-line tools -> %LOCALAPPDATA%\Android\Sdk\cmdline-tools\latest
# 2) accept licenses and install packages
$sdk = "$env:LOCALAPPDATA\Android\Sdk"
& "$sdk\cmdline-tools\latest\bin\sdkmanager.bat" --sdk_root="$sdk" --licenses          # accept all
& "$sdk\cmdline-tools\latest\bin\sdkmanager.bat" --sdk_root="$sdk" `
    "platform-tools" "platforms;android-35" "build-tools;35.0.0" `
    "emulator" "system-images;android-35;google_apis;x86_64"
```

Create `android/local.properties` (git-ignored) pointing at the SDK:

```
sdk.dir=C:\\Users\\<you>\\AppData\\Local\\Android\\Sdk
```

## Configuration

The build reads two configurable inputs; neither contains a committed secret.

| Input | Where | Default |
|-------|-------|---------|
| Backend base URL | `BuildConfig.DEFAULT_BACKEND_URL`, editable at runtime in **Settings** | `https://ca-api.ambitiousdesert-517ec9ed.swedencentral.azurecontainerapps.io` |
| Google **Web/server** OAuth client id | `-PGOOGLE_WEB_CLIENT_ID=…`, `GOOGLE_WEB_CLIENT_ID` env, or `googleWebClientId=` in `keystore.properties` | *(blank → sign-in reports "not configured")* |

When the Web client id is blank the app still **records and queues uploads locally**; sign-in is
safely reported as unavailable and no fake/placeholder token is ever minted. Uploads begin once a
real client id is provided and the user signs in.

```powershell
# Provide the Web client id for a build (never commit it):
./gradlew :app:assembleRelease -PGOOGLE_WEB_CLIENT_ID=xxxxxxxx.apps.googleusercontent.com
```

## Build

```powershell
cd android
./gradlew :app:assembleDebug       # debug APK (app-debug.apk, .debug applicationId suffix)
./gradlew :app:assembleRelease     # signed release APK (see Signing)
```

Release output (git-ignored, durable):
`android/app/build/outputs/apk/release/app-release.apk`.

## Test

```powershell
# Pure-JVM + Robolectric unit tests (chunking/overlap/remainder, WAV header, digest, state,
# retry/error classification, token expiry, cleanup preservation, API client + orchestrator via
# MockWebServer):
./gradlew :app:testDebugUnitTest

# Lint:
./gradlew :app:lintDebug        # or lintRelease

# Instrumented tests on a running emulator/device (Compose two-controls/state/accessibility, and
# a real foreground-service start/stop that captures a WAV/queue entry):
./gradlew :app:connectedDebugAndroidTest
```

### Emulator

```powershell
$sdk = "$env:LOCALAPPDATA\Android\Sdk"
& "$sdk\cmdline-tools\latest\bin\avdmanager.bat" create avd -n vp_test `
    -k "system-images;android-35;google_apis;x86_64" -d pixel_6
& "$sdk\emulator\emulator.exe" -avd vp_test -no-window -no-audio -no-boot-anim `
    -gpu swiftshader_indirect -no-snapshot
# wait for boot, then:
./gradlew :app:connectedDebugAndroidTest
```

The instrumented recording test uses the emulator's silent virtual microphone, so no live audio
input is required. Screen-lock continuation is not automated in CI (see *Manual verification*).

## Signing (sideload release)

The release build signs with a **stable local keystore kept outside version control**. It is
loaded from `android/keystore.properties` (git-ignored). If that file is absent (e.g. CI without
secrets) the release build deterministically falls back to the debug signing config so
`assembleRelease` still succeeds.

Create the keystore once and **back it up securely** — losing it means you cannot ship an update
that the same device will accept as the same app:

```powershell
$keytool = Join-Path $env:JAVA_HOME "bin\keytool.exe"
New-Item -ItemType Directory -Force android/keystore | Out-Null
& $keytool -genkeypair -v -keystore android/keystore/release.jks -alias voiceprompt `
    -keyalg RSA -keysize 4096 -validity 10000 `
    -dname "CN=VoicePrompt Sideload, OU=Personal, O=tomaskubica, C=CZ"
# then write android/keystore.properties (git-ignored):
#   storeFile=keystore/release.jks
#   storePassword=<your strong password>
#   keyAlias=voiceprompt
#   keyPassword=<your strong password>
#   googleWebClientId=
```

> **Backup requirement.** Copy `android/keystore/release.jks` **and** the password to a secure
> secret store (password manager / offline backup). Both are required to produce a compatible
> update; neither is recoverable if lost. The password is never printed by any build step.

Print the certificate fingerprints (non-secret; needed for Google OAuth Android client
registration):

```powershell
& $keytool -list -v -keystore android/keystore/release.jks -alias voiceprompt
```

## Sideload install

```powershell
$adb = "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe"
& $adb install -r android/app/build/outputs/apk/release/app-release.apk
```

## Google registration placeholders

The fixed package name and this repo's generated release fingerprints are recorded in
[`../docs/google-oauth.md`](../docs/google-oauth.md). Registration is **ready to create when the
parent asks** — do not create Google clients preemptively. The app expects a **Web/server** client
id as the ID-token audience (the backend allowlist); supply it via `GOOGLE_WEB_CLIENT_ID` as above.

## Manual verification

Two behaviours cannot run in headless CI and were verified manually / by instrumentation on a
booted emulator:

- **Foreground-service start/stop + capture** — verified by the instrumented
  `RecordingFlowInstrumentedTest` (starts the service via the toggle control, records, stops, and
  asserts a persisted chunk/queue entry).
- **Screen-lock continuation** — not automated. To verify manually: start a recording, lock the
  phone, wait > 30 s, unlock, stop; confirm multiple chunks were captured and the persistent
  notification remained. The partial wake lock + `microphone` foreground-service type keep capture
  running while locked.

## Quick recording and the lock screen

- Open the normal app once and grant microphone permission by starting a recording.
- Long-press the launcher icon for **Quick recording**. Compatible launchers can drag this
  shortcut onto the home screen. Mapping hardware buttons or gestures is device-specific.
- **Settings > Quick recording** enables an optional persistent start notification. Allow
  notifications (including its channel) on the lock screen in Android/HyperOS settings.
  Reopen the normal app to restore the notification after dismissal or a reboot.
- The exported `com.tomaskubica.voiceprompt.ui.QuickRecordActivity` accepts action
  `com.tomaskubica.voiceprompt.action.QUICK_RECORD` for automation tools. It starts capture
  once per explicit invocation, only after becoming visible. Repeated invocation while recording
  does not stop or duplicate capture. Opening the ongoing recording notification only shows controls.
- The separate quick-record task may appear above the keyguard and wake the display, but
  never unlocks the phone or exposes history, transcripts, authentication, or settings.
  It confirms microphone startup with a short vibration. Stop finishes and queues the recording;
  closing the screen or turning the display off does not stop capture.
- Android/HyperOS can still restrict launching activities from background gestures or require
  unlocking to interact with a notification. Lock-screen display flags do not bypass those rules.
  On Xiaomi, allow background operation / unrestricted battery and relevant lock-screen permissions.

Manual device scenarios: launch the shortcut with a secure keyguard; confirm capture and vibration;
turn the screen off for over 30 seconds; reopen controls and stop; confirm the complete recording.
Also check denied microphone permission, repeated shortcuts, rotation, and that private screens
remain behind the keyguard. Real locked-device behavior is not established by JVM tests.

## Architecture (app internals)

- **Capture** — `RecordingService` (foreground, type `microphone`) owns an `AudioRecord`
  (16 kHz mono PCM16) via `AudioRecordSource`/`PcmSource`, holds a partial wake lock only while
  recording, requests transient-exclusive audio focus, and shows a persistent notification with a
  **Stop** action. `StreamingChunker` + `ChunkPlan` implement the 30.0 s / 1.5 s-overlap windows
  and emit only genuinely new remainder audio at stop.
- **Durability** — every recording has a client UUID; `RecordingRepository` persists recording +
  chunk metadata and WAV files (Room) *before* upload. `UploadScheduler` builds one deterministic
  WorkManager chain per recording (`create → chunk 0 → … → complete`) with a network constraint and
  exponential backoff. `UploadOrchestrator` performs idempotent session create, idempotent chunk
  PUT with RFC 9530 `Content-Digest`, **ack-then-delete** (local WAV removed only after 200/202),
  and **complete-after-acks**. Error handling: 401 → auth-required (token invalidated, retry),
  409 → terminal corruption, 422 → terminal validation, 429/5xx/network/timeout → retry.
  Chunks and completion are appended with `APPEND_OR_REPLACE` (see `UploadWorkPolicy`) so a failed
  prerequisite cannot cascade and cancel later chunks, and each appended segment is prefixed with
  the idempotent create step so a rebuilt chain stays self-sufficient.
- **Capture failure never loses audio** — `CaptureFinalizer` decides what happens when capture
  ends, using the repository (not an in-memory counter) as the source of truth. Local data is
  deleted only on an explicit user cancel or when nothing was ever persisted; a mid-recording
  error keeps every durable WAV, schedules its upload, and declares the real chunk count.
- **Warmup** — `WarmupManager` fires an unauthenticated `/health/ready` probe asynchronously and
  surfaces cold-start / ready / offline; recording is always enabled regardless.
- **Auth** — `AuthManager` uses Credential Manager + Sign in with Google. Startup reuses a valid
  cached ID token without touching Credential Manager; otherwise it makes at most one foreground
  authorized-account restore attempt per process. Activity recreation cannot stack credential
  sheets. The token is stored in an Android Keystore-backed `EncryptedTokenStore`, and expiry is
  tracked so uploads never use a stale token. Background uploads never call Credential Manager
  because its provider may show a chooser even with auto-select enabled. An expired/rejected token
  keeps the step retryable and triggers a notification asking the user to open the app and sign in.
- **Expired sign-in recovery** — while the normal app is resumed, token validity is checked locally
  every second, without launching credential choosers. Pending uploads (including a completion
  request with no remaining WAVs) show an explicit sign-in prompt on Home; capture remains enabled.
  A successful foreground sign-in or first startup restore immediately rebuilds all non-terminal
  upload chains with fresh backoff and only unacknowledged chunks. Chain replacement is serialized
  with capture persistence and finalization, so signing in mid-recording cannot drop newly queued
  audio. Scheduling errors are visible and retryable. Auth and quick-record notifications have
  separate IDs; idempotent worker successes do not clear the auth warning while signed out.
  This does not introduce a background refresh-token session: if Google requires interaction,
  the user must still sign in from the foreground app.
- **History/cleanup** — history lists API transcripts (48 h retention); tapping a completed item
  copies it. `CleanupWorker` removes local metadata/WAVs older than 48 h **only when not pending
  upload**.
- **No audio content is ever logged.**

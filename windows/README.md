# windows/ — VoicePrompt Windows tray app

**Owner:** Windows component.
**Tech:** C# · .NET 8 · WPF (+ WinForms tray icon) · xUnit · Inno Setup.

A silent, always-running notification-area app. It holds a **direct Azure Web PubSub
connection** (the backend is never polled) and when a `transcript.completed` event arrives it
fetches the transcript, copies it to the clipboard, and shows a tray notification. A compact
settings/history window lists recent transcripts; selecting an old one copies it again.

- **Contract:** [`../openapi/voice-recorder.yaml`](../openapi/voice-recorder.yaml).
- **Behaviour:** [`../docs/architecture.md`](../docs/architecture.md).
- **OAuth setup:** [`../docs/google-oauth.md`](../docs/google-oauth.md).
- **Identity:** the microphone-cloud mark from [`../assets/voice-cloud.svg`](../assets/voice-cloud.svg),
  rendered to `src/VoicePrompt.App/Assets/app.ico` + `app.png`.

## Layout

| Path | Purpose |
|------|---------|
| `src/VoicePrompt.Core/` | Platform-neutral `net8.0` library: OAuth, API client, realtime, history, clipboard policy. |
| `src/VoicePrompt.App/` | `net8.0-windows` WPF tray app (x64): tray icon, settings window, DPAPI, registry auto-start, STA clipboard. |
| `tests/VoicePrompt.Core.Tests/` | xUnit unit + integration tests. |
| `tools/IconGen/` | One-shot renderer: SVG geometry → multi-resolution `app.ico` and `app.png`. |
| `installer/VoicePrompt.iss` | Inno Setup script producing `VoicePrompt-Setup.exe`. |
| `publish/win-x64/` | Framework-dependent publish output (git-ignored). |
| `installer/output/` | Built installer (git-ignored). |
| `installer/staging/` | Locally staged Desktop OAuth client JSON (git-ignored). |

## Prerequisites

- **.NET 8 SDK** (`global.json` pins 8.0.400 with `latestFeature` roll-forward).
- **.NET 8 Desktop Runtime (x64)** to *run* the app — the build is framework-dependent.
- **Inno Setup 6** to build the installer:
  `winget install --id JRSoftware.InnoSetup --exact`.

## Configure

The app needs the Google **Desktop** OAuth client JSON. It is sensitive operational config and
is **never committed** — it lives in the git-ignored `.secrets/` folder and is staged locally:

```powershell
# From the repo root. Classifies .secrets/*.json by shape and writes only ignored files.
./scripts/Configure-OAuth.ps1
# -> windows/installer/staging/google-desktop-client.json   (bundled by the installer)
# -> android/keystore.properties  googleWebClientId=...
# -> infra/main.parameters.local.json  (backend audiences + allowlisted email)
```

At runtime the client JSON is discovered in this order (first hit wins):

1. `%VOICEPROMPT_GOOGLE_DESKTOP_CLIENT%`
2. `%LOCALAPPDATA%\VoicePrompt\google-desktop-client.json`
3. `google-desktop-client.json` next to `VoicePrompt.exe` (installed by the setup)
4. `windows/installer/staging/google-desktop-client.json` (when running from the repo)

**If nothing is found the app still runs**: auth reports *Not configured*, sign-in is disabled,
the realtime loop stays idle, and no placeholder credentials are minted. This is exactly the
state CI builds and tests in.

Other settings live in `%LOCALAPPDATA%\VoicePrompt\settings.json` and are editable in the
window (backend URL, auto-start, pause notifications).

## Build

```powershell
cd windows
dotnet build VoicePrompt.sln -c Release
dotnet publish src/VoicePrompt.App/VoicePrompt.App.csproj `
    -c Release -r win-x64 --self-contained false -o publish/win-x64
```

Output: `windows/publish/win-x64/VoicePrompt.exe` (framework-dependent, x64).

Regenerate the icons after editing the SVG:

```powershell
dotnet run --project tools/IconGen -- src/VoicePrompt.App/Assets
```

## Test

```powershell
cd windows
dotnet test VoicePrompt.sln -c Release
```

Covers PKCE/state/authorization-URL construction, JWT expiry parsing, every refresh branch,
the DPAPI token store (through a reversible fake protector so it runs anywhere), RFC 9457
problem parsing and status classification, the 401-refresh-once rule, 403 being terminal,
429/5xx/network backoff, Web PubSub envelope parsing, event dedupe, jittered reconnect
backoff, 48-hour retention + atomic cache writes, clipboard retry and pause semantics,
single-instance behaviour, log redaction, and end-to-end
`negotiate → event → fetch → clipboard → history` plus refresh-on-401 — all with fakes, no
live Google or Azure calls.

## Package

```powershell
cd windows/installer
& "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" /Q VoicePrompt.iss
# -> windows/installer/output/VoicePrompt-Setup.exe
```

The installer is **per-user and non-Store**:

- installs to `%LOCALAPPDATA%\Programs\VoicePrompt` with `PrivilegesRequired=lowest` (no UAC),
- Start-menu group, optional desktop icon, and — when the *Startup* task is selected — a single
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value,
- **exactly one auto-start mechanism.** The `Run` value is the only one, and it is the same
  value the app's Settings toggle reads and writes (`RegistryAutoStartManager`). The installer
  deliberately creates **no** Startup-folder shortcut, and removes a legacy one left by earlier
  builds, so the tray app can never be launched twice at sign-in and the toggle always reflects
  reality,
- bundles **only** the Desktop OAuth client JSON (never the Android or Web client, never the
  `.secrets/` folder),
- checks for the .NET 8 Desktop Runtime and links to the download if missing,
- asks a running instance to exit first (`VoicePrompt.exe --quit`) so upgrades never force-kill,
- **preserves user data** in `%LOCALAPPDATA%\VoicePrompt` across upgrade *and* uninstall.

Silent install / uninstall:

```powershell
.\output\VoicePrompt-Setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /TASKS="startupicon"
& "$env:LOCALAPPDATA\Programs\VoicePrompt\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
```

> **SmartScreen.** The installer is **not code-signed**. On first run Windows SmartScreen shows
> *"Windows protected your PC"* — choose **More info → Run anyway**. Signing would require a
> code-signing certificate, which is out of scope for a personal sideload build.

## Use

- Launch from the Start menu. The app starts **hidden** — look for the microphone-cloud icon in
  the notification area (you may need to expand the overflow chevron).
- **Tray menu:** *Open* · *Copy latest* · *Pause notifications* · *Exit*.
- **Double-click** the tray icon (or launch the app again) to open the settings/history window.
- **First use:** open the window → *Settings* → *Sign in*. Your default browser opens Google's
  consent screen; the app listens on an ephemeral `http://127.0.0.1:<port>/` loopback address
  for the redirect and validates the `state` value. No embedded web view is ever used.
- **History:** double-click, <kbd>Enter</kbd>, or <kbd>Ctrl</kbd>+<kbd>C</kbd> copies the
  selected transcript. *Copy latest* and manual copies work **even while notifications are
  paused**. <kbd>Esc</kbd> hides the window; <kbd>F5</kbd> refreshes history from the backend.
- **Pause notifications:** transcripts are still fetched and cached, but nothing is copied
  automatically and no toast is shown.

## How it works

- **Auth.** Google OIDC **Authorization Code + PKCE** (S256) in the system browser with an
  ephemeral `127.0.0.1` `HttpListener` redirect, scopes `openid email profile`,
  `access_type=offline`. The refresh token is encrypted with **Windows DPAPI (CurrentUser)** and
  written atomically to `%LOCALAPPDATA%\VoicePrompt\tokens.bin`. ID tokens are refreshed ~5
  minutes before expiry. The backend bearer is always the **ID token** — the OAuth access token
  is never used as a backend credential. `invalid_grant`, or a refresh with no `id_token`, moves
  the app to *Sign in required*; signing out deletes the cache.
- **Realtime.** `POST /v1/realtime/negotiate` (authenticated) → `ClientWebSocket` with the
  `json.webpubsub.azure.v1` subprotocol → Azure server-envelope parsing
  (`system`/`message`/`ack`, `from` = `server` or `group`, `dataType` = `json` or `text`), with a
  bare payload object also accepted. Reconnects use exponential backoff with full jitter capped
  at 60 s; the access URL is renegotiated ~2 minutes before `expires_at`; suspend/resume is wired
  to `SystemEvents.PowerModeChanged`.
- **On event.** Dedupe on `event_id` (bounded FIFO) → `GET /v1/transcripts/{id}` → cache locally
  → copy on an **STA** thread with bounded retry → tray notification carrying the short preview
  only.
- **API rules.** Exactly one refresh + retry on `401`; `403` is terminal; `429`/`5xx`/timeouts /
  network errors retry with bounded backoff; error bodies are parsed as RFC 9457
  `application/problem+json`.
- **Cache.** `history.json` under LocalAppData, written atomically (temp file + replace), pruned
  to the newest 200 entries and a **48-hour** retention window on startup and every 30 minutes.
  **No audio is ever stored.**
- **Privacy.** Logs are redacted (JWTs, `Bearer` headers, `access_token`/`code`/`client_secret`
  query values) and transcript bodies are never logged or put in a notification.
- **Single instance.** A `Local\`-scoped named mutex per Windows user; a second launch asks the
  running instance to show its window instead of starting a second tray icon.
- **Accessibility.** All controls carry `AutomationProperties.Name`, buttons and tabs have access
  keys, keyboard focus is visible (orange focus ring), the history list is fully keyboard
  operable, and the window opens with focus placed on the tab strip.

## Troubleshooting

| Symptom | Cause / fix |
|---------|-------------|
| Connection shows *Not configured* | No Desktop OAuth client JSON found — see **Configure**. |
| Connection shows *Sign in required* | No/expired refresh token — open the window and sign in. |
| App does not start | Missing .NET 8 **Desktop** Runtime (x64). |
| Nothing is copied | *Pause notifications* is on, or the clipboard was held by another app (use *Copy latest*). |
| Diagnostics | `%LOCALAPPDATA%\VoicePrompt\logs\voiceprompt.log` (redacted, size-capped). |

## Manual verification still required

Only one thing cannot be automated here: a **real interactive Google sign-in** and the
resulting live end-to-end run (record on Android → transcript lands on the Windows clipboard).
Everything else — build, tests, packaging, install/launch/uninstall, backend reachability and
auth rejection — is covered above.

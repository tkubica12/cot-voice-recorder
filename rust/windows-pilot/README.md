# Native Windows migration client

This is an isolated **native migration client**, not an accepted production
replacement. Its default mode remains the platform-only foundation.
Closing the window or pressing Esc hides it. A second ordinary launch activates
the existing pilot; benchmark launches with a readiness path reject duplicates.

Without `--state-directory` (and outside `--installed` mode) it has no Google login, network connection, clipboard
or transcript/history access. With that explicit argument it loads only
`settings.json`, `tokens.bin` and `history.json` in the selected existing absolute
directory. `--oauth-config` is also explicit. Isolated mode refuses the real
installed state directory and its descendants; no installed tokens/history/audio
are discovered. Its
`Local\VoicePrompt.RustPilot.*` mutexes and window classes are separate from C#.
Isolated mode uses a stock system icon; installed mode loads the packaged app.ico.

Windows code is isolated in `src\platform.rs`; the audio core still forbids unsafe
code. The Windows crate version was queried in the user-approved crates.io source,
resolved by Cargo and recorded in the existing workspace lockfile.

## Run and verify

From the repository root:

```powershell
cargo fetch --manifest-path rust\Cargo.toml --locked
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-windows-pilot

# A real hidden-window/tray startup, focus check, duplicate rejection and DPAPI
# health check. The self-test exits after one second; a desktop Shell is required.
rust\target\release\VoicePromptRustPilot.exe --self-test --instance-key local-test

# Interactive, limited status/tray pilot. This does not start an agent or migration loop.
rust\target\release\VoicePromptRustPilot.exe
rust\target\release\VoicePromptRustPilot.exe --quit --instance-key interactive
```

Current-user DPAPI uses exactly `VoicePrompt.DPAPI.v1` UTF-8 entropy, matching the
existing C# protector. It forbids prompts, never selects LocalMachine and releases
DPAPI allocations with LocalFree. A synthetic roundtrip is checked before tray
readiness. Ciphertext is deliberately nondeterministic. Bidirectional C# tests
check decryptability, not ciphertext equality, and reject corrupted input.
This proves the encryption envelope only, **not** token JSON, complete recovery
manifest formats, authentication behavior, or other users/machines.

```powershell
dotnet build rust\tests\reference\ReferenceAudio.csproj -c Release
backend\.venv\Scripts\python.exe rust\windows-pilot\tests\test_platform_parity.py `
  --pilot rust\target\release\VoicePromptRustPilot.exe `
  --reference rust\tests\reference\bin\Release\net8.0\ReferenceAudio.dll
```

The C# reference links the actual existing Core project. All parity inputs are
synthetic; these commands do not read real tokens or recordings. CI runs native
unit/parity checks on Windows, not the tray self-test on a potentially headless
runner. Non-Windows invocation fails explicitly; the audio crate is portable.

The explicit-path encrypted `tokens.bin` store has fixture hooks for
cross-language tests. File replacement is same-directory, flushed,
and never falls back to copy/delete across volumes. Corrupt files remain intact
and return a redacted error; no existing token cache is loaded by tray startup.
OAuth/client contracts are documented in
[`client-protocol`](../client-protocol/README.md).

## Explicit-path native client

The connected mode adds native sign-in/out, realtime status, a 48-hour History
list and read-only text view, Copy latest/selected/original, persisted pause and
reconnect. User-triggered sign-in opens the system browser and uses the bounded
PKCE loopback flow. The realtime path fetches with a Google ID token, saves history
before clipboard delivery and queues work to the native UI thread. No WebView
or .NET runtime is used. Native microphone, both dictation modes, global shortcuts,
nonactivating overlay, guarded paste, Recovery and audit outbox are now wired.
These surfaces are implemented but have not passed real-device or production E2E.

```powershell
# Use an explicitly chosen isolated directory; do not use installed app state for tests.
rust\target\release\VoicePromptRustPilot.exe --instance-key isolated-client `
  --state-directory D:\chosen-isolated-state

# Only for an explicitly authorized real sign-in, not unattended tests:
rust\target\release\VoicePromptRustPilot.exe --instance-key isolated-client `
  --state-directory D:\chosen-isolated-state --oauth-config D:\chosen-desktop-client.json
```

Clipboard writes use immediate `CF_UNICODETEXT` publication from moveable global
memory, transferring ownership only after successful `SetClipboardData`.
Bounded retries apply only to a busy clipboard. Pause, suspended/sign-out state
and dictation-busy policy are rechecked at the actual UI write; an abandoned UI
request is cancelled so it cannot write later. Audit-source events never auto-copy.
Manual cached copying works while paused or signed out.
Notifications contain generic readiness/fallback messages, not transcript text.

Settings preserve every current C# field and retain unknown fields on Rust writes.
Invalid/corrupt settings fail explicitly instead of silently selecting production
defaults. Per-minute history cleanup persists retention; read errors preserve files.
Native Settings includes the current backend, pause, autostart, shortcut/language,
streaming/polishing and final-wait fields. Saving while idle persists atomically,
reconnects the native client and applies the one per-user Run mechanism. Standard
controls provide keyboard dialog navigation, DPI-specific system fonts and
per-monitor bounds. Human screen-reader/high-DPI acceptance remains pending.
History supports manual cloud Refresh (no autocopy), Clear with confirmation and
copy latest/selected/original; Recovery supports list/recover/explicit deletion.

The microphone adapter deliberately preserves the existing WinMM/WaveIn path:
16 kHz mono PCM16, 40 ms buffers and a bounded 50-buffer disk-consumer queue.
WinMM is legacy; WASAPI is Microsoft's recommendation for new designs, but changing
capture engines and resampling concurrently would invalidate behavior/latency parity.
Overload and device/storage failures stop explicitly with durable recovery retained.
Tests exercise synthetic queues, not a real microphone.

Hold selects its focus target at Start, hands-free at Stop. The guard latches
window/process/thread/focused-control changes, waits for released modifiers,
checks the clipboard sequence captured inside the publication lock and sends
exactly one Ctrl+V without Enter. Stale/cancelled/auth-invalidated UI jobs cannot
paste. The overlay does not activate a foreground app. Real paste has not been tested.

Polishing remains opt-in, single physical request, bounded rolling windows and
literal unique nonoverlapping edits against the original text. Stop freezes late
responses and leaves unprocessed tails raw; failures stay in diagnostics/History.
Audit freezes before delivery and drains independently every 30 seconds while idle.
Only acknowledged upload deletes the outbox, with fixed original completion/expiry.
Pause does not block explicit manual copy or audit upload; account/backend changes do.

The installed/isolated CLI parser is tested without filesystem/IPC access.
Packaged `VoicePrompt.exe` and explicit `--installed` always select the installed
client plan; value-option order cannot turn them into the platform-only tray.
Installed mode rejects fixture/readiness/explicit-path flags before any personal
state is opened. Process tests exercise only those rejection paths, not a real
installed launch or upgrade.

## Full-client signed-out measurement

The separate `--signed-out-fixture <marked-root> --fixture-ready-file <new-path>
--instance-key <N-guid> --run-for-seconds <1-3600>` mode initializes the actual
client/recovery/dictation/audit/overlay lifecycle without enabling physical ports.
It refuses token caches, OAuth, installed state and reparse paths and publishes
only `full-client-signed-out-fixture` readiness, never foundation readiness.
The matching C# tool starts the actual WPF App with isolated paths/IPC; see
`scripts\measure_clients.py` and its guard tests. These are bounded signed-out
measurements, not real-device or whole-migration acceptance.

## Recovery codec parity, not the recovery runtime

`--recovery-load-fixture` and `--recovery-save-fixture` take an explicit absolute
`<lowercase-session-id>\manifest.enc` path. They use the existing DPAPI entropy,
bounded decrypt/parse and atomic publication; corrupt input leaves the old file
intact. Reparse points in any ancestor are refused. These are codec/parity hooks,
not the quota-aware recovery runtime. The connected tray's separate Recovery UI
and real journal are described below.

The fixture suite reads real C# v3/v4 journals (including pending and frozen audit
state), compares plaintext byte-for-byte and reopens Rust output through the actual
C# `RecoveryStore`. Synthetic v1/v2 fixtures verify both readers and upgrades without
inventing absolute cursors or target context.

```powershell
backend\.venv\Scripts\python.exe rust\windows-pilot\tests\test_recovery_parity.py `
  --pilot rust\target\release\VoicePromptRustPilot.exe `
  --reference rust\tests\reference\bin\Release\net8.0\ReferenceAudio.dll
```

`NativeRecoveryStore` now implements the real durable journal separately from
those codec hooks. A zero-byte `writer.lock` uses Windows share mode zero for
cross-process/session exclusion; stores in the same process share one root gate.
Only one active journal owner is allowed per ID. All mutations reread the latest
manifest under that gate. Root quota is 256 MiB by default and includes orphan
files, previous targets and staging space. Confirmations publish before audio
deletion; same-text legacy retries do not need another manifest publication.
Cleanup uses the original strict `>48 h` updated-at boundary and preserves active
or unreadable sessions. Deletion refuses nested directories before deleting files.

Eight native tests cover reopen/sample parity, quota failure before publication,
orphan accounting, confirmation failure without audio deletion, concurrent capture
and result updates, active ownership, corruption and retention. The external suite
also exercises two actual writer processes and real Windows junction rejection
before read/write/delete. Native-created journals reopen through actual C# Core;
C# v1-v4 fixtures also open through the full native store.

The extra root lock file contains no data and is ignored by legacy C# directory
listing. C# **does not cooperate with this new writer lock**: cutover still requires
verified application single-instance/upgrade coordination, and two real engines
must never share the same root. Tests use sequential C#/Rust inspection of isolated
fixtures, not overlapping capture engines.

`LegacySession` now provides single-producer chunking, C# JSON checkpoints, at most
two ASR requests, capture-order output, terminal auth/backoff progress and durable
stop-timeout recovery. `StreamingSession` provides independent two-second durable
capture, live provisional preview versus confirmed output, quiet-pause commits,
50-minute rollover, auth-aware replay from the persisted completed cursor and batch
Recovery with retained context. A received ACK/provisional event never releases
audio. Confirmation beyond a sent commit is rejected.

Synthetic session tests cover out-of-order legacy completion, bounded concurrency,
stop timeout/shutdown, restart audio, live fragmentation, provisional failure,
full replay with exact samples, partial-confirmed replay and batch overlap recovery.
They use real encrypted journals but fake ASR/model peers, not a microphone or paid AI.
Backend binding preserves the original URL/newline/email format and is checked again
under the token session lock; sign-out cancels a bound request before its text is used.

Device capture/hotkeys/overlay/paste, polishing/outbox scheduling and native Recovery
UI are now wired. Library/session checks are not complete interactive dictation E2E.

## Build-only migration installer

`scripts\Build-RustWindowsInstaller.ps1` builds the release executable and an
Inno Setup per-user x64 installer under ignored `rust\target\installer-output`.
It uses the existing installed compiler, never installs tooling, never runs the
installer and stages only the executable and repository icon. It bundles no OAuth
configuration, tokens, history, settings or recovery and needs no .NET runtime.

The source installer reuses the existing AppId/Run value, preserves user data and
requests graceful synchronous `--quit`; inability to exit aborts, with no force-kill.
The native compatible-instance adapter uses the exact C# mutex/auto-reset events.
Bidirectional tests run real C#/Rust processes in randomized fixture namespaces,
not the installed application's namespace.

`--installed` (also selected by packaged `VoicePrompt.exe`) is the runtime cutover
path: it uses the compatible instance guard and existing per-user configuration and
state. **Do not run it or the installer for unattended tests.** Actual installation,
upgrade/rollback and personal-state acceptance require the later explicit consent.
The built installer is a migration preview, not a released or accepted replacement.

The desktop-only lifecycle runner uses temporary synthetic caches. Its self-test
disables both OAuth configuration and the real clipboard and checks hidden startup
focus plus the actual native History list. It is not a production E2E or a real
clipboard/paste test. CI does not run this desktop-dependent command.

```powershell
backend\.venv\Scripts\python.exe rust\windows-pilot\tests\test_client_lifecycle.py `
  --pilot rust\target\release\VoicePromptRustPilot.exe
```

## Measure this limited foundation

Choose an existing, empty output directory. The collector does not overwrite old
reports. It launches only `VoicePromptRustPilot.exe`, uses fresh isolated instance
keys, verifies PID/readiness, stops each owned instance gracefully and drains
diagnostic streams. Errors are failures, not faster startup samples.

```powershell
.\scripts\Measure-RustNativePilot.ps1 `
  -Executable rust\target\release\VoicePromptRustPilot.exe `
  -OutputDirectory "$env:TEMP\voiceprompt-native-measurements" `
  -SourceRevision YOUR_BUILT_COMMIT
```

The defaults are 30 process starts and 301 memory/CPU samples over five minutes.
The collector measures new-process to observed **native tray + DPAPI readiness**,
not OS cold-cache startup, Google login, transcription, the cloud, or user-perceived
dictation latency. Readiness is published atomically without overwriting.
Connected mode refuses foundation readiness arguments to prevent mislabeled
performance evidence.
The native process has a one-hour failsafe; memory runs are limited to 55 minutes.

`native-foundation-startup.json` retains every trial and the binary hash;
`native-foundation-memory.json` uses the common process sampler. The startup
report explicitly marks comparison with the complete client as invalid.
Much lower numbers for this incomplete foundation cannot pass the full pilot
performance goals. Full feature-parity measurements must be repeated later.

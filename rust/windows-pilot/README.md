# Native Windows foundation

This is an isolated **native migration client**, not a dictation client or a
production replacement. Its default mode remains the platform-only foundation.
Closing the window or pressing Esc hides it. A second ordinary launch activates
the existing pilot; benchmark launches with a readiness path reject duplicates.

Without `--state-directory` it has no Google login, network connection, clipboard
or transcript/history access. With that explicit argument it loads only
`settings.json`, `tokens.bin` and `history.json` in the selected existing absolute
directory. `--oauth-config` is also explicit; no installed configuration or user
state is discovered. Its
`Local\VoicePrompt.RustPilot.*` mutexes and window classes are separate from C#.
It uses a stock system icon until the client packaging stage.

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
or .NET runtime is used. No microphone, dictation, Recovery, global shortcut or
autostart registration is implemented yet.

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
Native controls provide keyboard dialog navigation and DPI-scaled geometry, but
full accessibility/font/DPI acceptance and the complete Settings UI remain pending.
This is not installer/upgrade single-instance compatibility with C#.

## Recovery codec parity, not the recovery runtime

`--recovery-load-fixture` and `--recovery-save-fixture` take an explicit absolute
`<lowercase-session-id>\manifest.enc` path. They use the existing DPAPI entropy,
bounded decrypt/parse and atomic publication; corrupt input leaves the old file
intact. Reparse points in any ancestor are refused. These are codec/parity hooks,
not a quota-aware, leased recovery journal; the tray does not discover Recovery.

The fixture suite reads real C# v3/v4 journals (including pending and frozen audit
state), compares plaintext byte-for-byte and reopens Rust output through the actual
C# `RecoveryStore`. Synthetic v1/v2 fixtures verify both readers and upgrades without
inventing absolute cursors or target context.

```powershell
backend\.venv\Scripts\python.exe rust\windows-pilot\tests\test_recovery_parity.py `
  --pilot rust\target\release\VoicePromptRustPilot.exe `
  --reference rust\tests\reference\bin\Release\net8.0\ReferenceAudio.dll
```

Recovery root quota/staging accounting, active journal fencing, durable checkpoint
publication/deletion ordering, streaming/fallback sessions, outbox upload and native
Recovery UI remain to be ported. Codec parity must not be reported as recovery E2E.

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

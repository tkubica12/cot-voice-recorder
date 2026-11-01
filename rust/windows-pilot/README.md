# Native Windows foundation

This is an isolated **platform pilot**, not a dictation client or a production
replacement. Its native tray menu can open a status window or exit. Closing the
window or pressing Esc hides it. A second ordinary launch activates the existing
pilot; benchmark launches with a readiness path instead reject duplicates.

The pilot has **no Google login, network connection, microphone, clipboard writes,
global shortcuts, transcript/history access, or autostart registration**. It never
opens the installed app's tokens, settings or recovery directories. Its
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

The next increment adds an explicit-path encrypted `tokens.bin` store and fixture
hooks for its cross-language tests. File replacement is same-directory, flushed,
and never falls back to copy/delete across volumes. Corrupt files remain intact
and return a redacted error; no existing token cache is loaded by tray startup.
OAuth/client contracts are documented in
[`client-protocol`](../client-protocol/README.md). They are not wired into the
interactive tray yet.

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
The native process has a one-hour failsafe; memory runs are limited to 55 minutes.

`native-foundation-startup.json` retains every trial and the binary hash;
`native-foundation-memory.json` uses the common process sampler. The startup
report explicitly marks comparison with the complete client as invalid.
Much lower numbers for this incomplete foundation cannot pass the full pilot
performance goals. Full feature-parity measurements must be repeated later.

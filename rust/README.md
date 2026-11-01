# Rust migration foundation

This is the staged foundation on `refactor-rust`, not a replacement
Windows client or backend. The existing clients, release builds and cloud remain
unchanged. Scope, targets, safeguards and progress are in
[the migration plan](../docs/rust-migration.md).

`voiceprompt-core` ports the current mono PCM16 WAV encoder, the backend's legacy
and strict WAV validation modes, and the Windows legacy/recovery PCM chunker.
It retains the existing 20 ms frames, silence gate, 180 ms pre-roll, six-second
chunks, 600 ms overlap and checkpoint state. This is not the live streaming
checkpoint journal, DPAPI persistence, ASR, or a claim of lossless capture.

The audio crate has no external crates and forbids unsafe code. The isolated
[`windows-pilot`](windows-pilot/README.md) adds native tray/lifecycle and DPAPI
primitives using the user-approved crates.io exception for this repository.
[`client-protocol`](client-protocol/README.md) adds OAuth/PKCE/token-cache/JWT,
loopback callback, auth-state and PubSub contracts. These are not yet a complete
interactive sign-in or connected client.
Cargo.lock is committed. Fetch its locked packages once, then run checks offline.
Python/npm/NuGet feeds remain unchanged. Never use GitHub source dependencies or
disable security controls as a workaround.

## Build and test

From the repository root, with the existing Rust tools in PATH:

```powershell
cargo fetch --manifest-path rust\Cargo.toml --locked
cargo test --manifest-path rust\Cargo.toml --workspace --offline --locked
cargo fmt --manifest-path rust\Cargo.toml --check
cargo clippy --manifest-path rust\Cargo.toml --workspace --offline --locked --all-targets -- -D warnings
cargo build --manifest-path rust\Cargo.toml --offline --locked --release --example audio_contract
```

The manifest declares Rust 1.85 or newer (edition 2024). The initial increment is
checked with the installed Rust 1.99.1; the minimum compiler is not separately
verified yet. A compiler version is not an SDK crate availability guarantee.
`rust-ci` fetches locked approved crates, then runs offline workspace checks on
Windows/Linux. Windows adds process-sampler safeguards and native DPAPI parity.
Windows also checks client-contract and encrypted token-store compatibility.
The audio cross-language comparison remains the separate command below; CI does
not claim to run it. Interactive tray startup is checked locally, not in CI.

## Cross-language parity

The reference .NET executable now links the actual Core project (including
`PcmChunker.cs` and the DPAPI protector), not a second implementation.
The Python runner imports the actual backend WAV validator.
All inputs are deterministic synthetic samples; there is no microphone, account,
network, clipboard operation or personal recording.

```powershell
dotnet build rust\tests\reference\ReferenceAudio.csproj -c Release
backend\.venv\Scripts\python.exe rust\tests\test_audio_parity.py `
  --rust-oracle rust\target\release\examples\audio_contract.exe `
  --dotnet-oracle rust\tests\reference\bin\Release\net8.0\ReferenceAudio.dll
```

The Rust example and C# reference are test probes only. They accept stdin and emit
WAV bytes, validation properties, or a binary chunk list including overlap flags
and absolute cursors. They are not shipped in an installer. Errors return nonzero
and print diagnostics to stderr.

## Read-only process measurement

`scripts\Measure-VoicePromptProcess.ps1` samples one explicit existing PID. It does
not start, restart, stop, configure or attach a debugger to the app. Choose the
PID and describe the actual scenario; an uncontrolled existing session is not a
controlled idle benchmark.

```powershell
# Replace 12345 with the intended process ID. The output directory must exist.
.\scripts\Measure-VoicePromptProcess.ps1 -ProcessId 12345 `
  -Scenario 'controlled-idle-empty-test-history' `
  -OutputPath "$env:TEMP\voiceprompt-idle-run-01.json"

.\scripts\tests\Test-ProcessMeasurement.ps1
```

The default is 301 samples at one-second intervals (about five minutes). Reports
contain every sample, actual elapsed time, binary SHA-256/product version,
working set, private bytes, threads, handles and interval CPU. Summary percentiles
use nearest rank; mean CPU uses total CPU delta over actual measurement duration.
One-core and whole-machine CPU percentages are separate. Private bytes measure
committed private memory, not resident physical memory.

Reports are published by a same-directory rename after a durable write.
Existing output files are never overwritten. A missing/exited process, invalid
parameters or inaccessible counters fail explicitly rather than publishing a
complete-looking report. This sampler does not measure startup, child processes,
Linux RSS, full end-to-end dictation latency, or cloud billing. `SourceRevision`
is optional; no source revision is inferred from an installed executable.

Generated reports belong in the session artifacts or another explicitly chosen
local output directory, not in source control. This increment alone cannot pass
the full migration's performance or compatibility checks.

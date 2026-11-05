# Rust applications

> **Source cleanup:** current compatibility commands use `rust/tests/golden` and `scripts/uv.lock`, not the removed C#/Python applications. Older revision-scoped measurement/reference descriptions below remain historical evidence, not runnable legacy-product instructions. The live Azure cutover remains separate and unperformed.

Windows and cloud application source is Rust; Android remains Kotlin. The original application sources are retired, while independent frozen wire/data contracts and small OS/SDK test tools preserve compatibility. Existing public releases and the live Azure cloud have not been replaced by this source cleanup. See [the migration ledger](../docs/rust-migration.md).

`voiceprompt-core` ports the current mono PCM16 WAV encoder, the backend's legacy
and strict WAV validation modes, and the Windows legacy/recovery PCM chunker.
It retains the existing 20 ms frames, silence gate, 180 ms pre-roll, six-second
chunks, 600 ms overlap and checkpoint state. This is not the live streaming
checkpoint journal, DPAPI persistence, ASR, or a claim of lossless capture.

The audio crate has no external crates and forbids unsafe code. The isolated
[`windows-pilot`](windows-pilot/README.md) adds native tray/lifecycle and DPAPI
primitives using the user-approved crates.io exception for this repository.
[`client-protocol`](client-protocol/README.md) adds OAuth/PKCE/token-cache/JWT,
loopback callback, auth-state and PubSub contracts. The explicit-path native mode
now wires these contracts into sign-in, realtime delivery, durable history,
clipboard commands and native dictation/recovery/settings/installer surfaces.
The real-device, accessibility and upgrade acceptance is still incomplete.
Cargo.lock is committed. Fetch its locked packages once, then run checks offline.
[`cloud`](cloud/README.md) now adds the staged strict OIDC/UAMI and storage REST
foundation plus runnable API/relay/worker/cleanup, batch AI/PubSub adapters and
source-only classical ACA image selection. Full contract/performance/human
acceptance remains incomplete; it is not an accepted production replacement.
Python/npm/NuGet feeds remain unchanged. Never use GitHub source dependencies or
disable security controls as a workaround.

## Build and test

From the repository root, with the existing Rust tools in PATH:

```powershell
cargo fetch --manifest-path rust\Cargo.toml --locked
cargo test --manifest-path rust\Cargo.toml --workspace --offline --locked
cargo fmt --manifest-path rust\Cargo.toml --all --check
cargo clippy --manifest-path rust\Cargo.toml --workspace --offline --locked --all-targets -- -D warnings
cargo build --manifest-path rust\Cargo.toml --offline --locked --release --example audio_contract
```

The manifest declares Rust 1.85 or newer (edition 2024). The installed Rust1.99.1
runs local checks; disposable Windows/Linux CI also compiles all targets on1.85.
A compiler version is not an SDK crate availability guarantee.
`rust-ci` fetches locked approved crates, then runs offline workspace checks on
Windows/Linux. Windows adds process-sampler safeguards and native DPAPI parity.
Windows also checks client-contract and encrypted token-store compatibility.
The audio cross-language comparison remains the separate command below; CI does
not claim to run it. Interactive tray startup is checked locally, not in CI.

## Cross-language parity

The versioned [golden corpus](tests/golden/README.md) was independently verified against the actual legacy implementations before removal. Rust black boxes now replay those contracts without importing the old apps. CurrentUser DPAPI/isolated IPC use only the small independent `tests/platform-oracle` tool; no Windows Core project is linked.

```powershell
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-core --example audio_contract
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-client-protocol --example client_contract
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-cloud --examples
uv run --project scripts --locked python rust\tests\test_golden_contracts.py --examples rust\target\release\examples
```

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

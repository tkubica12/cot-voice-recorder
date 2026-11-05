# Client authentication and realtime contracts

> **Source cleanup:** current compatibility commands use `rust/tests/golden` and `scripts/uv.lock`, not the removed C#/Python applications. Older revision-scoped measurement/reference descriptions below remain historical evidence, not runnable legacy-product instructions. The live Azure cutover remains separate and unperformed.

This crate ports client-side contracts and the transcript-delivery coordinator.
The native tray has an opt-in explicit-state mode for interactive Google sign-in
and realtime/clipboard/history delivery. Unattended tests use synthetic state and
local HTTP/WebSocket fixtures, never real credentials, endpoints or clipboard data.

Implemented:

- Google installed/web client JSON, minimal OIDC authorization URL, OS-random
  PKCE S256 and constant-time state checks.
- A real `127.0.0.1` callback listener that keeps its ephemeral port bound,
  limits headers/time, supports cancellation and validates before a success reply.
- C#-compatible PascalCase cached-token JSON with fractional RFC3339 timestamps.
- Read-only JWT scheduling/display claims and refresh response classification.
  The backend still verifies signatures; OAuth access tokens are not ID tokens.
- A single-owner auth state machine. Refresh caches must persist before publishing
  new live tokens. Revocation stops vending stale ID tokens. Failed durable
  sign-out remains an error even though in-memory token vending stops.
- A bounded async Google token transport with form encoding, no redirect following,
  native system TLS verification, explicit network/deadline/body failures and a
  one-MiB response limit. Plain HTTP is allowed only by a test-only loopback client.
- Async sign-in/refresh/sign-out orchestration with serialized session ownership,
  blocking cache/callback workers, status notifications and callback cancellation.
  Local integration exercises callback -> code exchange -> persisted ID token.
- Web PubSub system/ack/message envelopes, raw and string JSON payloads,
  `transcript.completed` validation and bounded FIFO event deduplication.
- An authenticated backend API client for negotiation and transcript retrieval,
  structured RFC 9457 failures, bounded transient retries and exactly one refresh
  after 401. HTTP response and transcript payload bounds are explicit.
- A real WebSocket receiver negotiating `json.webpubsub.azure.v1`, bounded
  buffers/messages and event backpressure, proactive URL renewal, ping/pong,
  suspend/resume/reconnect/stop controls and auth-change cancellation.
  Reconnect backoff preserves the existing 50%-100% jitter and 60-second cap.
- Direct WebSocket fault tests cover absolute URL renewal even after a slow
  handshake, full-channel stop/sign-out, malformed/oversized frames, consumer
  shutdown and a non-reading transport. Ping/pong writes have a three-second
  deadline and remain cancellable by auth/control/renewal.
- Compatible history JSON, 48-hour retention, newest-first 200-entry bounds and
  replace-by-transcript-ID behavior. The native history store persists atomically
  and surfaces corrupt data without discarding the file.
- Delivery policy preserving pause, dictation-busy suppression and the
  `windows_dictation` audit source's no-autocopy rule.
- Serialized coordinator: authenticated fetch -> durable history -> safe copy ->
  generic notification, with bounded dedupe committed only after persistence.
  Failed fetch/disk operations remain retryable; clipboard exhaustion keeps history
  and exposes manual-copy fallback. Manual latest/selected/original works paused.
- Current C# settings fields/defaults, bounded JSON and unknown-field preservation
  for rollback. Malformed settings and unsafe backend URLs are explicit errors.
- Recovery binary v1-v4 codec, C# UTC tick precision, audio SHA-256/canonical WAV
  verification, confirmed cursor transitions and fixed audit completion.
  Old v1/v2 unknown cursors are preserved; new output uses v3 or v4.
  Streaming gaps/overlap/odd cursors, malformed lengths/flags and conflicting
  confirmations/audits are rejected. Checkpoint planning validates before native
  audio publication. The native crate now supplies the durable journal; dictation
  engines and interactive recovery are wired in the native crate; real-device
  and complete performance/accessibility acceptance remain separate.
- Authenticated dictation WAV, bounded literal-edit polishing and immutable audit
  APIs. A real proxy WebSocket client validates readiness/cursors, sends bounded
  PCM/commit/finish, separates provisional/confirmed messages and invalidates both
  halves after auth changes. 401 refreshes exactly once.
- Legacy chunker PascalCase/base64 JSON, prefix-preserving stitching and preview
  semantics. Unicode normalization/category tables are deliberately pinned to the
  .NET 8-compatible profile rather than the newest tables. Direct reference parity
  covers valid BMP scalars, representative supplementary compatibility characters,
  invariant casing, short-word overlap and empty adjacency.

The optional `loopback-fixtures` feature is used only by the Windows cloud
dev-dependency to run native-engine -> real client -> real Rust router -> model
fixture/audit E2E. It allows only explicit `http://127.0.0.1:port` fixture URLs.
The normal constructor still requires HTTPS, system TLS verification is unchanged,
and the normal app/installer dependency graph does not enable this feature.

Token/config/PKCE/claims/frame Debug output is redacted. Parsing errors do not
embed input bodies. Invalid callback security fields, config endpoints, JWT
claims, token JSON and oversized packets are explicit errors rather than
success-shaped defaults.

The existing valid C# wire behavior is preserved. Deliberate malformed-input
hardening rejects duplicate OAuth security parameters, blank configuration and
unsafe endpoint/redirect URLs. The caller must log the redacted error and keep
sign-in/connection state visible; parser errors are not complete notifications.

`windows-pilot` provides `EncryptedTokenStore`: current-user DPAPI with the
original entropy, bounded reads and same-directory flushed atomic replacement.
Unlike the legacy blanket catch, corrupt/unreadable cache loading returns an
explicit error and preserves the file. Only absence returns `None`.

## Checks

From the repository root:

```powershell
cargo test --manifest-path rust\Cargo.toml --workspace --offline --locked
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-client-protocol --example client_contract
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-windows-pilot
dotnet build rust\tests\platform-oracle\PlatformOracle.csproj -c Release
uv run --project scripts --locked python rust\client-protocol\tests\test_client_parity.py `
  --client rust\target\release\examples\client_contract.exe `
  --pilot rust\target\release\VoicePromptRustPilot.exe `
  --reference rust\tests\platform-oracle\bin\Release\net8.0\PlatformOracle.dll
```

All inputs and temporary caches are synthetic. Tests compare actual C# Core
methods and persisted encrypted files in both directions, including replacement.
The callback test uses local TCP only. No browser, Google, Azure, private OAuth
configuration or existing user tokens are involved.

The native `--cache-save`, `--cache-load` and `--cache-clear` modes are fixture
hooks taking an explicit `tokens.bin` path. Never point tests at an installed
app's state. They are not interactive sign-in commands.

The native explicit-state tray now wires the browser flow and delivery coordinator.
Remaining: deeper direct fault/E2E/performance acceptance, actual installer upgrade,
and real Google/clipboard/device/accessibility acceptance. Do not label the local
fixture flow as end-to-end production/Google validation.

Native capture/session/controller, Settings/History/Recovery and outbox scheduling
are now wired. The current native installer is build-only and has not been installed.
The backend summary model is also checked locally with `--backend-model` on the
dictation parity runner (requires the existing backend Python environment);
Rust-only CI does not run that optional Python-model check. This exposed and fixed
the summary receipt's absence of `character_count`.

The API/transport primitives and native wiring above now exist. Auth status
publication occurs under the session lock; a delayed
refresh cannot restore SignedIn or vend a stale bearer after sign-out.

```powershell
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-core --example audio_contract
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-client-protocol --example client_contract
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-cloud --examples
uv run --project scripts --locked python rust\tests\test_golden_contracts.py --examples rust\target\release\examples
```

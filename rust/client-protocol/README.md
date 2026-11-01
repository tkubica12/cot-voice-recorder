# Client authentication and realtime contracts

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
  engines and interactive recovery wiring remain separate unfinished work.

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
dotnet build rust\tests\reference\ReferenceAudio.csproj -c Release
backend\.venv\Scripts\python.exe rust\client-protocol\tests\test_client_parity.py `
  --client rust\target\release\examples\client_contract.exe `
  --pilot rust\target\release\VoicePromptRustPilot.exe `
  --reference rust\tests\reference\bin\Release\net8.0\ReferenceAudio.dll
```

All inputs and temporary caches are synthetic. Tests compare actual C# Core
methods and persisted encrypted files in both directions, including replacement.
The callback test uses local TCP only. No browser, Google, Azure, private OAuth
configuration or existing user tokens are involved.

The native `--cache-save`, `--cache-load` and `--cache-clear` modes are fixture
hooks taking an explicit `tokens.bin` path. Never point tests at an installed
app's state. They are not interactive sign-in commands.

The native explicit-state tray now wires the browser flow and delivery coordinator.
Remaining: the dictation/recovery/audit API surfaces, full native Settings/Recovery,
installer/upgrade and real Google/clipboard/device acceptance. Do not label the local
fixture flow as end-to-end production/Google validation.

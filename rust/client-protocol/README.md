# Client authentication and realtime contracts

This crate ports client-side contracts, not the complete interactive Google
login or a connected realtime loop. The native tray still does not access real
credentials, a microphone, production endpoints, or the clipboard.

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
app's state. These hooks do not wire authentication into the tray UI.

Remaining: native user-initiated browser flow/UI, integration of realtime events with
clipboard/history delivery and the rest of the dictation/audit API surfaces. Do not label
the local fixture flow as end-to-end production/Google validation.

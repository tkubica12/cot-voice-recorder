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
- Web PubSub system/ack/message envelopes, raw and string JSON payloads,
  `transcript.completed` validation and bounded FIFO event deduplication.

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

Remaining: bounded HTTP token transport and asynchronous auth orchestration,
user-initiated browser flow/UI, connected negotiate/reconnect/suspend behavior,
authenticated transcript retrieval and clipboard/history delivery. Do not label
this crate's tests as full sign-in or end-to-end production validation.

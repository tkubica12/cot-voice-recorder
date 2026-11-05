# Frozen legacy compatibility contracts

These are deterministic **synthetic test data**, not a second application.
`manifest.json` identifies the original source commit, individual C#/Python
source hashes, probe hashes, executed exporter sources and the reference tests
that independently checked each vector before source removal.

The initial export passed 32 original cross-language tests, followed by 14
Windows platform/recovery tests against the actual C# application library.
The corpus contains 808 core/client/cloud vectors, 159 actual FastAPI HTTP
responses, v1-v4 recovery plaintext, three file-wire fixtures and both exact
polishing prompts. It includes full WAV/chunk bytes, restored cursors and overlap,
Unicode normalization, OAuth/PKCE negatives, token/cache shapes, entities, relay
ordering, protected edits, audit envelopes and rejected-body keepalive cases.

The `.zlib` blobs are content-addressed by SHA-256. The runner checks both their
declared decompressed length and digest. Inputs and outputs are deduplicated;
no audio was recorded. No personal configuration, live credentials, account,
clipboard contents or machine/user-bound DPAPI ciphertext is included.

Run the black-box corpus after building the four Rust contract examples:

```powershell
python rust\tests\test_golden_contracts.py --examples rust\target\release\examples
```

HTTP uses a fresh loopback RSA/JWKS authority and the explicit Rust test server,
not the installed client or existing backend. Current-time audit fields are
shifted together; the expected canonical envelope/digest is recomputed from
the frozen Python envelope. Server-generated creation/receipt times must be
valid and within the current run. All other checked wire fields remain frozen.
The existing error contract checks and the complete 40-iteration rejected-body
sequence are retained, without a performance profile.
The historical lifecycle probe deliberately used the accepted free-form client
ID `lifecycle`, whereas OpenAPI describes a UUID. That baseline is preserved,
not silently rewritten or made stricter during source cleanup. Its schema errors
must remain identical to the frozen Python result. A separate canonical-UUID
Rust HTTP lifecycle must pass OpenAPI with no errors, including the nullable
failure reason, chunk response and Location header. Schema reference/keyword
guards and negative enum/edit bounds also remain checked.

Windows DPAPI and named-kernel-object interoperability use the small independent
`platform-oracle` test tool, not the old client/Core project. The tool uses
CurrentUser DPAPI with UTF-8 `VoicePrompt.DPAPI.v1` entropy. Ciphertext is generated
for the current test user. `legacy_reference.py` restores only these versioned
synthetic wire files and independently reads their documented binary fields;
it has no application engine, auth service or network implementation.

Frozen vectors do not replace the Rust unit/fault/security/concurrency tests,
the independent Azure SDK storage contracts, or actual deployment acceptance.
The historical C#/Python evidence remains revision-scoped.

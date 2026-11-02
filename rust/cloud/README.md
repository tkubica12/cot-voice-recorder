# Rust cloud migration

This crate provides runnable staged Rust API/worker/cleanup entry points, **not an
accepted production replacement** for the existing Python services. Classical ACA, UAMI,
private Storage/Speech and the Android contract remain unchanged.

Implemented:

- Google ID-token RS256/JWKS verification with bounded/cached key retrieval,
  issuer/audience/exp/iat/nbf checks, verified-email semantics and one-email allowlist.
  Client-side JWT scheduling is not reused as signature verification.
- Explicit production configuration; fake/auth-disabled/Azurite modes cannot
  accidentally boot production. No account keys or client secrets are accepted.
- ACA injected `IDENTITY_ENDPOINT`/`IDENTITY_HEADER` with user-assigned
  `AZURE_CLIENT_ID`, private endpoint/no-proxy checks and finite approved resource
  scopes. Token/response bodies are not logged.
- Native REST Blob, Table and Queue primitives using managed identity. Table
  replacement requires the exact opaque ETag and rejects wildcard/unconditional
  updates. Queue renewal replaces its pop receipt; stale receipts are explicit.
- Stable RFC 9457 problem response fields and authentication challenge.
- Recording lifecycle/upload/completion/finalize reservation and transcript/audit
  services, wired through the actual Axum router. Readiness remains false until
  explicit UAMI bootstrap probes of existing resources.
- Batch MAI Speech and alternate Foundry transcription, exact-prompt literal-edit
  refinement, and Web PubSub REST negotiation/preview-only delivery. Ephemeral
  dictation and refinement retain four/two physical slots after waiter cancellation
  or timeout. Provider bodies are not exposed.
- Server-owned MAI upstream and authenticated live PCM WebSocket relay, with
  ordered confirmations, explicit cursors, bounded commit backpressure and
  no confirmed/done output after a missing-completion timeout.
- Continuous recording lease primitives and strict durable PCM/sample replay
  reader. Table write results retain their own response ETag, not a racy
  subsequent GET snapshot.
- Leased continuous model processor with full durable replay after failure, exact
  sample/legacy-overlap handling, queue receipt heartbeat and joined shutdown.
  A sequential queue loop and legacy/finalization processors now share the ports.
- Python-profile full-casefold overlap stitching, deterministic UUID5 publication
  identities and separate exact-prompt recording refinement. Canonical Blob lease
  acquire/renew/write/release primitives protect active writers. Finalization uses
  a renewable lock and immutable first-writer canonical bodies, conditional
  metadata, stable IDs and durable notification retries without extending expiry.
- Cleanup with complete bounded Blob enumeration, conditional recording fencing,
  active audio/continuous/finalizer ownership checks, fixed audit expiry and
  conservative retention of fresh unpublished audio.
- `voice-recorder api|worker|cleanup|check-config`, existing-resource bootstrap
  probes and SIGTERM/Ctrl+C shutdown. Startup never creates resources.

Stored domain/entity codecs, strict wire defaults, content-digest/sample validation
and conditional repository primitives now exist.
Source-only container/ACA image selection is wired, but container validation,
full external/OpenAPI/config/fault contracts and feature-matched performance
remain incomplete. Do not deploy or run an alternate worker over existing data.

```powershell
cargo fetch --manifest-path rust\Cargo.toml --locked
cargo test --manifest-path rust\Cargo.toml --offline --locked -p voiceprompt-cloud
cargo clippy --manifest-path rust\Cargo.toml --workspace --offline --locked --all-targets -- -D warnings
```

The direct cloud tests use generated in-memory RSA keys and local HTTP/WebSocket
fixtures. They exercise real signature validation, cached JWKS, rejected
HS256/unsigned/wrong claims, UAMI selection, Table CAS, Queue lease receipts,
actual HTTP handlers, multipart/provider contracts, physical-call cancellation,
ordered PCM relay, actual 15-second/30-second timeout boundaries, lease takeover/
late-publication fencing, exact contiguous/legacy continuous model replay,
receipt rotation and graceful producer/heartbeat shutdown.
The current Windows workspace has 213 passing tests, including 94 cloud library tests and
two CLI guards. Read-only resource probes and active-writer-safe cleanup use
synthetic local fixtures.
No Azure, paid model, production data or credentials are used. These are not
private-endpoint/UAMI acceptance against the actual cloud.

Dependencies were queried in the user-approved crates.io exception and resolved
in the existing workspace lockfile. `jsonwebtoken` 9.3.1 and `quick-xml` 0.38.3
retain the declared Rust 1.85 profile; the newest XML version required a newer
compiler. The installed compiler and CI verify the locked graph, not the MSRV
itself. RSA key generation is a test-only dependency; no private key fixture is
committed.

The explicit MSRV CI job has checked all targets on Windows/Linux with Rust1.85
in disposable runners. No compiler was installed on the user's machine. Supported preview/
upload overrides are wired through the services; incompatible retention, batch
or PCM overrides fail explicitly instead of being silently ignored.

## Stored data compatibility

Recording/chunk/transcript codecs retain the existing partition/row keys,
legacy pipeline defaults, nullable fields, dates, samples, lease/finalization
flags and audit digests. Ordinary integers use `Edm.Int32`; large counters use
`Edm.Int64`. Dates retain the Python/SDK microsecond profile.

The direct Python runner links the current repository's actual Table writers and
readers and the installed Azure SDK deserializer, not a duplicate reference.
It reads Python output in Rust and reopens Rust-generated entities in Python,
including a 2.7-billion-sample counter and nullable source metadata.

```powershell
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-cloud --example entity_contract
backend\.venv\Scripts\python.exe rust\cloud\tests\test_entity_parity.py `
  --oracle rust\target\release\examples\entity_contract.exe
```

This uncovered the Python reader's inability to unwrap SDK `EntityProperty`
Int64 values. The coupled reader repair and regression tests preserve ordinary
int behavior and make the rollback reference able to read large counters.
An older unrepaired Python image still has that Int64 limitation; do not claim
acceptance for such an image or mix its worker with Rust. Backend CI now preserves
the existing Microsoft-feed lock rather than re-resolving against public PyPI,
and runs the exact bidirectional SDK/entity check.

REST references: [ACA identity endpoint](https://learn.microsoft.com/azure/container-apps/managed-identity),
[Table conditional replacement](https://learn.microsoft.com/rest/api/storageservices/update-entity2).
The Azure topology and full acceptance criteria are in
[`docs\rust-migration.md`](../../docs/rust-migration.md).

## Direct service and provider compatibility

The edit runner compares the current Python validator and exact existing system
prompt with Rust. The audit runner calls the current Python archive/retrieval
services and compares Rust-generated envelope bytes, digests, fixed expiry,
preview and original/final text. All fixtures are synthetic and in memory.

```powershell
backend\.venv\Scripts\python.exe rust\cloud\tests\test_edit_parity.py --oracle rust\target\release\examples\entity_contract.exe
backend\.venv\Scripts\python.exe rust\cloud\tests\test_archive_parity.py --oracle rust\target\release\examples\entity_contract.exe
backend\.venv\Scripts\python.exe rust\cloud\tests\test_relay_parity.py --oracle rust\target\release\examples\entity_contract.exe
backend\.venv\Scripts\python.exe rust\cloud\tests\test_pipeline_text_parity.py --oracle rust\target\release\examples\entity_contract.exe
```

These do not prove live Azure connectivity, full WebSocket/worker parity,
production readiness or feature-matched performance. No cloud deployment or paid
model call is part of these checks.

## Runtime and source-only packaging

```powershell
rust\target\release\voice-recorder.exe --help
rust\target\release\voice-recorder.exe check-config
```

`check-config` validates explicit `VR_` settings without network or identity calls.
The other modes require injected ACA UAMI and existing resources; they are not
authorized production tests. Startup fake adapters are deliberately not silently
substituted; direct tests inject explicit isolated adapters.

The root-context `rust\cloud\Dockerfile` builds a non-root UID10001 image with the
same API8000/worker/cleanup entry point. The current local Docker daemon is
unavailable; CI has built and probed the image without publishing it. The source-only
`infra\deploy.ps1 -BackendRuntime Rust` switch preserves all classical ACA topology
and keeps Python as its default/rollback. Do not execute it until deployment is
separately approved.

## Actual HTTP reference contracts

```powershell
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-cloud --example cloud_contract
backend\.venv\Scripts\python.exe rust\cloud\tests\test_http_parity.py --oracle rust\target\release\examples\cloud_contract.exe
```

`backend-ci` also records a Linux **authenticated in-memory protocol** performance
profile using this HTTP example and the actual FastAPI/Uvicorn factory. Both use
real synthetic RS256/JWKS verification and the same recording/upload/audit workload.
The artifact includes all30 starts per implementation, three load series, RSS
observations and per-operation p95 thresholds. AI, realtime, workers, Azure and
production/container limits are explicitly excluded: this is not full backend
performance acceptance. The collector refuses normal application binary names,
personal environment configuration and overwritten/partial report publication.
See `scripts\measure_backend.py` and the [acceptance plan](../../docs/rust-migration.md).

The opt-in `storage::azurite_tests::https_azure_contract` is also wired into
`backend-ci` with an immutable official Azurite image. It exercises the actual
REST adapters over verified local HTTPS, real Table ETags/Blob leases/Queue
receipts, and bidirectional current Azure SDK payloads. Storage API-version
checks remain enabled. Identity, certificate and data are explicit ephemeral
fixtures; no real Azure resources, account data or credentials are used.
Azurite's basic OAuth mode does **not** prove token signatures, permissions,
managed identity or private-endpoint deployment. The normal binary/config has
no emulator/shared-key fallback. Ordinary tests keep this scenario ignored;
the bounded Linux runner is `scripts\test_azure_rest.py`.

The opt-in `loopback-fixtures` feature adds a constructor used only by the
`cloud_contract` example for an **adapter-matched authenticated HTTPS Azurite**
profile. It requires a fresh explicit fixture root, private synthetic identity
header, strict `127.0.0.1` endpoints, an explicit trusted ephemeral certificate
and per-process Blob/Queue/Table namespaces. It does not relax TLS, API-version
checks or normal production configuration. The actual Python factory uses the
current Azure SDK adapters with the same synthetic identity and workload, not
in-memory storage. The existing readiness/RSS/API-p95 thresholds are unchanged.
Namespaces are created and removed outside the timed interval; ordinary adapter
initialization/probes remain timed. The SDK independently reopens each load
process's recording/chunk/transcript/Blob workload before deleting its namespace.

`backend-ci` builds the feature explicitly, runs the existing REST/worker/cleanup
contract, then collects 30 starts each and three 100-flow API series against the
same pinned emulator. `backend-azure-performance.json` is published only after
owned emulator/identity/private-key cleanup succeeds. The actual
[33d04aa Linux run](https://github.com/tkubica12/cot-voice-recorder/actions/runs/41523050769)
completed with all original scoped thresholds passing: readiness p95
147.049 ms versus Python809.698 ms; RSS18.887/21.379/23.480 MiB versus
108.813/117.910/129.207 MiB; worst API-p95 ratio82.160%. All raw samples,
62 SDK-checked/deleted namespaces and66 source/lock hashes were independently
verified. These are short post-load RSS observations, not an hour soak.
Neither it nor the older memory profile measures actual Azure IAM/private
networking, deployed ACA readiness, model inference, worker/cleanup/realtime
latency or a cutover. The production image does not enable the fixture feature.

Rejected HTTP request bodies now have an explicit bounded connection lifecycle.
Small unread bodies with a known length may drain up to 64 KiB/100 ms after the
handler chooses its original problem response. Unknown-length, oversized, failed
or stalled unread bodies close the connection without unbounded polling.
Already-consumed requests incur no second read. Validation order, status/problem
shapes, bearer checks and audio bounds remain unchanged; no retry masks a reset.
Direct Windows keepalive tests cover repeated rejected-body/following-request
sequences as well as the existing no-poll invalid-metadata invariant.

The loopback-only server uses real RS256 verification against an in-memory
generated-key local JWKS. The probe compares live Rust HTTP status/headers,
structured errors, lifecycle and audit/history shapes with the actual FastAPI
factory. No real account, cloud, model, database or credentials are involved.
This is a bounded contract corpus, not exhaustive Pydantic coercion/message-text
or distributed-fault parity.

Windows cloud tests additionally connect the actual native DPAPI recovery journal,
both dictation engines and authenticated client transports to the real Rust router
and synthetic model peers. These require no microphone or clipboard. A finishing
relay still observes disconnect/signout and releases its model/slot immediately;
it never converts provisional text into confirmed output. The loopback transport
feature is confined to the Windows dev-dependency, not the normal app/image graph.

Cleanup fault checks cover deletion failure and durable resumption: an expired
body delete must not first discard its retry metadata; orphan plain bodies are
retained while an active canonical publisher owns their lease.

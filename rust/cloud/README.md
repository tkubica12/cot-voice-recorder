# Rust cloud migration

This crate is the staged cloud foundation, **not yet a runnable or production-ready
replacement** for the existing Python API/worker/cleanup. Classical ACA, UAMI,
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

Stored domain/entity codecs, strict wire defaults, content-digest/sample validation
and conditional repository primitives now exist. API handlers, live MAI relay,
ordered worker replay, finalization, cleanup, container/ACA wiring and full external
contracts remain unfinished. Blob enumeration and production bootstrap are not wired.
Do not deploy the current library or run an alternate worker over existing data.

```powershell
cargo fetch --manifest-path rust\Cargo.toml --locked
cargo test --manifest-path rust\Cargo.toml --offline --locked -p voiceprompt-cloud
cargo clippy --manifest-path rust\Cargo.toml --workspace --offline --locked --all-targets -- -D warnings
```

The fourteen direct cloud tests use generated in-memory RSA keys and local HTTP
fixtures. They exercise real signature validation, cached JWKS, rejected
HS256/unsigned/wrong claims, UAMI selection, Table CAS and Queue lease receipts.
No Azure, paid model, production data or credentials are used. These are not
private-endpoint/UAMI acceptance against the actual cloud.

Dependencies were queried in the user-approved crates.io exception and resolved
in the existing workspace lockfile. `jsonwebtoken` 9.3.1 and `quick-xml` 0.38.3
retain the declared Rust 1.85 profile; the newest XML version required a newer
compiler. The installed compiler and CI verify the locked graph, not the MSRV
itself. RSA key generation is a test-only dependency; no private key fixture is
committed.

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

# backend/ — Cloud backend

**Owner:** backend component.
**Tech:** Python 3.13 · FastAPI · uv · Azure Container Apps (API min replicas 1, worker 0), managed identity only.

The stateless API and workers that orchestrate the recorder: authenticate the user,
accept recording sessions and audio chunks, transcribe Czech with `MAI-Transcribe-2`
through Azure Speech, and refine (`gpt-5.6-luna` default / `gpt-5.6-terra`) through
Azure AI Foundry, persist
short-lived transcripts, and push `transcript.completed` events over Azure Web PubSub.

Implements the contract in [`../openapi/voice-recorder.yaml`](../openapi/voice-recorder.yaml)
and the behavior in [`../docs/architecture.md`](../docs/architecture.md).

## Layout

```
backend/
├── pyproject.toml / uv.lock      # project + pinned deps (runtime + dev)
├── Dockerfile / .dockerignore    # production image (API + worker + cleanup)
├── docker-compose.azurite.yml    # local Azurite for integration tests
├── .env.example                  # non-secret settings/placeholders
├── src/voice_recorder/           # typed package
│   ├── app.py                    # FastAPI factory, middleware, problem handlers
│   ├── config.py                 # settings (env-driven, prod validation)
│   ├── auth.py                   # Google OIDC verifier (replaceable)
│   ├── domain.py / models.py     # domain entities + wire models
│   ├── services/                 # recording lifecycle, pipeline, cleanup
│   ├── repositories/             # Table repos + in-memory fakes
│   ├── storage/                  # Blob + Queue adapters + in-memory fakes
│   ├── ai/                       # Azure Speech/Foundry adapters + fakes
│   ├── realtime/                 # Web PubSub gateway + in-memory fake
│   ├── routers/                  # health, recordings, transcripts, realtime
│   ├── worker.py                 # queue worker (bounded retries)
│   └── __main__.py               # `voice-recorder api|worker|cleanup`
└── tests/                        # pytest unit/API + opt-in Azurite integration
```

## Prerequisites

- Python 3.13 and [uv](https://docs.astral.sh/uv/).
- Docker (only for the Docker image and the opt-in Azurite integration tests).

## Setup

```powershell
cd backend
uv sync --extra dev
```

> If your environment routes PyPI through a mirror, set `UV_DEFAULT_INDEX` before
> `uv sync` (e.g. `$env:UV_DEFAULT_INDEX="https://<mirror>/pypi/simple/"`).

## Developer commands

```powershell
# Lint, format check, and type check
uv run ruff check src tests
uv run ruff format --check src tests
uv run mypy

# Unit + API tests (no Docker, no cloud)
uv run pytest --ignore=tests/integration

# Run the API locally (fakes for AI/Web PubSub, Azurite for storage)
Copy-Item .env.example .env    # then edit as needed
uv run voice-recorder api      # http://localhost:8000  (docs at /docs)

# Run the queue worker and the cleanup job
uv run voice-recorder worker
uv run voice-recorder cleanup
```

Health probes are unauthenticated: `GET /health/live`, `GET /health/ready`.
With `VR_AUTH_DISABLED=true` (local only) send `Authorization: Bearer dev-token`.

## Opt-in dictation refinement benchmark

`tools\benchmark_refinement.py` compares the existing whole-transcript refinement
prompt with exact text-edit patches, 30-second background blocks, and 6-second
fragments with one-fragment lookahead. It uses the fictional Czech/English corpus
in `tests\fixtures\refinement_benchmark.json`, not microphone audio or private history.
It does **not** enable refinement in Windows or change the production prompt.

Run from `backend` using the existing development environment:

```powershell
# Offline safeguards; no model calls.
.\.venv\Scripts\python.exe -m pytest tests\test_refinement_benchmark.py

# Print the bounded request plan without contacting Azure.
.\.venv\Scripts\python.exe -m tools.benchmark_refinement `
  --output C:\benchmarks\luna-run-01

# Opt in to paid inference using an already authenticated Azure CLI identity.
# Use an existing resource with a gpt-5.6-luna deployment and data-plane access.
# The output directory must not already exist.
.\.venv\Scripts\python.exe -m tools.benchmark_refinement `
  --endpoint https://YOUR-RESOURCE.cognitiveservices.azure.com/ `
  --output C:\benchmarks\luna-run-01 --run --paced

# Recompute diagnostics from saved responses, without additional model calls.
.\.venv\Scripts\python.exe -m tools.benchmark_refinement `
  --output C:\benchmarks\luna-run-01 --summarize
```

The default matrix makes 261 calls including warmup; `--paced` adds ten calls in
a real-time 60-second **text-arrival** replay. Concurrency is two, SDK retries are
disabled, the network-operation timeout is 120 seconds, and five failures abort remaining
queued work. Requests already in flight finish and are retained. There are no automatic model
substitutions or permission changes. Credentials are not written to the results.
`--resume` runs only unrecorded cases with identical saved prompts and endpoint;
it does not retry failures. `--skip-case CASE_ID` records an explicit exclusion
without sending the input, for example when stopping further tests containing
context that already triggered a provider policy filter. Exclusions are not
successful cleanups. `--patch-blocks-only` runs a separate 40-block incremental
patch follow-up (41 requests with warmup) in a new output directory; adding
`--paced` to that profile makes two additional requests in a 60-second replay.

Results include exact prompts, corpus hash, request IDs, returned model version,
first-content and complete-response timings, token/cache/reasoning usage, original
model outputs, assembled transcripts, and literal-preservation diagnostics.
Malformed, ambiguous, overlapping or truncated edit patches are rejected, not
silently applied. Raw results are flushed after each completed request.

Incremental stop-time estimates replay **measured** request durations against
**simulated** 1/5/20-minute arrivals. Separate estimates add an assumed one-second
ASR lag; that lag is not a measured MAI result. The paced trial validates a real
one-minute wall-clock schedule, still without audio, the production API, or paste.
Three whole-text samples per length are exploratory, not a reliable p95 estimate.
Inspect semantic changes manually: exact literal checks do not prove that every
detail or intended repetition survived. Additional diagnostics flag new scripts
and long repetitions not present in the input, including accidentally echoed
read-only context. Failed/skipped incremental blocks can be assembled using
explicitly marked raw-text fallback; their timings are not clean-refinement
successes. Deployment-specific results and the
integration decision belong in [the dictation design](../docs/windows-dictation.md).

## Azurite integration tests (opt-in)

Unit tests never require Docker. The Azurite-backed tests are opt-in:

```powershell
docker compose -f docker-compose.azurite.yml up -d
$env:VR_RUN_AZURITE="1"; uv run pytest -m azurite
docker compose -f docker-compose.azurite.yml down
```

## Docker image

The single image serves the API, the worker, and the cleanup job:

```powershell
docker build -t voice-recorder-backend .
# If PyPI is proxied: docker build --build-arg UV_DEFAULT_INDEX=https://<mirror>/pypi/simple/ -t voice-recorder-backend .

# Smoke-start fully self-contained (in-memory storage + fake AI/Web PubSub)
docker run --rm -p 8000:8000 `
  -e VR_ENVIRONMENT=local -e VR_USE_FAKE_STORAGE=true `
  -e VR_USE_FAKE_AI=true -e VR_USE_FAKE_REALTIME=true `
  -e VR_AUTH_DISABLED=true `
  voice-recorder-backend
# then: curl http://localhost:8000/health/ready
docker run --rm voice-recorder-backend worker
docker run --rm voice-recorder-backend cleanup
```

The container runs as a non-root user and starts quickly; the worker and API handle
`SIGTERM` gracefully for Container Apps scale-in.

## Configuration

All settings are environment variables prefixed with `VR_` (see `.env.example`).
Production access to Azure Storage, Azure Speech, Web PubSub, and Azure AI Foundry uses a shared
**user-assigned managed identity** — no keys or connection strings. The identity's
client id is passed to every container as `AZURE_CLIENT_ID` so `DefaultAzureCredential`
selects it. The API, queue worker, and cleanup job run as three separate Container
Apps resources (external-ingress API, no-ingress queue-scaled worker, scheduled job)
that all share this one identity. The gated local mode (`VR_ENVIRONMENT != production`) may use Azurite
(`VR_STORAGE_USE_AZURITE=true`) and in-memory AI/realtime fakes (`VR_USE_FAKE_AI`,
`VR_USE_FAKE_REALTIME`). Config validation refuses any of these shortcuts in
production and refuses to start without the required audience/allowlist/endpoint
settings.

## Behavior notes / contract clarifications

### Optional low-latency dictation polishing

`POST /v1/dictation/refine` accepts UTF-8 `application/json`:
`{"text":"current block","previous_text":"read-only preceding context"}` and returns
`{"text":"validated current block","edits":[{"original":"exact anchor","replacement":"correction"}]}`.
The existing Google ID token and
single-user allowlist are required. `previous_text` defaults to `""`; current text
must be nonblank and at most **4000 Unicode characters**, previous context at most
**8000**. The body is limited to **32768 bytes before JSON parsing**, even without
Content-Length. Unknown fields, duplicate JSON keys, invalid UTF-8 and compressed
bodies are rejected. No whitespace normalization or truncation is performed.

This optional path is independent of MAI transcription and Android/recording
refinement, intended for opt-in **background rolling windows**, never to delay raw
paste or rewrite an entire session at stop. Its primary objective is removing
unintended ASR word/phrase repetitions, especially chunk seams. A repetition spanning
the end of read-only context and start of current text may delete only the second
occurrence in current text. Clear ASR fixes and accidental fillers are allowed;
intentional repetition/emphasis/counting, ambiguous speech and numeric literals stay.
No style polishing, synonym substitution or guessing missing words.
It uses `VR_FOUNDRY_ENDPOINT`, `VR_FOUNDRY_API_VERSION` (2024-10-21),
`VR_FOUNDRY_SCOPE`, and `VR_REFINE_DEPLOYMENT_DEFAULT` (gpt-5.6-luna), with a dedicated
managed-identity client, `reasoning_effort=none`, at most 2048 completion tokens,
**zero SDK retries**, and **one model call per request**. No new credentials, keys,
deployment or dependencies are needed. Gated `VR_USE_FAKE_AI=true` returns unchanged
text locally. Two dedicated threads per API process allow **two concurrent calls**;
excess requests return `429` immediately with `Retry-After: 1`. The response
deadline and upstream HTTP timeout are **20 seconds**. Windows allows **25 seconds**
including transport overhead, but never waits for AI at final assembly. A timed-out or cancelled
waiter retains its slot until its synchronous work actually ends. Shutdown drains
work off-loop before closing the dedicated client and credential.

**API runtime configuration:** these are the existing settings, not new secrets:

| Setting | Required effective value |
| --- | --- |
| `VR_FOUNDRY_ENDPOINT` | `https://tomaskubica-foundry-resource.cognitiveservices.azure.com/` (or the configured compatible Foundry resource) |
| `VR_FOUNDRY_API_VERSION` | `2024-10-21` |
| `VR_REFINE_DEPLOYMENT_DEFAULT` | Existing deployment `gpt-5.6-luna`, not the versioned model name |
| `VR_FOUNDRY_SCOPE` | Default `https://cognitiveservices.azure.com/.default`; explicit env override is unnecessary |
| `AZURE_CLIENT_ID` | Existing attached runtime user-assigned managed identity's client ID |

The declared infrastructure already supplies the endpoint/API version/deployment
and identity client ID through `commonEnv` to **both API and worker**
(`infra/modules/apps.bicep`). They share the attached user-assigned identity;
`infra/main.bicep` and `infra/modules/foundry-rbac.bicep` already assign it
**Cognitive Services User** on the existing Foundry account. No additional API
environment variable, role assignment or infrastructure change is needed when the
deployment matches these declarations. The API needs outbound access to that
Foundry endpoint and managed-identity token acquisition. Runtime permission or
configuration drift must be checked independently; the local CLI smoke test does
not verify the deployed Container App's identity.

The model proposes only `{"edits":[["current","exact original substring","replacement"]]}`.
At most 64 edits are accepted, with each anchor/replacement limited to 256 characters.
The server validates the entire set against the **original** current text: exact
unique anchors (including overlapping occurrences), no overlaps, current-only IDs,
no malformed or truncated output, and no fuzzy repair. It preserves line breaks,
tabs and each line's leading/trailing whitespace; edits cannot cross a line break
or rewrite whitespace alone. Internal ordinary spaces may change with corrections
or deletions, so `This is is a test.` can become `This is a test.`. It preserves
numeric literals and lexical technical identifiers, rejects new letter scripts,
new long repetitions/context echoes, and empty output for meaningful input. An
empty edits array is valid. The response contains the **same validated edits** used
to assemble `text`, ordered by original location, with `{original,replacement}`
strings and **no positions** (avoiding Unicode-code-point versus UTF-16 offsets).
Clients must locate all anchors in the original request snapshot and apply them
atomically, not search sequentially in modified output. No-op edits are omitted;
unchanged text always returns `edits: []`. Existing text-only clients may ignore
this additive field. The prompt treats transcripts as untrusted data and
preserves languages, negations, meaning, technical names and intentional repetition;
these conservative checks are **not a proof of semantic equivalence**.

Errors use `application/problem+json`: `401/403` authentication/allowlist, `413`
body size, `415` content type/encoding, `422` invalid JSON/fields, `429` capacity,
`502` rejected/invalid/unsafe model response, `503` unavailable configuration or
transient provider failure, and `504` deadline. No model retry or fallback occurs
on the backend. Clients should keep raw text on **any** failure, including `404`
from an older server without this optional route.

Metadata-only diagnostics emit fixed rejection categories (for example
`anchor_missing`, `anchor_not_unique`, `protected_numbers`, `line_structure`),
plus per-request outcome and elapsed milliseconds. They never include anchors,
replacements, transcripts, provider bodies, exception text or credentials.

**Privacy:** both text fields and the output exist only transiently in process
memory and are sent to the configured Foundry provider. No Blob/Table storage,
queue, recording history, worker refinement or Web PubSub is used. All handled
responses have `Cache-Control: no-store`; logs never include text, prompts, model
responses, provider error bodies or credentials. This is application-level
nonpersistence, not a claim about provider retention.

The opt-in integration test runs a local API with isolated test auth and real
Foundry inference through the existing Azure CLI identity (synthetic Czech/English
spelling, deduplication, filler, seam and protected-repetition cases, no access or
infrastructure changes):

```powershell
$env:VR_RUN_LIVE_DICTATION_REFINEMENT = "1"
.\.venv\Scripts\python.exe -m pytest tests\integration\test_live_dictation_refinement.py -s
Remove-Item Env:VR_RUN_LIVE_DICTATION_REFINEMENT
```

It reports only status, latency, model version and token counts. Regular test runs
skip it; do not enable it with private dictation or change production authentication.
The expanded test also reports edit count, removed-character count and boolean
checks for expected output, literal preservation and proposal/response consistency.
It validates the actual provider proposal against the returned `{text, edits}`,
not a fake or a separately reconstructed model output. Latency is recorded, not
asserted; the normal API deadline remains in force.

**Expanded synthetic validation, 2026-09-16:** existing Azure CLI credentials
successfully called deployment `gpt-5.6-luna`, returning model
`gpt-5.6-luna-2026-07-09`. Three exploratory runs exposed an omitted Czech
cross-context seam deletion, a filler deletion leaving a comma, and an ambiguous
within-block anchor rejected with `anchor_not_unique`. The prompt was clarified
for multilingual seams, filler punctuation and anchors containing both copies.
One intermediate run also hit the existing five-second Azure CLI credential
process timeout; no SDK retries, deadline changes or dependency changes were made.

The last run returned HTTP 200 for all eight cases, with zero reasoning tokens
and exact proposal/response consistency, but **failed the Czech cross-context
seam assertion**. Do not treat this as a passing live quality gate:

| Synthetic case | Last observed result | Local API seconds |
| --- | --- | ---: |
| Czech clean text | Unchanged | 6.701 |
| English spelling | Repaired | 1.867 |
| English repeated word | Removed 3 characters | 1.810 |
| Czech repeated phrase | Removed 15 characters | 1.600 |
| Filler and delimiters | Removed 5 characters | 1.871 |
| Intentional `1, 1, 2, 3.` | Unchanged, empty edits | 1.779 |
| English context/current seam | Removed 17 characters; `literal123` and `8443` intact | 1.637 |
| Czech context/current seam | Unchanged despite expected deletion; literals intact | 1.545 |

The Czech seam did delete correctly in the intermediate run (25 characters,
1.679 seconds), so observed behavior is **inconsistent**, not unsupported.
The opt-in test deliberately retains the exact expected deletion and fails rather
than accepting a no-op. These tiny exploratory samples, including the first
request's credential acquisition, are not reliability or latency guarantees.
Only fixed synthetic text was sent; no History/audio was accessed, and no cloud
resources were changed or application build deployed/installed.

**Exact release-probe sentences, 2026-09-16:** the separate `release-probe` group
also passed against real `gpt-5.6-luna-2026-07-09` through the local API. It sends
only the two approved synthetic Czech/English deployment sentences, with empty
previous context. Czech `novou verzi aplikace` and English `the new version` each
occurred exactly once afterward; port `8443` and the complete authentication-safety
sentence were unchanged. Both returned one validated edit, HTTP 200 and zero
reasoning tokens. Observed local latency was **5.482 seconds Czech** (first request,
including credential acquisition) and **1.922 seconds English**, without timing
assertions. This within-block result does not resolve the separate intermittent
Czech cross-context seam failure above and does not verify a deployed API/client.

Run just those two fixed release probes from `backend`:

```powershell
$env:VR_RUN_LIVE_DICTATION_REFINEMENT = "1"
.\.venv\Scripts\python.exe -m pytest tests\integration\test_live_dictation_refinement.py -k release-probe -s
Remove-Item Env:VR_RUN_LIVE_DICTATION_REFINEMENT
```

On 2026-09-16 the local endpoint smoke test returned HTTP 200 for both synthetic
requests using actual model **`gpt-5.6-luna-2026-07-09`**: Czech 6.925 seconds
(unchanged valid text, 11 completion tokens), English 1.977 seconds (spelling repaired,
19 completion tokens). Both used **zero reasoning tokens**. These are local
end-to-end observations including CLI token acquisition, not production latency
guarantees. A desktop with a stricter two-second budget will legitimately retain
raw text, especially for a cold credential/model call. Windows' 4000/8000 UTF-16-unit
caps are a conservative subset of the API's Unicode-character limits; the
32768-byte serialized JSON limit applies independently.

### Low-latency Windows dictation transcription

`POST /v1/dictation/transcribe?language=auto` accepts **raw `audio/wav`** (not
multipart or JSON) and returns `200 application/json` with exactly `{"text": "..."}`.
The existing Google ID-token authentication and single-user allowlist are required;
there is no anonymous dictation route. Local fake auth still requires its dev bearer
token. `language` defaults to `auto`; the only explicit alternatives are `cs` and
`en`. Automatic mode omits Speech `locales`, allowing multilingual recognition.
Use `auto` for Czech, English, or mixed-language speech. Explicit `cs` and `en` are
strong **single input-language hints**, not output/translation languages; the backend
does not infer either from desktop culture or the Android `VR_TRANSCRIBE_LANGUAGE`.
Dictation returns the provider's source-language text without translation or refinement.
It sends neither a translation task, target language, nor a custom prompt.

Microsoft's [MAI guide](https://learn.microsoft.com/azure/ai-services/speech-service/mai-transcribe)
documents automatic multilingual detection when `locales` is omitted and supports
only one locale when supplied (do not send `["cs", "en"]`). `clean` removes fillers
and formats speech; it is not a translation mode. The
[feature matrix](https://learn.microsoft.com/azure/ai-services/speech-service/llm-speech#feature-availability)
lists translation and custom prompts as unsupported by MAI-Transcribe-2. If Czech
speech produces English, first verify that the client actually sends `auto`, not
`en`. An incorrect result with `auto` can still be a provider recognition error;
request-shape tests do not prove live language-identification accuracy.

- Audio must be a complete RIFF/WAVE file with one nonempty data chunk, uncompressed
  PCM format code 1, **16 kHz, 16-bit, mono**, and **at most 10 seconds** (320,000 PCM
  bytes). The total request limit is **324,096 bytes**, allowing 4 KiB of container
  overhead at the maximum duration. RIFF/chunk lengths, frame alignment, and PCM byte
  rate are checked; truncated/duplicate chunks and empty audio are rejected. Size is
  enforced while streaming even without `Content-Length`. Content encoding other
  than identity is not accepted. Audio is never silently shortened.
- Clients should send nonoverlapping batches **at most 8 seconds**, cut on silence,
  with **at most two requests in flight** and preserve capture order when pasting
  responses. No recording creation, completion call, digest, or chunk index is needed.
- A separate MAI Azure Speech client uses `enhancedMode`, the configured MAI model
  (default `MAI-Transcribe-2`), `clean` style, and a **20-second HTTP timeout**. A
  **20-second response deadline** also covers token acquisition and the synchronous
  call. There are no automatic transcription retries or fallback models. Existing
  Android transcription keeps its Czech default, glossary, verbatim style, and
  configured long timeout; dictation sends no glossary or refinement prompt.
- Each API process permits **four concurrent dictation calls** on dedicated threads;
  excess work gets immediate `429` with `Retry-After: 1`, not an unbounded queue.
  A timed-out or cancelled request retains its slot until the actual underlying
  call ends, because Python cannot cancel a running synchronous HTTP call. Shutdown
  drains these calls off the event loop before closing the Speech client.
- Errors are RFC 9457 `application/problem+json`: `401`/`403` authentication,
  `413` total body size, `415` media/content encoding, `422` malformed/empty WAV,
  duration, format, or language, `429` capacity, `502` provider rejection or malformed
  response, `503` transient provider failure or unsupported configuration, and `504`
  response deadline. A valid provider no-speech result returns `{"text": ""}`.
  Upstream error details are not returned or logged. Client retries are new model
  calls: the endpoint is not idempotent and a timeout does not prove upstream failure.
- Dictation requires `VR_TRANSCRIBE_PROVIDER=azure_speech`, a configured
  `VR_SPEECH_ENDPOINT`, and a `MAI-Transcribe-*` model, otherwise it returns `503`.
  `VR_USE_FAKE_AI=true` is supported only in the existing gated local/test mode.
  Speech authentication remains backend managed identity; no Azure key goes to Windows.

**Privacy:** audio and text are held transiently in process memory and sent only to
the configured Speech service for transcription. Dictation does not use Blob/Table
storage, queues, recording history, refiners, or Web PubSub. Responses, including
handled errors, carry `Cache-Control: no-store`. Application logs contain only safe
error categories/statuses, never dictation audio, text, provider error bodies, or tokens.
This is application-level nonpersistence, not a claim about the upstream provider's
own retention policies.

**Idle cost:** `infra/modules/apps.bicep` now keeps **one API replica warm** to remove
API scale-from-zero latency. This introduces a **fixed ongoing idle compute cost**
relative to scale-to-zero (subject to Azure billing allowances); it does not guarantee
an upstream Speech latency. Worker minimum replicas remain zero, with no change to
worker scaling or cleanup.

### Durable recordings

- **Finalization trigger.** The refinement finalization runs exactly once, after all
  expected chunk transcripts exist (guarded by an ETag-checked `finalize_enqueued`
  flag). To detect chunks that never arrive, `POST /complete` also schedules a delayed
  *watchdog* finalize message; when it fires it either completes (if by then all
  chunks are transcribed), waits (re-queued with backoff), or fails the recording with
  `missing_chunks` once the grace window elapses. Duplicate finalize deliveries are
  idempotent (deterministic `transcript_id`, state guards).
- **Chunk digest mismatch** (body does not match `Content-Digest`) returns `422`.
- Existing recording OpenAPI shapes and status codes are unchanged; dictation is an
  independent additive endpoint.

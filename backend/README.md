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

### Low-latency Windows dictation

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

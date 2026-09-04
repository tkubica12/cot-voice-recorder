# backend/ — Cloud backend

**Owner:** backend component.
**Tech:** Python 3.13 · FastAPI · uv · Azure Container Apps (min replicas 0), managed identity only.

The stateless API and workers that orchestrate the recorder: authenticate the user,
accept recording sessions and audio chunks, transcribe (Czech, `gpt-4o-transcribe`) and
refine (`gpt-5.6-luna` default / `gpt-5.6-terra`) via Azure AI Foundry, persist
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
│   ├── ai/                       # Foundry transcriber/refiner + fakes
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
Production access to Azure Storage, Web PubSub, and Azure AI Foundry uses a shared
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

- **Finalization trigger.** The refinement finalization runs exactly once, after all
  expected chunk transcripts exist (guarded by an ETag-checked `finalize_enqueued`
  flag). To detect chunks that never arrive, `POST /complete` also schedules a delayed
  *watchdog* finalize message; when it fires it either completes (if by then all
  chunks are transcribed), waits (re-queued with backoff), or fails the recording with
  `missing_chunks` once the grace window elapses. Duplicate finalize deliveries are
  idempotent (deterministic `transcript_id`, state guards).
- **Chunk digest mismatch** (body does not match `Content-Digest`) returns `422`.
- No OpenAPI shapes or status codes were changed; the above only clarifies timing the
  contract left unspecified.

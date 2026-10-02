# Architecture

This document describes the end-to-end design of **cot-voice-recorder**: how a spoken
prompt on an Android phone becomes a refined transcript on a Windows clipboard, and the
rules that keep the pipeline correct, private, and cheap.

- **Audience:** implementers of the backend, Android, and Windows components.
- **Contract:** the wire format is defined authoritatively in
  [`../openapi/voice-recorder.yaml`](../openapi/voice-recorder.yaml). This document explains
  *behavior* around that contract; where the two overlap, the OpenAPI file wins for shapes
  and status codes, and this document wins for lifecycle and semantics.

**Current default (Android 1.3.0):** durable ten-second uploads feed one continuous
MAI session. The legacy 30-second/1.5-second-overlap pipeline remains available for
older recordings and Settings rollback. Detailed lease, sample-position, replay and
measurement rules are in [the mobile streaming contract](mobile-streaming.md).

## 1. Goals and constraints

- **Single, authenticated user.** One allowlisted Google account. No multi-tenant concerns,
  but strict auth on every `/v1` route.
- **Perceived latency matters more than throughput.** The Android UI must feel instant;
  actual transcription may take seconds and runs asynchronously.
- **Cheap at rest.** The worker scales to **zero** replicas when idle. The API keeps one
  warm replica for Windows dictation; Windows notifications do not poll the backend.
- **Privacy-first.** Audio is transient, transcripts are short-lived, and notifications never
  carry private text.
- **No keys in the data plane.** Azure data-plane access uses **managed identity** only.
- **No public Storage data plane.** Storage public network access and shared-key
  authentication are disabled. Blob, Queue, and Table are reachable only through private
  endpoints from the Container Apps virtual network.

## 2. Component map

```mermaid
flowchart LR
  subgraph Phone[Android app]
    UI[Two-button UI<br/>hold / toggle]
    FS[Foreground service<br/>AudioRecord + wake lock]
    Q[(Durable chunk queue)]
    UI --> FS --> Q
  end

  subgraph VNET[Application VNet]
    subgraph Cloud[Azure Container Apps · FastAPI · API min replicas 1]
      API["REST API /v1"]
      W[Leased streaming / legacy chunk / finalize worker]
      CLEAN[Scheduled cleanup job]
    end
    PE[Blob · Queue · Table<br/>private endpoints]
  end

  subgraph Data[Azure Storage · public access disabled]
    BLOB[(Blob<br/>audio + transcript bodies)]
    QUEUE[(Queue<br/>stream / chunk / finalize work)]
    TABLE[(Table<br/>session metadata)]
  end

  SPEECH[Azure Speech<br/>MAI-Transcribe-2]
  STREAM[AI Foundry<br/>MAI-Transcribe-2-Streaming]
  FND[Azure AI Foundry<br/>gpt-6-luna; legacy 5.6-luna/terra]
  WPS[Azure Web PubSub]

  subgraph Tray[Windows tray app · .NET 8 WPF]
    LISTEN[Web PubSub client]
    CLIP[Clipboard + toast]
    HIST[History window]
  end

  Q -->|HTTPS Bearer ID token| API
  API --> PE
  PE --> BLOB
  PE --> QUEUE
  PE --> TABLE
  QUEUE --> W
  W --> BLOB
  W --> TABLE
  W --> SPEECH
  W --> STREAM
  W --> FND
  W -->|transcript.completed| WPS
  CLEAN --> BLOB
  CLEAN --> TABLE
  WPS -->|event| LISTEN --> CLIP
  LISTEN -.->|GET /v1/transcripts/id| API
  HIST -.-> API
```

## 3. End-to-end flow

### 3.1 Continuous streaming happy path

```mermaid
sequenceDiagram
  autonumber
  participant A as Android
  participant API as FastAPI
  participant ST as Private Storage
  participant WK as Leased worker
  participant AI as MAI streaming
  participant WPS as Web PubSub
  participant W as Windows tray

  A->>API: POST recording (immutable mode/layout/polishing)
  API->>ST: create recording idempotently
  API-->>A: recording_id
  loop each contiguous 10s segment
    A->>API: PUT WAV + checksum + exact source sample positions
    API->>ST: immutable audio + chunk metadata + stream wakeup
    API-->>A: durable ACK
    A->>A: delete acknowledged local file
    WK->>ST: claim/renew recording and queue leases; read contiguous prefix
    WK->>AI: append PCM to one session, no boundary commit or silence
    AI-->>WK: provisional hypothesis / delta
    WK->>ST: coalesced submitted-duration and preview snapshot
  end
  A->>API: complete (expected chunk count)
  API->>ST: durable EOF intent
  WK->>ST: verify every declared source segment is available
  WK->>AI: commit once at EOF
  AI-->>WK: authoritative full transcript
  WK->>ST: immutable raw result + owner-checked canonical pointer
  opt polishing enabled
    WK->>WK: optional recorded-model refinement
  end
  WK->>ST: final body + metadata, state=completed
  WK->>WPS: transcript.completed (stable event ID)
  WPS-->>W: completed event
  W->>API: authenticated transcript fetch
  API-->>W: full body
  W->>W: deduplicate, copy to clipboard, notify
  WK->>ST: acknowledge notification and remove retained audio/attempts
```

Disconnect/restart discards attempt text and replays retained source audio from zero.
Waiting for a missing upload does not pad silence or advance beyond a source gap.

### 3.2 Legacy chunked happy path

```mermaid
sequenceDiagram
  autonumber
  participant A as Android
  participant API as FastAPI
  participant ST as Storage (Blob/Queue/Table)
  participant WK as Worker
  participant AI as Azure Speech / AI Foundry
  participant WPS as Web PubSub
  participant W as Windows tray

  A->>API: POST /v1/recordings (client_recording_id)
  API->>ST: upsert session row (state=recording)
  API-->>A: 201/200 {recording_id, state}
  loop each 30s window (1.5s overlap)
    A->>API: PUT /v1/recordings/{id}/chunks/{index} (WAV + checksum)
    API->>ST: put blob + chunk row (idempotent)
    API->>ST: enqueue transcribe-chunk
    API-->>A: 202 {chunk_state: accepted}
    A->>A: delete local chunk after ack
    WK->>ST: dequeue transcribe-chunk
    WK->>AI: transcribe (MAI-Transcribe-2, cs)
    AI-->>WK: chunk text
    WK->>ST: store chunk text, DELETE audio blob
  end
  A->>API: POST /v1/recordings/{id}/complete (chunk_count)
  API->>ST: state=transcribing, enqueue finalize
  API-->>A: 202 {state}
  WK->>ST: dequeue finalize (all chunks transcribed?)
  WK->>WK: stitch chunks (dedupe 1.5s overlap)
  WK->>ST: state=refining
  WK->>AI: refine text (recorded model; gpt-6-luna default)
  AI-->>WK: refined transcript
  WK->>ST: store transcript body (blob), state=completed
  WK->>WPS: send transcript.completed (preview only) to user group
  WPS-->>W: transcript.completed
  W->>API: GET /v1/transcripts/{transcript_id}
  API-->>W: full body
  W->>W: copy to clipboard + toast
```

### 3.3 Cold start

Container Apps may be at **zero replicas** when the user hits record. The client design
tolerates this:

- `POST /v1/recordings` is created **asynchronously** on the Android side: the UI starts
  capturing immediately and enqueues the create request. The user never waits on the network.
- Chunk uploads retry with backoff against a durable local queue; the first request simply
  absorbs the cold-start latency.
- Because create and chunk upload are **idempotent**, retries during cold start cannot create
  duplicates or corrupt ordering.
- The ID token can expire while uploads are queued. Background workers first use a valid cached
  token and otherwise leave the step retryable while a notification asks the user to sign in.
  Credential Manager is Activity-only because its provider can show a chooser even when
  auto-select is requested; background code therefore never calls it or launches UI.
- Local audio is durable *before* it is uploaded, and it stays durable: a mid-recording capture
  error preserves every persisted chunk, schedules its upload and declares the real chunk count.
  Local data is deleted only on an explicit user cancel or when nothing was ever persisted.

## 4. State machine

A recording moves through these states (see `RecordingState` in the OpenAPI file):

```mermaid
stateDiagram-v2
  [*] --> recording
  recording --> uploading: complete requested / chunks in flight
  uploading --> transcribing: all chunks received
  recording --> transcribing: complete before some chunks (waits)
  transcribing --> refining: canonical stream ready or legacy chunks stitched
  refining --> completed: refined body stored
  recording --> failed: fatal error
  uploading --> failed
  transcribing --> failed
  refining --> failed
  completed --> [*]
  failed --> [*]
```

Semantics:

- **recording** — session exists; chunks may still arrive. Set on create.
- **uploading** — `complete` has been requested and the backend is waiting for the declared
  `chunk_count` chunks to finish arriving. Distinguishes "user stopped" from "still capturing".
- **transcribing** — transcription is running or finishing; streaming progress can also
  advance during capture before the expected count is declared.
- **refining** — finalization has the canonical stream text or stitched legacy text;
  optional refinement may run. This state can be brief when polishing is off.
- **completed** — final transcript body is durable. Notification delivery can still be
  retried against the same canonical result and stable event ID.
- **failed** — terminal error (e.g., missing chunks after grace period, model failure after
  retries). Carries a `failure_reason`.

`progress` (received / expected counts, transcribed count, submitted audio duration,
provisional preview and attempt status) is exposed on
`GET /v1/recordings/{id}` so the client can show status without WebSocket access.

## 5. Idempotency & retry rules

Every mutating operation is safe to retry. This is essential because the Android durable queue
retries aggressively and the backend can cold-start mid-request.

| Operation | Idempotency key | Retry behavior |
|-----------|-----------------|----------------|
| `POST /v1/recordings` | `client_recording_id` (client-generated UUID) | Same key returns the same `recording_id`. First call → `201`, subsequent → `200`. Different `client_recording_id` → new recording. |
| `PUT .../chunks/{index}` | `(recording_id, index)` + content `checksum` | Identical retry returns `200`; streaming retry repairs wakeup work, but an active recording lease prevents another model session. Different bytes return `409`. |
| `POST .../complete` | `(recording_id, chunk_count)` | Same count returns current state and repairs required queue work; a different declared count returns `409`. |
| Queue messages (stream/transcribe/finalize) | recording lease / persisted readiness / canonical transcript | At-least-once delivery; streaming duplicates cannot acquire an active lease. Completed finalization can resume unfinished notification/cleanup, without another transcript or LLM call. |

### 5.1 Complete / chunk race

`complete` may arrive **before** the last chunk, or a late chunk may arrive **after**
`complete`:

- `complete` records the **expected** `chunk_count` and moves the session to `uploading`
  (not `transcribing`) until exactly `chunk_count` distinct indices are present.
- A chunk arriving after `complete` with an index `< chunk_count` is accepted normally.
- A chunk with index `>= chunk_count` after `complete` → `409` (client declared fewer chunks
  than it sent; indicates a client bug).
- Legacy missing chunks fail after the grace watchdog. Streaming waits/releases/replays
  within retention instead of exhausting per-chunk attempts.

### 5.2 Legacy transcription & refinement retries

- Chunk transcription retries with exponential backoff up to a bounded attempt count; the
  audio blob is retained until a chunk **succeeds**, then deleted immediately. On terminal
  failure the chunk (and recording) go `failed` and the audio is still deleted per retention.
- Refinement retries similarly; the stitched raw text is preserved in Table/Blob so a retry
  does not need audio.

### 5.3 Queue leasing & worker resilience

- The worker receives **one message at a time** (`VR_QUEUE_BATCH_SIZE=1`). Processing is
  sequential, so a larger batch would let the visibility lease of the last message in the
  batch expire while the first is still waiting on Foundry — the message would be
  redelivered to another replica and the model call duplicated.
- The processing lease is `VR_QUEUE_VISIBILITY_SECONDS` (default **300 s**), sized to
  comfortably exceed the slowest Foundry transcription/refinement round-trip.
- Retry backoff is a separate knob, `VR_QUEUE_RETRY_BASE_SECONDS` (default 30 s, doubling
  per dequeue up to 600 s): the lease sizes one attempt, the backoff sizes the wait
  between attempts.
- The worker loop survives queue-level faults. A `receive` failure is logged with
  structured metadata (never payload) and retried after an interruptible backoff that
  still honours SIGTERM. A failed `delete` or a stale/expired pop receipt on `renew` is
  logged and tolerated: the message is simply redelivered, and both `transcribe` and
  `finalize` are idempotent.
- Streaming additionally renews the recording lease and current queue receipt every
  20 seconds. `queueLengthStrategy=all` counts invisible leased messages, so an active
  stream is not scaled away as an apparently empty queue. The recording lease fences
  canonical publication independently of queue visibility.

### 5.4 Finalize durability

`finalize_enqueued` is a reservation flag persisted with optimistic concurrency **before**
the finalize message is sent, so concurrent triggers cannot both enqueue. If the send then
fails, the flag is rolled back and the error is re-raised, so the caller retries. Every
`POST /complete` — including an idempotent replay that returns `200` — re-arms finalize or
the grace watchdog before answering, so a recording whose finalize message was never
successfully enqueued is repaired by the client's normal retry. Duplicate finalize
messages reuse the canonical transcript. Streaming notification/cleanup can resume
after `completed`; a persisted delivery acknowledgement avoids ordinary duplicate
notifications, and Windows deduplicates ambiguous at-least-once sends by event ID.

## 6. Security boundaries

```mermaid
flowchart TB
  subgraph Untrusted[Client devices]
    AND[Android]
    WIN[Windows]
  end
  subgraph Edge[Backend trust boundary]
    AUTH[Bearer Google ID token<br/>validate aud + iss + exp<br/>allowlist email]
    ROUTES["/v1 routes"]
  end
  subgraph AzureMI[Azure data plane · managed identity]
    STORAGE[(Storage)]
    WPSVC[Web PubSub]
    FOUNDRY[AI Foundry]
  end
  AND -->|ID token| AUTH
  WIN -->|ID token| AUTH
  AUTH --> ROUTES
  ROUTES -->|MI token| STORAGE
  ROUTES -->|MI token| WPSVC
  ROUTES -->|MI token| FOUNDRY
```

- **Inbound auth.** All `/v1` routes require an `Authorization: Bearer <Google ID token>`.
  The backend validates signature (Google JWKS), `iss`, `exp`, and that `aud` is one of the
  configured Google client IDs (Android + Windows), and that `email` matches the single
  allowlisted address (and `email_verified` is true). Failures return
  `401` (missing/invalid token) or `403` (valid token, not allowlisted).
- **Health probes** `GET /health/live` and `GET /health/ready` are unauthenticated and expose
  no user data.
- **Outbound auth.** The backend holds **no keys**. It uses a shared **user-assigned
  managed identity** (its client id supplied via `AZURE_CLIENT_ID` for
  `DefaultAzureCredential`) to obtain tokens for Storage (Blob/Queue/Table data-plane
  roles), Web PubSub (to mint client URIs and send events), Azure AI Foundry (Cognitive
  Services user role), and ACR image pull. The same identity is shared by all three
  runtime components so RBAC, ACR pull, and KEDA queue authorization are pre-created and
  deterministic. Those components are **three separate Azure Container Apps resources**:
  the external-ingress **API** app (one warm replica for low-latency dictation), a no-ingress **worker** app
  scaled from zero on the private Storage queue via managed identity, and a **scheduled
  cleanup job** that runs hourly — rather than running API and worker in one process.
- **Private Storage networking.** The external-ingress Container Apps environment is injected
  into a dedicated infrastructure subnet. A separate subnet hosts private endpoints for the
  Storage `blob`, `queue`, and `table` subresources. The VNet is linked to
  `privatelink.blob.core.windows.net`, `privatelink.queue.core.windows.net`, and
  `privatelink.table.core.windows.net` private DNS zones so SDK calls to the normal Storage
  host names resolve privately. Storage sets `publicNetworkAccess: Disabled`,
  `defaultAction: Deny`, and `allowSharedKeyAccess: false`.
- **Network separation.** Client traffic still enters the authenticated public HTTPS endpoint
  of the Container App; clients never receive Storage URLs or credentials. The private-endpoint
  subnet is distinct from the Container Apps delegated subnet and disables private-endpoint
  network policies as required by Azure.
- **Web PubSub scoping.** `POST /v1/realtime/negotiate` returns a **user-scoped** client
  access URI (joined to the single user's group with only the `transcript.completed` event
  type). The client connects directly; the backend never has to stay awake to relay.
- **Notification minimization.** Events contain IDs, timestamp, and a short preview; the full
  transcript is only retrievable via an authenticated `GET`.

## 7. Data lifecycle & storage design

| Data | Store | Lifetime |
|------|-------|----------|
| Streaming audio segment | Blob (content-addressed path stored in chunk metadata) | Retained through model attempts until final transcript completion; failed recordings remain bounded by retention cleanup. |
| Legacy audio WAV chunk | Blob (`audio/{recording_id}/{index}.wav`) | Deleted after independent chunk transcription or terminal failure. |
| Session metadata (state, progress, chunk rows) | Table | 48 hours after completion/failure, then cleaned. |
| Canonical raw text / attempt results | Blob/Table (internal) | Immutable stream attempt pointer fences publication; successful attempts are reclaimed after completion, raw text remains for refinement retries until cleanup. |
| Final transcript body | Blob (`transcript/{transcript_id}.txt`) | 48 hours, then deleted. |
| Queue messages | Queue | Leased/renewed during streaming, re-hidden on retry and deleted when settled. |

- **Primary deletion is inline** (after full streaming finalization, individual legacy
  transcription or at retention expiry).
- **Scheduled cleanup job** runs periodically as a *safety net*: it deletes audio blobs whose
  session no longer needs them, and any session/transcript older than 48 hours that inline
  paths missed. It is idempotent and safe to run at any time.
- Blob lifecycle management policies are a secondary backstop but are not relied upon for the
  privacy guarantee (their granularity is coarse).

## 8. Model choice rationale

- **Continuous transcription — `MAI-Transcribe-2-Streaming`.** The Realtime API consumes
  one ordered PCM source, avoiding the extra text seams from independent STT windows.
  Transport remains durable and asynchronous. Failed sessions replay retained audio;
  automatic language detection is the Android 1.3 default. The model remains preview
  without SLA; ordinary ASR errors and replay time remain.
- **Legacy transcription — `MAI-Transcribe-2` via Azure Speech Fast Transcription API
  `2025-10-15`.** Chosen after live Czech comparison against `gpt-4o-transcribe`: the
  models produced the same mean word-error rate on clean, self-correction, code-switching,
  and 12 dB noisy samples, while MAI verbatim completed about twice as fast. It runs in a
  private North Europe `AIServices` resource because Sweden Central does not currently
  support MAI transcription. `gpt-4o-transcribe` remains a configurable fallback.
  MAI-Transcribe-2 is currently an Azure public-preview feature without an SLA.
- **Optional refinement — `gpt-6-luna` (2026-09-22).** Cleans up disfluencies, fixes
  punctuation/casing, and stitches overlap seams into coherent prose while preserving meaning.
  Android sends the model selected when recording starts; Windows dictation independently
  uses the backend's `VR_REFINE_DEPLOYMENT_DEFAULT`. New Android streaming recordings
  leave polishing off unless explicitly enabled; legacy defaults are unchanged.
- **Refinement (legacy) — `gpt-5.6-luna` and `gpt-5.6-terra` (2026-07-09).**
  Still accepted for recordings already queued or created by older Android clients;
  the worker honors the stored selection rather than rewriting history.
- **Legacy two-stage rationale.** Transcription is specialized and cheap per chunk; refinement benefits
  from full context after stitching. Separating them lets each retry independently and keeps
  audio deletable as soon as raw text exists.
- **Legacy overlap handling.** 1.5 s overlap between 30 s windows avoids clipping words at boundaries;
  the stitch step dedupes the overlap using the transcribed text (and timing hints) before
  refinement. Streaming capture has no overlap; compatible overlapping input is trimmed
  by exact source samples, not by matching recognized words.

## 9. Notification path

- The backend sends `transcript.completed` to the user's Web PubSub group after the final body
  is stored. The event schema (see `TranscriptCompletedEvent` in the OpenAPI file):
  - `event` = `"transcript.completed"`, unique `event_id` (UUID) for client dedup,
  - `transcript_id`, `recording_id`,
  - `completed_at` (ISO 8601), and a short `preview` (bounded length),
  - **no full transcript body.**
- The Windows tray app maintains a persistent Web PubSub connection (renewing its access URI
  via `negotiate` as needed), dedupes on `event_id`, then fetches the body and copies it to the
  clipboard with a toast. Android may also subscribe for in-app confirmation.
- This path avoids desktop polling. The worker can scale to zero between recordings;
  the API keeps one warm replica for the separate Windows dictation path.

## 10. Tradeoffs & alternatives considered

- **Scale-to-zero vs. latency.** Async capture/upload absorbs worker cold starts, while
  one warm API replica keeps Windows dictation responsive.
- **Web PubSub vs. polling.** Direct client Web PubSub avoids the tray app polling (which would
  keep the backend warm and drain battery). Cost is an extra managed service and a negotiate
  round-trip.
- **Chunked upload vs. single-file.** Chunking gives fast perceived completion, bounded memory,
  progressive transcription, and resumability. It no longer implies independent STT:
  continuous streaming removes the added stitch/overlap seam problem.
- **Legacy two-model pipeline vs. single model.** Two stages add orchestration but allow immediate
  audio deletion and independent retries; a single multimodal call would hold audio longer and
  couple failure modes.
- **Streaming recovery vs. immediate audio deletion.** One continuous model session requires
  retaining source audio through finalization. Full replay costs time and inference again;
  it prevents a failed ephemeral session from being the only copy of the recording.
- **Table storage vs. a database.** Table storage is sufficient for single-user session
  metadata and is cheap/serverless; a relational DB would add operational weight for no benefit
  here.
- **Private endpoints vs. service endpoints.** Private endpoints are used for all three Storage
  data services because subscription policy disallows public access. This adds DNS and subnet
  resources but gives deterministic private routing and keeps the Storage firewall closed.
- **48 h retention.** Long enough to recover a missed clipboard, short enough to minimize data
  at rest. Enforced by inline deletion plus a scheduled net.

## 11. Testing strategy

| Layer | Scope | Tooling |
|-------|-------|---------|
| Backend unit | Auth/state/idempotency, exact PCM continuity, leases/replay, legacy stitch logic and publication/notification faults | pytest |
| Backend integration | Blob/Queue/Table interactions, worker flows, cleanup job | pytest + **Azurite** |
| Android JVM | Contiguous/legacy windows, Room upgrade preservation, durable queue, retry/backoff, ack-then-delete | JUnit / Robolectric |
| Android instrumented | Foreground service + AudioRecord capture, permission & wake-lock behavior | Android emulator tests |
| Windows unit | Web PubSub event handling, dedup, DPAPI token store, 48 h cache expiry, clipboard/history logic | xUnit |
| API contract | Requests/responses conform to `openapi/voice-recorder.yaml` (both directions) | schema-driven contract tests |
| Azure smoke | Real Container Apps + private Storage + Web PubSub + Foundry, managed identity, gapped/live synthetic uploads and desktop delivery | scripted smoke run |

Contract tests are the glue: each component validates its I/O against the shared OpenAPI file
so client and backend can be built independently and still interoperate.

## 12. Open questions / future work

- Grace-window duration for `missing_chunks` before failing a recording (tune from real usage).
- Whether Android should also copy the transcript locally (parity with Windows) — currently
  Windows is the primary sink.
- Real-device radio changes, locked-screen capture and battery checks for Android 1.3.

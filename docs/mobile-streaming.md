# Mobile durable upload and continuous cloud transcription

Android 1.3.0 adds a streaming recording mode without making recording depend on a
live phone WebSocket. New recordings default to streaming, automatic language
detection and no LLM polishing. Settings can restore legacy chunked transcription
or enable optional polishing. Each recording persists its own mode/layout/choice;
changing Settings never reinterprets pending older recordings.

## Capture and upload

- Streaming capture emits contiguous 10-second mono PCM16/16 kHz WAV files.
  `start_sample=index*160000`, with the exact actual sample count, is supplied in
  upload headers. The last segment can be shorter. No overlap or overlap-only
  terminal segment is generated.
- Audio and metadata are persisted before upload. WorkManager keeps the existing
  create -> upload -> complete chain, network constraint, idempotency and retry
  behavior. The phone deletes a file only after a successful durable cloud ACK.
- Capture has a bounded persistence queue. Incomplete PCM and persistence overload
  stop explicitly rather than silently dropping samples. Capture errors retain
  persisted files, drain already-queued writes and try to seal the available tail.
- This is not a lossless local audio journal: process death can lose the incomplete
  ten-second window and queued, not-yet-persisted audio. It does not protect against
  disk/device loss. Older recordings retain their 30-second/1.5-second-overlap layout.
- Room migration 2 -> 3 adds immutable recording choices and progress fields without
  deleting recordings or chunk rows. Existing rows default to legacy semantics.

## One model session, not one STT request per file

The API stores streaming audio in content-addressed immutable blob paths and wakes
the recording's queue work. A worker claims an optimistic-concurrency lease on the
recording (120 seconds) and renews it and the current queue receipt every 20 seconds.
Another worker can acknowledge duplicate wakeups, but cannot create a second active
model session. An expired owner cannot publish a canonical result.

The worker reads only the contiguous available prefix in source order, validates
WAV/checksum/sample metadata, strips WAV headers and removes only proven duplicate
source samples from legacy overlapping input. It sends 20 ms PCM packets to one
MAI-Transcribe-2-Streaming session. Transport file boundaries send no commit/reset;
the only commit is at known capture completion, after every declared segment.

Available audio is paced at up to four times real time. Source sample positions,
not wall-clock network gaps, define the audio timeline. Waiting for a later upload
does not insert zero PCM or infer silence. A missing range cannot be skipped.
Provisional text is exposed to the phone's existing recording-status poll, with
five-second coalescing that also flushes the latest snapshot while waiting for
another upload. It is never copied incrementally to the PC.

The queue scaler counts visible **and invisible** messages (`all`), so a leased,
renewed work item continues to represent activity without permanently warming the
worker. One replica runs one work item; two replicas can own different recordings.

## Failure and publication

Every audio blob remains available throughout streaming. A failed/disconnected
session, worker restart or shutdown discards the attempt's provisional transcript
and replays the recording from sample zero in a new session. No partial result
from a previous attempt is appended to the new result.

Waiting for more audio is not a terminal failure. After 120 seconds without a new
segment, the worker releases the session and re-arms queue work. Model sessions
rotate at the 55-minute wall-clock deadline. Stream failures use bounded backoff
and remain recoverable within the recording retention; they are not converted to
a dropped recording after the ordinary per-chunk retry count.

A successful final transcript is first stored at an immutable attempt path, then
published by an owner-checked recording CAS. Finalization reads that canonical
text directly instead of stitching independent STT results. LLM runs only when
the recording explicitly enabled polishing. Audio is removed only after final
transcript/metadata completion; terminal streaming errors retain audio until
retention cleanup. Expired recording cleanup also purges raw attempt blobs.

Final Web PubSub/Windows delivery remains the existing one completed-transcript
event, never a partial-text stream. Failed notification is retried against the
same canonical transcript and stable event ID; notification transport is
at-least-once and Windows deduplicates that ID. The existing Windows live-dictation proxy,
local recovery and cloud audit are unchanged.

## API compatibility and operations

`POST /v1/recordings` adds:

| Field | Legacy default | Android streaming value |
|---|---|---|
| `transcription_mode` | `chunked` | `streaming` |
| `audio_layout` | `legacy_overlap` | `contiguous` |
| `refinement_enabled` | `true` | `false` unless polishing selected |

Contiguous uploads require `X-Audio-Start-Sample` and `X-Audio-Sample-Count`.
Current-attempt progress includes submitted audio duration, provisional preview,
attempt number and a safe retry category. It is not a durable inference ACK.

Update **worker, API and cleanup job** before installing Android 1.3.0. Older clients
continue to use independent chunk transcription without changing their behavior.
The existing managed identity needs access to the configured Foundry deployment.
Storage stays private; no Google/Entra credential or model key is added to Android.

Runtime controls are `VR_RECORDING_STREAM_DEPLOYMENT`, `VR_RECORDING_STREAM_REPLAY_SPEED`
(default 4), `VR_RECORDING_STREAM_IDLE_SECONDS` (120), poll interval (1 second),
heartbeat (20 seconds), lease (120 seconds) and session deadline (3300 seconds).
Disabling streaming in Android Settings is the client rollback for new recordings.

## Evidence and limits

The fictional 49.55-second Czech/English corpus was replayed in a single session
at normal cadence, as ten-second uploads with network gaps and four-times playback,
and as a complete four-times backlog. Gapped and normal transcripts matched after
punctuation/case normalization. Accelerated replay differed in one technical spelling
and preserved checked numbers/negations; ordinary ASR mistakes remained.

The actual new worker engine then processed **30 minutes / 180 segments /
28,800,000 samples** using real MAI and local fake Storage. It finished in **461.34 s**
at approximately 3.90 times real time, renewed heartbeat 23 times, retained all 180
audio blobs until finalization, deleted them after completion, made zero LLM calls
and emitted one completion notification. The source repeated the same fictional
49.55-second clip: this tests length/state behavior, not diverse natural speech.
It is a cold-backlog duration, not the expected post-Stop delay of a healthy recording
that was already processed during capture.

The deployed API/private Storage/queue/managed-identity worker was then tested with
the 49.55-second fixture in five segments. An identical upload returned an idempotent
ACK; segment 2 arrived before segment 1; provisional text appeared before completion;
the recording completed in one model attempt with all 49,550 ms submitted and
`refine_model=none`. The final cloud transcript exactly matched the text automatically
copied by the installed Windows client. Transcript expiry was 48 hours after completion.
This run included a cold worker, deliberate waiting and a late burst: first preview
was observed after 52.02 s and completion 9.58 s after declaring EOF. These are not
healthy steady-state Stop latency figures or a promise of sub-second mobile delivery.

After the idle-snapshot flush fix, a warm-worker repetition showed 10,000 ms submitted
while segment 1 was missing (segment 2 was already uploaded), then completed all audio
in one attempt. A separate hosted run released each file only at its actual ten-second
capture boundary: the first preview was observed at 13.78 s from test startup and
the final transcript was fetched 6.92 s after declaring EOF. This includes HTTP,
queue/finalization work and two-second status polling, not just the MAI final drain.
These are single synthetic runs; mobile radio behavior and real-device timing are
still unmeasured.

Unit coverage checks exact byte concatenation, overlap removal, gaps, full replay,
exclusive lease/expired owner, idle/shutdown, queue receipts, failed raw publication,
queue-wakeup repair, immutable conflicting uploads, notification retry, legacy
endpoints, contiguous Android windows and non-destructive database migration.
Real microphone quality, locked-phone behavior, radio changes and battery remain
physical-device checks. USB debugging is needed only for convenient APK installation
and device diagnostics, not backend implementation or these synthetic tests.

MAI remains public preview without SLA. The tested acceleration is configurable,
not a service-wide speed guarantee.

# Windows dictation: design and operating contract

## Decision

Add Handy-style global push-to-talk and hands-free shortcuts to the existing tray app. Capture the Windows
default microphone locally, transcribe with **MAI-Transcribe-2 in Azure**, and paste the
complete text once when stopped. This is local capture, **not offline transcription**.
Android's durable recording/cleanup workflow remains separate and unchanged.

The desktop sends short WAV requests to `POST /v1/dictation/transcribe`, authenticated
with the same Google ID token as the existing API. The API invokes MAI directly using its
user-assigned managed identity and private Speech endpoint. No Blob, Table, Queue,
worker, refinement model, or Web PubSub operation is on the dictation critical path.
Successful dictation text is saved to Windows' existing 48-hour history, not cloud history.

### Why this path

| Option | Decision |
|---|---|
| Existing Android recording pipeline | Keep for Android, not interactive dictation. Storage, queuing, worker startup and final LLM cleanup add latency. |
| Windows directly to Speech | Avoid for this deployment: Speech is private, Windows has Google rather than Azure identity, and distributing resource keys or exposing the endpoint would weaken the existing boundary. |
| Short requests through the API | Implemented. Reuses auth and private networking; transcription runs while capture continues. |
| Voice Live websocket | Possible future alternative, not assumed equivalent to MAI-Transcribe-2 Fast Transcription. Current language documentation says the `mai-transcribe` alias defaults to 1.5. It adds a different session/model configuration to deploy and measure. |
| ACA Express in Sweden Central | Considered: the current feature matrix lists Sweden Central, VNet egress and user-assigned identities, including ACR pulls. Older FAQ sections conflict with that matrix. It is suitable to evaluate for the HTTP API, not the queue worker/cleanup job. The owner selected standard ACA with one warm API replica for this release. |

The standard API has `minReplicas: 1` to avoid ordinary idle scale-to-zero cold starts.
This adds idle compute cost. It does not eliminate deployment/restart delay, first-use
identity/JWKS fetches, network latency, or MAI inference time. The worker still scales to zero.
Express's advertised subsecond platform startup is not an end-to-end transcription guarantee.

## Audio and latency

- Start capture immediately on the shortcut; do not await cloud requests first.
- Capture PCM16, 16 kHz, mono, using 40 ms microphone buffers and 20 ms analysis frames.
- Use an inexpensive RMS gate to suppress silence-only uploads, retaining approximately
  180 ms of pre-roll. This is an energy gate, not a neural speech detector; very quiet speech
  or background noise can be misclassified.
- After at least 2 seconds, a 400 ms quiet interval ends a chunk. Continuous speech
  is cut at 6 seconds with 600 ms overlap. Stop flushes remaining speech immediately,
  even when it is shorter than 2 seconds. Never upload an overlap-only tail.
- Run at most two transcription requests concurrently. Pending chunks live in an encrypted
  disk journal rather than an unbounded in-memory queue. The microphone-to-checkpoint queue
  holds at most 50 buffers (about two seconds); disk overload stops explicitly and retains the
  last durable checkpoint rather than dropping samples silently.
- Order responses by capture order. Deduplicate punctuation/case-normalized suffix/prefix
  words **only at overlapping boundaries**, preserving repetitions at normal pause boundaries.
  Version 1.3.1 also matches single short words (such as "to", "that", or "go"); earlier
  versions intentionally kept one-word matches shorter than eight characters. Matching is
  limited to the immediately preceding chunk; an empty result breaks that adjacency.
  Repetitions inside a chunk are not sanitized. This heuristic cannot guarantee a perfect
  seam when the model recognizes overlap differently, and can remove an intentional repeat
  that happens to match across an overlapping boundary. It does not correct recognition
  mistakes or add an LLM/network request.
- Use MAI's `clean` style with automatic multilingual recognition by default. Czech and English
  can be forced in Settings. There is no separate LLM cleanup round trip.
- Each desktop request has a 25-second timeout and at most one transient retry, plus the
  existing one-time token refresh on 401. The API has a 20-second response deadline, four
  model-call slots per process and no hidden model retry. Timeout slots remain occupied
  until the underlying synchronous request really finishes.
- No fixed five-minute recording limit. Checkpoint every two seconds of captured audio and
  whenever a chunk is sealed, without cutting/restarting capture. The checkpoint includes the
  exact partial frame, pre-roll and overlap state, so restarting does not create new seams.
- Transient cloud failures back off while capture/checkpointing continues. Authentication or
  configuration failures pause uploads until explicit recovery. Release/stop waits at most
  30 seconds for remaining results; if still pending, retain the session for Recovery, never
  paste an incomplete transcript as though it were finished.

Stop-to-text latency is the remaining work for any unfinished chunks, usually the final
short chunk rather than the complete recording. Stop-to-delivery additionally includes
history persistence, clipboard availability and waiting for the shortcut's modifier keys
to be released. No fixed cloud latency guarantee is claimed.

## Desktop behavior and safeguards

Default shortcuts: hold **Ctrl+Alt+Space** while speaking; release to finish and paste.
Press **Ctrl+Alt+Shift+Space** once to start hands-free, and again to stop. Release does not
stop hands-free capture. The hold shortcut does not take over an active hands-free session
and the toggle shortcut does not take over a hold-to-talk session. **Escape** discards
while recording or transcribing. Settings can disable dictation or change both shortcuts/language.
Shortcut collisions are reported rather than ignored. The temporary Escape binding is released
as soon as the session finishes.
Each shortcut press is latched until the trigger or a required modifier is released,
independently of Windows' `MOD_NOREPEAT`. Holding the shortcut must not repeatedly start/stop
sessions. Release detection samples the chord every 20 ms only while a shortcut is held;
there is no fixed cooldown. Queued messages for an already released chord are ignored.

The fixed 440 by 142 DIP indicator is topmost, click-through, `WS_EX_NOACTIVATE`, and absent
from the taskbar. It does not take keyboard focus. "Listening" stays stable while transcription
runs concurrently; the latest approximately 30 words (bounded to 220 Unicode code points)
appear only as the contiguous, ordered transcript advances. Later results cannot jump ahead
of missing earlier chunks. Separate saved-audio and transcribed-through timestamps distinguish
recognition from durable capture. "Finishing" and a brief "Pasted"/"Saved - paste skipped"
status complete the interaction. There is no incremental clipboard paste.

Hold-to-talk captures the foreground HWND, process/thread and native focused child at start,
then checks for changes during capture and before paste. Hands-free allows focus changes while
recording and instead selects the destination at **stop**; keep that input focused while
finishing. It never activates another window. Selecting the wrong input at stop can paste
into that input; it is not guaranteed to "fail safely" merely because it was not your intended
destination.
If focus changes, modifiers remain held, the clipboard changes, or Windows rejects input
(for example an elevated target), it skips automatic paste and reports the clipboard/history
fallback. Applications with multiple custom-drawn editors sharing one native focus HWND
cannot always be distinguished; keep the intended input focused.

Paste is a single Ctrl+V, never Enter or submit. The transcript remains on the clipboard;
the previous clipboard is deliberately not restored because delayed restoration can overwrite
a subsequent user copy. If clipboard access fails, recover from History. Background Android
auto-copy is suppressed during dictation so it does not normally replace dictation text.

Explicit Escape stops and discards the journal. Sign-out, Windows lock/disconnect, suspend and
normal exit stop without paste and preserve recovery. Failed sessions never paste partial chunks.
In-flight cloud requests already sent cannot be recalled by cancelling locally.

## Durable recovery and privacy

`%LOCALAPPDATA%\VoicePrompt\dictation-recovery` contains current-user DPAPI-encrypted WAV
chunks and an encrypted manifest containing ordered recognized text, capture state, language
and account/backend binding. New audio files are durably flushed before atomically replacing
the matching manifest. Results are committed before their audio is removed. Already recognized
chunks are not retranscribed on recovery. The full original audio is not archived after recognition.
Silence gating also retains only its short pre-roll, not an archive of discarded quiet audio.
The saved cursor describes a recoverable processing checkpoint, not a lossless recording of
every microphone sample; quiet speech misclassified as silence can still be missed.

The journal is retained until successful History persistence/delivery, explicit discard, or
48-hour expiry; cleanup runs at startup and periodically while idle. The recovery store has a
256 MiB quota. Disk/quota/decryption failures surface explicitly. A process crash can lose
audio since the last completed checkpoint (normally about two seconds, plus queued capture);
disk failure or an OS forcibly terminating during shutdown can prevent a final checkpoint.
This is not protection against disk loss or loss of the Windows user encryption keys.
Completed History remains the existing local text cache, not DPAPI-encrypted.

Open **Recovery**, select a session, and choose **Recover to History**. The original signed-in
account and backend are required before uploading pending audio. Recovery never chooses a
paste target or writes the clipboard automatically; select the result in History and Copy.
Recovery continues while chunks are completing; 30 seconds without progress leaves the
remaining work saved for a later retry. Escape stops recovery without deleting its saved data.
Corrupt/unreadable entries remain visible for explicit discard rather than silently disappearing.
History IDs are derived from the recovery ID so retrying finalization replaces the same item.

## Optional LLM polishing (not enabled)

Windows dictation uses MAI's `clean` transcription style and local overlap deduplication, not
the Android worker's separate refinement model. A second LLM can improve fillers, repetitions
and punctuation, but can also alter names, numbers, code or meaning; seeing full context does
not guarantee perfect correction. Streaming tokens alone does not make a full five-minute
rewrite complete within one or two seconds.

Before adding this to the fast path, benchmark MAI-only against background, sentence-aware
polishing windows (for example 30-60 seconds of new text with preceding context and a small
uncommitted tail). Preserve the raw transcript, allow a strict stop-time budget with raw-text
fallback, and never replace text after it has been pasted. Compare end-to-end stop-to-ready
p50/p95, corrections versus meaning-changing edits, Czech/English switching, names/numbers,
and short versus five-minute recordings. These are proposed experiments, not measured model
latency or a promise that a particular small model can meet the budget.

## Verification

```powershell
dotnet test windows\tests\VoicePrompt.Core.Tests\VoicePrompt.Core.Tests.csproj -c Release
dotnet run --project windows\tools\DictationProbe\DictationProbe.csproj -c Release
dotnet run --project windows\tools\DictationProbe\DictationProbe.csproj -c Release -- --microphone
dotnet run --project windows\tools\DictationProbe\DictationProbe.csproj -c Release -- --live <synthetic.wav>
dotnet run --project windows\tools\DictationProbe\DictationProbe.csproj -c Release -- --editor-live <synthetic.wav>
```

Core tests cover actual WAV bytes, irregular buffer boundaries, silence, short utterances,
hard-cut overlap reconstruction, final flush, concurrency, out-of-order responses, overload,
cancellation, failure handling, seam deduplication, modifier/focus/clipboard delivery policy,
binary upload replay after 401, settings compatibility and backend URL changes.
Recovery tests additionally simulate 20 minutes of capture with blocked cloud requests,
reopen durable journals after disposal, verify exact audio across checkpoint boundaries,
exercise out-of-order preview, retries, quota failures, encryption/corruption/retention and
Unicode preview limits. Simulated capture runs faster than real time; it does not establish
real-cloud throughput or accuracy.

The native probe checks real hotkey registration/reconfiguration, indicator HWND styles and
focus preservation, and actual x64 INPUT marshalling size. It enumerates but does not record
microphones or send global paste input. It also pumps the real WPF dispatcher and drives the
actual dictation controller with repeated native hotkey messages, worker-thread audio callbacks,
and controlled transcription/delivery. It checks that a held start/stop shortcut produces
one session and one delivery on release (including modifier release), cancellation never pastes, silence reports once, changed focus
falls back, and cloud failure does not paste partial results. It also checks hands-free release
behavior, destination selection at stop, encrypted recovery, interrupted capture, and
account/backend binding. The separate `--microphone` probe opens the default
microphone for 240 ms, verifies capture and stop, and discards all samples without upload or
disk storage. The opt-in live probe uses the installed app's normal
Google credentials without printing tokens, sends a synthetic PCM WAV, verifies recognized
keywords, and reports both request times and paced-capture stop-to-text latency. The sample
must contain the words "dictation" and "cloud". It does not paste or alter transcript history.
The opt-in `--editor-live` probe adds the real controller, cloud endpoint, clipboard and
`SendInput` path. Bring its dedicated editor to the foreground and click its run button;
leave it focused until the on-screen result appears. It replays synthetic capture, verifies
the editor contains exactly one transcript after its prefix, never sends Enter, and uses
isolated temporary history instead of the user's history.

### Long-session validation

The full Windows core suite passed **297 tests**, including an accelerated 20-minute offline
capture followed by ordered recovery. Native dual-shortcut, controller, interruption and
recovery checks passed. A 12-second paced synthetic capture exercised real Windows DPAPI
checkpoints and live preview without the checkpoint writer falling behind; one recorded run
completed 182 ms after stop. This uses controlled transcription and a test clipboard, **not
a measured live MAI response or real foreground paste latency**. The desktop build completed
without warnings or errors. A local Windows 1.3.0 installer was subsequently built and
installed successfully, preserving existing settings and OAuth configuration. Version
1.3.1 packages these changes together with the short-word overlap fix. The Android APK
distributed alongside it remains the unchanged signed 1.2.0 build.
Release 1.3.1 passed the expanded **309-test** core suite and the native/controller probes,
including short-word stitching in preview, final text and reopened recovery results.

Manual end-to-end acceptance: focus an ordinary text editor, hold the shortcut, speak Czech/English,
release, verify text once; repeat with Escape and with another window selected, checking
that neither case pastes into the wrong application. Microphone disconnect and an elevated
target should result in a clear failure/fallback, never a success notification.

### Observed validation (2026-09-15)

VoicePrompt 1.1.0 was installed and restarted successfully. The standard ACA API revision
`ca-api--0000006` runs image `dictation-20260915-095616`, with one warm replica and all
API traffic. Readiness returned 200; anonymous dictation returned 401. Worker, cleanup,
networking and access policies were not changed.

- Windows core: 246 passed; native hotkey/indicator checks passed. Default microphone
  capture and stop were verified separately, with captured samples discarded locally.
- Backend: 258 passed, one Azurite-dependent test skipped.
- Authenticated cloud probe: 7.11 seconds of synthetic English speech, mono 16 kHz PCM16,
  split into two chunks; recognized keywords verified on all runs.
- Three immediate two-chunk runs took 2,820 ms, 786 ms and 613 ms end-to-end. Individual
  request times were 2,603/2,714 ms, 537/782 ms and 606/611 ms, respectively.
- With the same audio paced at the microphone's 40 ms cadence, stop-to-text was **361 ms**.

These are a small sample from this deployment, not a percentile or latency guarantee.
The first run includes first-use client/server overhead; no individual cause was isolated.
Stop-to-text excludes clipboard/history delivery. The live probe does not paste; complete
human microphone-to-editor acceptance remains a manual check.

The initial 1.1.0 validation missed repeated shortcut messages. After a reported flashing
Listening/Transcribing indicator and notification storm, the installed binary reproduced
40 toggles from a 40-message burst. Version 1.1.1 reduces the same burst to one toggle and
adds the controller regression scenarios above. Capture logs now include stop duration,
PCM byte count and emitted chunk count, without recording audio or transcript contents.
Foreground acceptance also exposed globalization-invariant mode rejecting Windows input
culture 1033. In 1.1.2 the WPF application and native probe explicitly enable normal globalization;
the automated probe exercises Windows input-language and Unicode text data support.
The real cloud-to-editor test subsequently passed: one exact paste, no Enter, and 657 ms
from stop to the final editor check (including a deliberate 200 ms input-processing wait).
The owner also confirmed a stable indicator on real microphone input.

Version 1.2 replaces the original toggle interaction with hold-to-record/release-to-paste.
Automatic detection remains enabled (`language=auto`, no locale hint to MAI).
The owner's reported Czech-to-English output is not explained by a forced-English setting;
live language-preservation investigation is separate from the successful paste test.

## Rollback

Original source commit: `aa6282ca01924da30524828e23beae343069845c`.
Local rollback tag: `pre-windows-dictation-20260915`. The tag is not automatically pushed.
Changes are developed directly on `main`; no automatic commit is made.

Pre-dictation Azure API image:
`crcotvrspddxkti.azurecr.io/voice-recorder-backend:mai-transcribe-2-default-v2`,
with API minimum replicas zero. Restoring that image disables the new endpoint; restore
the matching older desktop build or disable dictation when doing so. Do not reset a dirty
working tree to roll back: preserve desired uncommitted work and use the tag for a separate
build or a reviewed revert.

## Sources (checked 2026-09-15)

- [Handy workflow](https://github.com/cjpais/Handy): shortcut, local recording, transcription, paste.
  This implementation reuses the existing C# app; it does not copy Handy's implementation.
- [MAI-Transcribe](https://learn.microsoft.com/azure/ai-services/speech-service/mai-transcribe):
  Fast Transcription, MAI-Transcribe-2, clean style, automatic language detection, preview status.
- [Voice Live language support](https://learn.microsoft.com/azure/ai-services/speech-service/voice-live-language-support):
  `mai-transcribe` default model qualification.
- [ACA Express feature matrix](https://learn.microsoft.com/azure/container-apps/express-overview):
  current capabilities and Sweden Central.
- [Express FAQ](https://learn.microsoft.com/azure/container-apps/express-faq):
  startup claims, preview/no-SLA and migration discussion; older feature/region sections are stale
  relative to the current overview.

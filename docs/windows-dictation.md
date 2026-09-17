# Windows dictation: design and operating contract

## Decision

Add Handy-style global push-to-talk and hands-free shortcuts to the existing tray app. Capture the Windows
default microphone locally, transcribe with **MAI-Transcribe-2 in Azure**, and paste the
complete text once when stopped. This is local capture, **not offline transcription**.
Android's durable recording/cleanup workflow remains separate and unchanged.

The desktop sends short WAV requests to `POST /v1/dictation/transcribe`, authenticated
with the same Google ID token as the existing API. The API invokes MAI directly using its
user-assigned managed identity and private Speech endpoint. No Blob, Table, Queue,
worker or Web PubSub operation is on the dictation critical path. Optional text polishing
uses a separate bounded request to `POST /v1/dictation/refine` while capture continues;
it is disabled by default and requires the updated API and desktop build.
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
  Without optional AI polishing, repetitions inside a chunk are not sanitized. This heuristic cannot guarantee a perfect
  seam when the model recognizes overlap differently, and can remove an intentional repeat
  that happens to match across an overlapping boundary. It does not correct recognition
  mistakes or add an LLM/network request.
- Use MAI's `clean` style with automatic multilingual recognition by default. Czech and English
  can be forced in Settings. There is no separate LLM cleanup round trip by default.
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

## Optional LLM polishing (off by default)

Windows 1.4.2 includes **Polish dictation with GPT-5.6 Luna** in Settings.
It is opt-in and was not included in the 1.3.1 installer. With it disabled,
Windows uses only MAI's `clean` transcription style and local overlap deduplication.
The Android worker's refinement pipeline is unchanged. A second LLM can improve fillers, repetitions
and punctuation, but can also alter names, numbers, code or meaning; seeing full context does
not guarantee perfect correction. Streaming tokens alone does not make a full five-minute
rewrite complete within one or two seconds.

### Measured Luna comparison (2026-09-16)

Actual Azure inference was measured with deployment `gpt-5.6-luna`, returned model
`gpt-5.6-luna-2026-07-09`, API version `2024-10-21`, `reasoning_effort=none`,
streaming usage enabled, two concurrent requests, and no SDK retries. Reported reasoning
tokens were zero. The existing resource is in Sweden Central with GlobalStandard deployment;
this is not a claim that inference was region-pinned. Local Entra authentication was warmed
separately. Production configuration, credentials and content filters were not changed.

The fictional Czech/English corpus contains 146, 732 and 2,973 words for nominal 1/5/20-minute
profiles, approximately 146-149 words/minute. This is **text cleanup, not an audio/MAI accuracy
benchmark**. Direct Windows-to-Foundry request timings exclude the production API hop, MAI's
remaining transcription, clipboard and UI delivery. No private recording was uploaded.

| Strategy | 1-minute profile | 5-minute profile | 20-minute profile |
|---|---:|---:|---:|
| Rewrite all text after stop | 2.69 s | 9.94 s | 34.79 s |
| Return exact edits for all text after stop | 1.63 s | 2.37 s | 3.81 s valid; another response rejected |
| Rewrite 30-second blocks during recording | 1.64 s | 2.28 s | 2.70 s, with raw fallback |
| Rewrite 6-second fragments with lookahead | 3.86 s | 1.62 s | 1.61 s, with raw fallback |
| Return exact edits for 30-second blocks during recording | 1.27 s | 1.61 s, with raw fallback | 1.65 s, with raw fallback |

The first two rows are **measured complete-request times**, medians of three samples per
length except the long edit-patch case. Whole-rewrite ranges were 2.64-3.24, 9.02-10.45 and
34.34-35.95 seconds. Long patches produced one valid response in 3.81 seconds and one
invalid response in 5.86 seconds; the third case was excluded as described below.
The first token of the long rewrites arrived in 1.36-1.47 seconds, but the complete
transcript took roughly 35 seconds. Streaming the first token is not fast completion.

The background rows are **simulated stop-to-ready times using actual per-request durations**,
not real 20-minute microphone trials. Arrivals are spaced by 30 or 6 seconds, with two model
slots; lookahead releases two final jobs at stop. There was no simulated queue backlog.
Adding an assumed one-second MAI arrival lag adds one second to these estimates; that
assumption is not a new MAI measurement. Raw fallback means retaining an original block
when refinement failed, was excluded, or returned an invalid edit, not losing that block.
Excluded requests are assigned zero processing time in this fallback simulation.
These prefix samples are not independent repeated long-session trials or a reliable p95.

**Quality changed the recommendation.** Short-fragment rewriting was fast but unsafe to
adopt as tested: it echoed read-only context into several outputs, creating new duplicated
passages. The assembled long output contained 52 excess repeated eight-word windows
(overlapping diagnostics, not 52 independent incidents) and an unexpected CJK insertion.
This happened despite explicit instructions to edit only the current fragment. A real-paced
60-second text-arrival replay completed 1.86 seconds after stop, confirming the timing
approach but not rescuing that strategy's observed quality problems.

The incremental edit-patch follow-up measured 37 model responses: median 1.36 seconds,
range 0.96-3.25 seconds, descriptive nearest-rank p95 2.30 seconds across different blocks.
Of those, **35 passed exact-anchor validation and two were rejected** because the model
invented `H m` where the source contained `Hm`. Three quote-containing context blocks were
excluded. With original-text fallback for those five blocks, the assembled output preserved
all protected literals, removed 35 of the 40 planted filler groups, and introduced no
detected long duplicates or new scripts. All returned patches were manually inspected;
the three explicit numerical corrections and the legitimate `had had` / `that that`
examples remained intact. This is a small, deliberately constructed corpus, not proof of
general ASR-error correction or guaranteed meaning preservation. A separate, real-paced
60-second patch-block replay then completed **1.98 seconds after stop**, with both patches
passing validation; its earlier background call took 3.10 seconds without delaying capture.
This measures actual wall-clock scheduling and model responses, still not microphone-to-paste.

Whole rewrites also illustrate why literal diagnostics require inspection: `16000 Hz`
became `16 000 Hz`, and quote/capitalization changes triggered a literal check without
removing the negation. Block rewriting translated the technical word `exception` to its Czech
equivalent. These are not invented numerical values, but exact technical spelling and
mixed-language preservation need stronger controls if that is the desired contract.

**Provider failure is a normal fallback path.** Five short-fragment requests were rejected
with Azure's `content_filter` policy error around a quoted fictional instruction. No filter
was disabled, blocked input retried, or content disguised. The initial benchmark stopped
at its five-failure guard, then resumed only remaining safe cases with the same prompts.
Three remaining standard-matrix cases and three incremental-patch cases containing that
context were explicitly excluded and reported, not counted as successful refinements.
The initial abort could have allowed up to two already-started calls to finish without
recording their results; subsequent harness runs drain and retain in-flight results.
All reported samples come from persisted responses, not inferred results of those calls.

The standard matrix recorded 258 requests (five rejected), and the patch-block follow-up
recorded 38 including warmup; paced trials are stored separately. Nested prefixes and
repeated prompts produced cache hits: the two matrices reported 230,768 input tokens,
42,088 output tokens and 28,225 cached input tokens, excluding paced trials and the initial
access probe. These are usage observations, not a dollar-cost estimate or an uncached
latency guarantee.

The reproducible harness and offline safeguards are in
`backend\tools\benchmark_refinement.py` and `backend\tests\test_refinement_benchmark.py`;
the synthetic corpus is `backend\tests\fixtures\refinement_benchmark.json`.
See [benchmark commands](../backend/README.md#opt-in-dictation-refinement-benchmark).
Full request records, prompts, model outputs, assembled transcripts and summaries for this
run are retained in the session artifacts `llm-benchmark-01` and `llm-benchmark-patch30`.

### Implemented integration

The desktop uses **background edit patches**, not a full rewrite at stop or unrestricted
six-second rewriting. MAI's short audio chunks and immediate preview stay unchanged.
The polisher operates on overlapping text windows spanning multiple audio chunks. It
dispatches on sufficient new text or 12 seconds of pending-text age, rather than waiting
only for a word threshold. The normal window target is roughly 75 words (about 30 seconds,
depending on speaking rate); the last 15 raw words remain editable in the next window.
Each request is capped at 4,000 characters and carries up to 150 preceding raw words
(at most 8,000 characters) as read-only context. JSON requests are capped at 32 KiB.
Only one physical request runs at a time, with no queue of stale snapshots. Its result
is tied to exact source-text positions, so incoming text is never replaced accidentally.
A 25-second desktop request deadline keeps the previous available edits and original text
on failure. The backend response deadline and provider HTTP timeout are 20 seconds, leaving
five seconds of client-side transport headroom. These background limits do not add a final
AI wait. The physical slot stays occupied until an uncooperative call actually exits.
When catching up after a stall, the latest window also has a 30-second raw-text arrival-age
limit. Skipped unprocessed older text stays verbatim and is reported as a real backlog
fallback; aging already processed overlap is not an error. Accepted edits persist across
empty later edit responses. An explicit intersecting edit supersedes an older correction.

The API asks Luna for exact replacement edits, not a new transcript. It applies them only
against the immutable source block with unique, nonoverlapping exact anchors, without fuzzy
repair. All edits are validated atomically; invalid anchors, service rejection, timeout,
or suspicious changes to literals, scripts or repeated passages produce an explicit error,
and the desktop keeps the original text or previously accepted edits. The response includes
both assembled text and the exact validated original/replacement edits, so the desktop can
retain edit provenance across rolling windows. Ordinary spaces may change when removing
unintentional duplicate words or fillers; paragraph and boundary whitespace is protected.
Structural validation does not prove semantic correctness. Earlier committed context is
not retrospectively rewritten when a correction arrives much later; preserve the explicit
correction and history instead. If ASR changes an already published prefix, the desktop
invalidates earlier edits and keeps that session raw rather than mixing text revisions.

At stop, **no new AI request is scheduled and there is no final AI wait**. Already running
work may finish while the remaining MAI audio is transcribed, but cannot launch another
request. Raw MAI text is written to History first. The result is then frozen immediately,
combining available edits with original text elsewhere. There is one final paste and no
late clipboard/history replacement. Remaining MAI work, local storage, clipboard access
and modifier-key release still take time; this does not promise fully polished output.

The primary prompt objective is removing clear accidental repetitions across the text
window and its preceding context. It preserves intentional emphasis/counting, prohibits
stylistic rewrites and guessing missing content, and permits only unambiguous ASR fixes.
This cannot reconstruct words the speech recognizer omitted.

The compact, non-activating overlay shows only the stable recording/finishing state and a
three-line recent-text preview. Version 1.4.2 removes saved/transcribed/pending counters and
AI diagnostic details from the popup; diagnostics remain in logs and History, and actionable
failures still produce notifications. A normal unprocessed ending, or a short recording
that never dispatched AI, is not counted as a failure and never produces a warning.
Version 1.4.3 also suppresses notifications for real optional-AI request/validation failures,
timeouts and window-limit fallbacks. Each fallback writes a sanitized reason to the diagnostic
log, and History retains the fallback count and original text. Earlier accepted edits and raw
text elsewhere are still delivered normally. Transcription, storage and clipboard/paste failures
continue to produce actionable notifications; this is quiet optional cleanup, not hidden data loss.
History saves the assembled and raw text together, does not claim the entire transcript was
checked, and exposes **Copy original**. Both versions share the existing
unencrypted 48-hour/200-entry cache. Intermediate AI edits exist only in memory; the encrypted
recovery journal always preserves raw ASR data. Explicit Recovery restores raw text without
AI calls or automatic paste. Escape during capture or finishing discards the temporary raw
History entry as well as the journal; interruption/sign-out keeps recoverable raw data.

Calls stay behind the existing authenticated API and managed identity. The benchmark's
developer Azure CLI access is not a desktop authentication architecture; do not distribute
Foundry keys or add a queue/worker to the interactive path. The API performs no transcript
storage for this route. Model/transient failures are not retried by either client or API;
the desktop retains its one-time 401 token refresh. Already-sent cloud requests cannot be
recalled, but cancelled or late results cannot affect delivery. **Deployment, release and
installation are separate from this source implementation.**

## Verification

```powershell
dotnet test windows\tests\VoicePrompt.Core.Tests\VoicePrompt.Core.Tests.csproj -c Release
dotnet run --project windows\tools\DictationProbe\DictationProbe.csproj -c Release
dotnet run --project windows\tools\DictationProbe\DictationProbe.csproj -c Release -- --microphone
dotnet run --project windows\tools\DictationProbe\DictationProbe.csproj -c Release -- --live <synthetic.wav>
dotnet run --project windows\tools\DictationProbe\DictationProbe.csproj -c Release -- --refine-live
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

### Background-only rolling-window validation (2026-09-16)

The Windows core suite passed 377 tests before the final empty-overlap retention adjustment;
all 48 rolling-polisher and twenty-minute integration cases passed after that adjustment.
The twenty-minute case uses virtual time, synthetic six-second ASR arrivals and the actual
typed HTTP client with a deterministic patch server. It verifies ordered content, bounded
requests, retained corrections and no request or wait at Stop; it is not an ASR accuracy
benchmark or a twenty-minute live model recording.

Native/controller probes passed short-dictation raw-only behavior without warning,
background edits while Listening, one final History-backed paste, no new request at Stop,
ignored late results, genuine error reporting, Escape/account interruption, raw Recovery
and History disk failures. With a deliberately stalled LLM, controlled stop-to-completion
was 366 ms, including remaining synthetic transcription and local persistence. The separate
12-second real-paced encrypted checkpoint scenario completed 254 ms after Stop. Neither
measurement is a promise about actual microphone or network latency.

### Quiet cleanup deployment (2026-09-17)

The 1.4.3 API revision `ca-api--quiet-143-20260917` is healthy with 100% traffic.
Its source-only image is
`crcotvrspddxkti.azurecr.io/voice-recorder-backend@sha256:63b5695b0d911d7e62ac7cc77c7657ee2c3e8dd591ad3ac585f15861545b46be`.
It reuses the previous locked runtime without dependency, worker, identity or network changes.
Rollback uses the preceding digest
`sha256:cc2b24f540a592a1f03d8aeee46ff95f16a831e73ac34586b4988e551568d9eb`.
Readiness returned 200 and anonymous refinement returned 401.

The Windows core suite passed 379 tests and the backend suite passed 439 tests.
Native controller scenarios verified quiet network/502/invalid-edit/timeout fallback, one
History-backed paste, and preserved actionable storage/paste errors. The controlled stalled
LLM scenario finished 323 ms after Stop with the longer deadline. An actual deployed synthetic
Czech/English check took 4,382/2,440 ms, and a real background result was frozen in 1 ms
without a final wait. These samples do not establish production latency percentiles or explain
historical provider errors. No private recordings or History were resubmitted.

### Rolling deployment and local installation (2026-09-16)

Windows **1.4.1** was installed locally after approval, preserving settings and Desktop OAuth
configuration. The installed executable and application/core DLLs match the published build;
the restarted tray process was responsive. The user's existing AI-enabled setting was retained.
The public-installer SHA-256 is
`a978711ac6f5acdc6a55fafad3ebf10a9b8721d387222da6397b13313e7a3fe3`.
This installation is not a new GitHub release.

API revision `ca-api--rolling-141-20260916` is healthy, with one replica and 100% traffic.
The source-only image reuses the previous locked runtime without dependency changes:
`crcotvrspddxkti.azurecr.io/voice-recorder-backend@sha256:cc2b24f540a592a1f03d8aeee46ff95f16a831e73ac34586b4988e551568d9eb`.
The prior API rollback image is
`crcotvrspddxkti.azurecr.io/voice-recorder-backend@sha256:aa95893f17309322db7b7f6c73b9dec1a40fab9629db146bff5a980cc91cb87b`.
Readiness returned 200; anonymous refinement still returned 401.

The final backend suite passed **438 tests**, with three opt-in live tests skipped;
Ruff and mypy passed. Separate real-Luna probes removed a synthetic duplicate phrase in
both Czech and English while preserving port 8443. The deployed Windows-client rerun took
2,428/2,432 ms respectively. A separate real background response completed before Stop;
final assembly took **2 ms**, with one successful window, ten raw tail words and zero
fallback errors. An earlier oversized single-snapshot probe correctly reported a backlog
fallback; the final probe uses a bounded first window and verifies no unexpected fallback.

These are small synthetic text checks, not microphone/MAI accuracy guarantees. A stricter
Czech repetition spanning read-only previous context and current text was intermittent in
the backend live evaluation. Keeping overlap editable reduces reliance on that case, but
does not guarantee every repetition is removed. See the backend README for the live test
commands and limitations. No private user recordings or History were resubmitted.

### Earlier two-second-budget validation (superseded by background-only scheduling)

The added core tests cover block boundaries, bounded raw context, concurrency, queue overload,
request timeout, one total finishing deadline, cancellation-ignoring delegates, immutable
completion and changed-prefix fallback. API/client tests cover payload bounds, authentication,
no transient model retries, invalid results and raw-history persistence. Native/controller
probes additionally exercise raw History before the final AI response, fallback warnings,
two-second finishing, ignored late results, explicit discard and raw-only Recovery, using
controlled transcription/refinement and a test clipboard. These checks do not substitute
for human acceptance of AI meaning preservation on real microphone input.
The current implementation passed **355 Windows core tests** and the native/controller
probe, including starting cleanup before capture stops and injected disk failures before
and after the AI response. A controlled cancellation-ignoring request finished with raw
fallback in 2.34 seconds from stop (including final capture/ASR and probe polling overhead);
the independent AI waiting budget is two seconds. The Windows app/probe build succeeded.
The backend suite passed **399 tests**, with two opt-in/environment-dependent tests skipped;
Ruff and mypy passed. A separate opt-in local-API test called actual
`gpt-5.6-luna-2026-07-09`: Czech completed in **6.925 seconds**, English in **1.977 seconds**,
both HTTP 200 with zero reasoning tokens. The English sample's spelling was repaired.
These are two local-API samples with developer Azure authentication, not deployed
microphone-to-paste measurements. Cold authentication/model latency can exceed the desktop's
final two-second budget, so a short dictation may legitimately keep its original text.
Background requests can finish earlier while the user is still speaking. Source validation
was followed by the separately approved deployment and installation below.

### Deployed polishing and local installation (2026-09-16)

Windows **1.4.0** was installed locally, preserving the settings file and existing OAuth
configuration. The installed executable and application/core assemblies match the published
build, and the tray process was restarted. AI cleanup remains **off by default**; enable
**Polish dictation with GPT-5.6 Luna** in Settings and apply the dictation settings to test it.
This local installation is not a new GitHub release.

API revision `ca-api--polish-20260916-1135` is healthy with one warm replica and all API
traffic. Image `dictation-polish-20260916-1135` has digest
`sha256:aa95893f17309322db7b7f6c73b9dec1a40fab9629db146bff5a980cc91cb87b`.
It layers the current backend source over the immutable previously deployed runtime;
dependencies, worker/cleanup images, networking and identities were not changed. The
previous API image remains `dictation-20260915-095616`, digest
`sha256:9fb4d0cb7336c93bf94197cd4e2122d65c15ff8267e95371446b2a4e72988fe6`.
Readiness returned 200 and anonymous refinement returned 401.

The opt-in `--refine-live` probe uses the desktop API client and existing Google sign-in,
sends only built-in synthetic text, and does not write clipboard or History. Through the
deployed API, Czech and English requests took 4,114 and 2,092 ms; the Czech text was left
unchanged and the English spelling sample was corrected. A separate background-polisher
trial finished **1,550 ms** after its simulated stop, with zero raw-fallback blocks.
This is text-arrival scheduling, not a real microphone-to-editor latency measurement.
The existing synthetic MAI probe also passed: three two-chunk runs took 1,947 / 863 / 641 ms,
and real-paced synthetic capture completed transcription 466 ms after stop.

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

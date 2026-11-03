# Native Windows migration client

This is an isolated **native migration client**, not an accepted production
replacement. Its default mode remains the platform-only foundation.
Closing the window or pressing Esc hides it. A second ordinary launch activates
the existing pilot; benchmark launches with a readiness path reject duplicates.

Without `--state-directory` (and outside `--installed` mode) it has no Google login, network connection, clipboard
or transcript/history access. With that explicit argument it loads only
`settings.json`, `tokens.bin` and `history.json` in the selected existing absolute
directory. `--oauth-config` is also explicit. Isolated mode refuses the real
installed state directory and its descendants; no installed tokens/history/audio
are discovered. Its
`Local\VoicePrompt.RustPilot.*` mutexes and window classes are separate from C#.
Both isolated and installed modes use the original repo icon embedded in the EXE,
with DPI-sized window/tray icons. Settings uses the same embedded asset. The
build-only installer and its shortcuts reuse the original `app.ico`; no sidecar
file is needed to display the executable or native window icon.

Windows code is isolated in `src\platform.rs`; the audio core still forbids unsafe
code. The Windows crate version was queried in the user-approved crates.io source,
resolved by Cargo and recorded in the existing workspace lockfile.

## Run and verify

From the repository root:

```powershell
cargo fetch --manifest-path rust\Cargo.toml --locked
cargo build --manifest-path rust\Cargo.toml --offline --locked --release -p voiceprompt-windows-pilot

# A real hidden-window/tray startup, focus check, duplicate rejection and DPAPI
# health check. The self-test exits after one second; a desktop Shell is required.
rust\target\release\VoicePromptRustPilot.exe --self-test --instance-key local-test

# Interactive, limited status/tray pilot. This does not start an agent or migration loop.
rust\target\release\VoicePromptRustPilot.exe
rust\target\release\VoicePromptRustPilot.exe --quit --instance-key interactive
```

Current-user DPAPI uses exactly `VoicePrompt.DPAPI.v1` UTF-8 entropy, matching the
existing C# protector. It forbids prompts, never selects LocalMachine and releases
DPAPI allocations with LocalFree. A synthetic roundtrip is checked before tray
readiness. Ciphertext is deliberately nondeterministic. Bidirectional C# tests
check decryptability, not ciphertext equality, and reject corrupted input.
This proves the encryption envelope only, **not** token JSON, complete recovery
manifest formats, authentication behavior, or other users/machines.

```powershell
dotnet build rust\tests\reference\ReferenceAudio.csproj -c Release
backend\.venv\Scripts\python.exe rust\windows-pilot\tests\test_platform_parity.py `
  --pilot rust\target\release\VoicePromptRustPilot.exe `
  --reference rust\tests\reference\bin\Release\net8.0\ReferenceAudio.dll
```

The C# reference links the actual existing Core project. All parity inputs are
synthetic; these commands do not read real tokens or recordings. CI runs native
unit/parity checks on Windows, not the tray self-test on a potentially headless
runner. Non-Windows invocation fails explicitly; the audio crate is portable.

The explicit-path encrypted `tokens.bin` store has fixture hooks for
cross-language tests. File replacement is same-directory, flushed,
and never falls back to copy/delete across volumes. Corrupt files remain intact
and return a redacted error; no existing token cache is loaded by tray startup.
OAuth/client contracts are documented in
[`client-protocol`](../client-protocol/README.md).

## Explicit-path native client

The connected mode adds native sign-in/out, realtime status, a 48-hour History
list and read-only text view, Copy latest/selected/original, persisted pause and
reconnect. User-triggered sign-in opens the system browser and uses the bounded
PKCE loopback flow. The realtime path fetches with a Google ID token, saves history
before clipboard delivery and queues work to the native UI thread. No WebView
or .NET runtime is used. Native microphone, both dictation modes, global shortcuts,
nonactivating overlay, guarded paste, Recovery and audit outbox are now wired.
These surfaces are implemented but have not passed real-device or production E2E.

Dictation initialization now checks pending Stop, Discard, interruption,
authentication changes and shutdown both before initialization and after joining
the journal worker. Session engines and microphone startup are deferred until
that second check passes. An already-running journal write is joined rather than
detached; cancellation does not promise to interrupt a blocked OS/storage call.
A stopped or discarded new, unstarted journal is removed only after releasing
its ownership. Existing Recovery is retained, including when Esc cancels it.
Deterministic NoPorts tests cover these boundaries, stale generations, closed
channels, initialization errors and exact retained manifest/audio bytes. This
does not replace physical controller or hour-long acceptance.

The controller loop now uses private typed ports for authentication context,
bound backend creation, refinement and capture lifecycle. The normal client
always constructs `NativeDictationPorts`, which delegates to the same authenticated
API, `BoundBatchBackend` and `Microphone`; no command-line/configuration switch
selects synthetic ports. Test-only ports run the actual controller, both engines,
encrypted Recovery and atomic History together. Synthetic UI acknowledgements
exercise handoff/cancellation, not operating-system paste.

All `fixture_runtime` client actions are read-only: runtime commands are refused
before dispatch, and the native action controls are disabled except Settings
navigation. The UI rejects forced commands before confirmation dialogs or
shortcut registration. Settings fields remain navigable but Save is disabled
and rejected in the runtime; closing/cancelling preserves the input files.
The separately ignored `platform::tests::manual_native_client_keyboard_preview`
requires a fresh ordinary signed-out fixture root plus
`keyboard-fixture.json` equal to
`{"scope":"single-native-keyboard-only-fixture"}`. It verifies disabled action
controls before readiness and opens one three-minute keyboard-only preview.
User keyboard participation requires its own permission; this does not enable
clipboard, global hotkeys, devices, accounts, backend operations or installation.

`platform::tests::automated_native_client_keyboard_preview` is a separate,
user-directed Computer Use case with the same read-only guards and a bounded
ten-minute lifetime. Its fresh keyboard marker must be exactly
`{"scope":"single-native-keyboard-automation-fixture"}`. It does not reuse an
expired human case or turn automated control/focus evidence into screen-reader
or human-accessibility acceptance.

### Current controller wall-clock longevity, without physical or GUI ports

The separately ignored
`dictation::controller_tests::realtime_synthetic_controller_hour` drives the
current `DictationHost::run`, real streaming engine, DPAPI Recovery, atomic History,
polisher and periodic audit outbox through private synthetic ports on the same
two-thread Tokio runtime shape as the native client. It uses ordered 16-kHz PCM
at the native1280-byte/40-ms capture intervals, checks every sample, durable confirmation cursors and
the real 50-minute stream rotation. Only the synthetic UI handoff is acknowledged;
no window, tray, renderer, hotkey, microphone, clipboard, Google or real backend
transport is constructed.

`scripts\measure_synthetic_capture.py --controller` requires an explicitly frozen
`VoicePromptNativeControllerFixture.exe`. It samples 301 warm-idle observations,
3601 capture observations and 301 post-idle observations for the full3600s case.
The same controller and outbox stay alive across measured pre/post idle. Synthetic
History and Recovery return to the declared empty baseline only after durable
History, capture stop and outbox acknowledgment are proved. Raw process samples,
sanitized progress/logs and failure state are retained; the final report is
published only after owned-process/control-directory cleanup. The original
worst-post-idle-growth limit remains10MiB. A short smoke is explicitly incomplete,
not an hour or full-GUI acceptance.

The hidden Settings lifecycle also queries real Win32 forward/backward tab order
for all ten controls at both viewport sizes, checking disabled Save is skipped.
This emits no keyboard/input or activation and does not replace the blocked CUI
dispatcher or screen-reader/human acceptance.

The current-controller collector additionally enables an explicit sampler
`SampleJournalPath`: a flushed, create-new JSONL identity header and every raw
sample survive an interrupted capture. Completed journals must match every row
of the final raw report. Ordinary sampling without this optional path is unchanged.

Capture persistence consumes the original bounded microphone queue on one
joined blocking worker, not on the controller's Tokio `select!` thread. Stop,
auth changes and interruption stop capture and join queued writes and final
sealing; only explicit Discard invalidates queued audio. There is no second
queue, capacity increase, dropped-success fallback or stop-budget extension.
Engine pumps acquire contended state locks cooperatively so a blocked journal
writer cannot occupy all async workers. Legacy shutdown cancels network waits
but joins any already-started journal read/write before releasing ownership.
The reactive status changes to Finishing before the pending write is joined.

Deterministic NoPorts tests block the actual isolated Recovery write and prove
timer/control responsiveness, both-engine original-binding sample preservation,
full-queue overflow refusal, final-seal cancellation and joined legacy writes.
A separate locked-file failure proves that the durable prefix survives and is
not reported as completed capture. Storage failure cannot guarantee persistence
of unwritable samples; the client reports that limit explicitly rather than
claiming all queued audio is durable. Neither this repair nor a short smoke
establishes the still-incomplete current-controller wall-clock-hour acceptance.

Async state readers now respect an already-waiting synchronous capture writer
instead of repeatedly winning an immediate `try_lock`. This is admission
fairness, not a larger audio queue or a relaxed deadline. Recovery quota walks
reuse each directory entry's non-symlink-following enumeration metadata on
Windows while retaining root/path, reparse, quota and ciphertext bounds.

Test-only diagnostics distinguish accepted queue bytes, bytes received by the
writer, successful `append` calls, durable saved bytes and confirmed cursors.
They record writer age and the current Recovery read/protect/unprotect/path/
quota/write/delete/progress-publication phase without recording PCM or tokens.
An append completion is not itself durable confirmation. These diagnostics
have exposed additional backlog in bounded runs; neither their instrumentation
nor the fairness/metadata changes establish a completed hour or resolved
historical latency cause.

The optional test-only `VOICEPROMPT_LOCK_TRACE=owner-phases` mode wraps the
actual state/journal mutex guards. It records process-monotonic ownership,
wait and release timestamps, opaque Rust-thread keys and bounded operation
contexts. Its monitor uses atomics and a separate `try_lock` cache, never the
blocked mutex. Local PDB symbolization is verified on a known fixture callsite
before a diagnostic run; stack acquisition is frozen after warmup. At most64
contexts of16KiB are retained. These are owner-thread **acquisition contexts**,
not a current suspended kernel stack, OS thread ID, memory/environment dump
or WinDbg sampling. Optimized generic function names can share code; the
instrumented lock kind, operation and epoch identify the actual owner.

That diagnostic identified `send-audio-read` and `NativeRecoveryJournal::read_audio`
holding the streaming state and recovery-root locks while capture waited.
Path construction is now lexical: the complete fresh reparse/path validation
remains at every actual read/write/delete/create entry, without an extra
ancestor walk merely to construct the same path. No validation is cached across
operations. The change has passed data/ownership/junction regression; it is not
yet a completed wall-clock-hour proof.

Capture diagnostics sample atomic progress and owner/I/O snapshots every ten
seconds on a separate bounded observer thread. That observer never takes the
engine or journal mutex and is joined on normal exit or failure. It records
observed queue occupancy and measured hold/wait maxima since lock creation;
these are observations, not new acceptance thresholds. A blocked-journal unit
test verifies the observer still returns while both target locks are held.

Encrypted audio reads now use immutable published checkpoint descriptors and
owned read pins. Engine locks select/copy metadata and admit the pin; bounded
file read, DPAPI decrypt and digest/WAV validation execute on the blocking pool
outside both engine and recovery-root mutexes. The opened handle denies writes
and deletes and rejects a final reparse point, with the same fresh full-path,
byte/quota and original-binding checks.

Confirmation remains durable-before-release. A confirmed file with active pins
is retained and charged to quota until its last reader closes; a pin outliving
its journal prevents another writer or cleanup from reopening/deleting it.
Failed/cancelled reads release their pins. Stream and batch cancellation cancels
network waits but joins already-started file reads before releasing journal
ownership, reconnecting or deleting recovery. At most the existing bounded
engine read jobs hold bounded audio payloads; no extra PCM queue is introduced.

Deterministic tests block actual pinned encrypted reads, then prove concurrent
capture append, checkpoint/confirmation, Stop/Esc/auth/shutdown responsiveness,
ordered replay from the unchanged confirmed cursor, denied mutation/deletion,
last-pin cleanup and original bindings. The public read API still validates
fresh manifest state; internal engine pins use descriptors updated only after
durable publication. Corrupt data or changed binding is an explicit failure.
These are structural/data proofs and a short smoke, not a60s or hour PASS.

```powershell
# Use an explicitly chosen isolated directory; do not use installed app state for tests.
rust\target\release\VoicePromptRustPilot.exe --instance-key isolated-client `
  --state-directory D:\chosen-isolated-state

# Only for an explicitly authorized real sign-in, not unattended tests:
rust\target\release\VoicePromptRustPilot.exe --instance-key isolated-client `
  --state-directory D:\chosen-isolated-state --oauth-config D:\chosen-desktop-client.json
```

### Explicit live Google acceptance without backend access

`--auth-only --state-directory <absolute-root> --oauth-config <absolute-json>
--instance-key <N-format-GUID> --run-for-seconds <1-3600>` is a separately
authorized, one-attempt manual acceptance fixture, not a normal installed mode.
It requires a fresh root with `migration-fixture.json` containing exactly
`{"scope":"single-google-auth-only-fixture","instance_key":"<same-GUID>"}`.
Its explicit settings must use `https://fixture.example.invalid`, pause
notifications, and disable autostart and dictation. Existing token/result files
are rejected; no personal config or state is discovered.

The actual native Sign in/Sign out controls use the real PKCE/loopback/Google
token and DPAPI implementation. The user completes account selection and any
Google consent personally. Only the exact official Google endpoints are allowed.
This composition **never constructs** a backend, realtime, recovery, dictation
or audit runtime; non-authentication commands and physical ports are disabled.
Its atomic `auth-fixture-result.json` contains only Boolean state and process
identity, never token values or account details. A previous attempt cannot be
restarted in the same root.

Sign out matches the C# reference: it clears local credentials and disables
in-memory bearer vending. It does **not** call Google's token-revocation API.
Do not claim remote revocation, production-backend acceptance or automatic
dictation acceptance from this deliberately narrower test.

The separately ignored libtest
`desktop::manual_tests::manual_native_guarded_paste_once` is **not** authorized
by the Google fixture, ordinary CI, or a previous manual clipboard test. It
needs a new, explicit one-overwrite/one-automatic-Ctrl+V permission and a fresh
current directory marked with scope `single-native-guarded-paste-fixture`.
No window is created without that ownership marker; an existing result rejects
reuse. The user must personally click **Run once**, then leave focus and keys
alone for one second. Only the test's own initially blank field can be targeted.
It waits 500 ms, checks focus/modifiers/clipboard sequence, publishes only the
fixed synthetic marker, and invokes the actual `DesktopGuard` once without
Enter, Save, retries, microphone, global hotkeys, accounts or backend access.
The old clipboard is never read, backed up or restored. The normal installer
does not contain this test-only code.
Phase evidence is immutable (`manual-paste-started.json`, `-armed.json`,
`-delivering.json`, `-result.json`, `-closed.json`); the delivering phase
must publish before any clipboard call. An observer never races a report
replacement. Failures are preserved separately in the owned root, and all
previous phase/result/failure files reject reuse.

A separate ignored `desktop::manual_tests::manual_guarded_paste_observer_only`
entrypoint is a **read-only observer diagnostic**, not reopening that physical
case. It requires its own `native-guarded-paste-observer-only-fixture` marker and
fresh root, shows the same DPI-aware/icon-bearing fixture shell with Run once
disabled and the blank target read-only, and closes after three minutes. Its separate
event loop rejects action messages before any clipboard, focus-target, or native
input port. Immutable `observer-ready.json`/`observer-closed.json` are not paste
attempt evidence. Both scopes reject each other's markers and all old phase roots.
An actual paste test still needs new explicit permission and a new physical root;
observer success cannot consume or substitute for that decision.

Visible test shells now use temporary topmost placement on the active monitor
without initially activating it. A background AX-visible HWND alone is not proof
that the user sees the window; human visibility was checked separately in the
NoPorts observer. This does not change the normal client's window behavior.
Physical evidence is write-ahead and immutable: started/ready/attempted/focus
preflight/focused/armed/delivering/result/closed. In particular, started=false is
only an initial snapshot, not evidence that a later Run was never accepted.

The fixture's `SetFocus` handling checks an existing current-UI-thread target and
the actual `GetFocus` postcondition. Win32 returns the **previous** focus: disabling
the clicked Run button clears it, so a successful focus change can be projected
by the binding as `Err(S_OK)`. This is not a generic suppression of Windows errors.
The separate ignored `desktop::manual_tests::post_run_focus_noports_fixture`
requires a fresh `native-post-run-focus-noports-fixture` marker. It reproduces
the exact DisableWindow/SetFocus sequence using owned message-only controls, publishes
immutable per-operation/attempt/target/normal-exit evidence, and never constructs
clipboard or native-input ports. `HWND_MESSAGE` plus `WS_EX_NOACTIVATE` keeps the
parent out of desktop Z-order; hidden alone was not a sufficient activation
boundary in parallel regression. It verifies own PID/thread/parent/focus and nonactivation, not the
full foreground/clipboard/SendInput guard or an actual paste acceptance.

Guard diagnostics now distinguish a focus/modifier/missing-sequence/changed-sequence
rejection from a native input attempt. Native event counts and any release-only
cleanup counts are explicit; the ordinary client's Boolean API still succeeds
only for the complete four-event Ctrl+V batch. Synthetic callbacks exercise zero,
partial, complete and incomplete-cleanup outcomes without calling native sensors,
clipboard or SendInput. A false legacy Boolean alone must not be interpreted as
"no native events", and an unattempted field observation is not a blank-field proof.
The physical fixture explicitly prohibits manual paste; a marker later pasted by
the user is assisted evidence, not an automatic-paste PASS.

Clipboard writes use immediate `CF_UNICODETEXT` publication from moveable global
memory, transferring ownership only after successful `SetClipboardData`.
The writer now checks `CloseClipboard` before returning a publication sequence;
the guard must not mark a sequence sampled inside an unfinished clipboard
transaction. Ownership metadata must still identify the publishing window.
Missing/changed ownership, unavailable sequence or a failed close is visible
failure, never a successful copy or an arbitrary rebase after a paste rejection.
The close guard is UI-thread-affine; a successful explicit close does not close
again on drop. No clipboard contents are read or restored by these checks.
Bounded retries apply only to a busy clipboard. Pause, suspended/sign-out state
and dictation-busy policy are rechecked at the actual UI write; an abandoned UI
request is cancelled so it cannot write later. Audit-source events never auto-copy.
Manual cached copying works while paused or signed out.
Notifications contain generic readiness/fallback messages, not transcript text.

Settings preserve every current C# field and retain unknown fields on Rust writes.
Invalid/corrupt settings fail explicitly instead of silently selecting production
defaults. Per-minute history cleanup persists retention; read errors preserve files.
Native Settings includes the current backend, pause, autostart, shortcut/language,
streaming/polishing and final-wait fields. Saving while idle persists atomically,
reconnects the native client and applies the one per-user Run mechanism. Standard
controls provide keyboard dialog navigation and per-monitor bounds. Settings
retains DPI-specific system fonts; the main window now uses the original WPF
Segoe UI roles, white/gray/orange palette, embedded 40-DIP branding, rounded status
card, flat section underline/buttons and two-line localized History rows. Actual
selected-tab font inheritance, row selection and keyboard-focus styling follow
the reference rather than a new visual theme. Native sections and the read-only
text toggle do not overlap; section transitions clear previous borders without
painting over child controls. Fonts/brushes/pens are owned and restored, with a
separate 1,000-cycle graphics-only resource regression in CI.
Human screen-reader/high-DPI acceptance remains pending.

Settings is resizable and scrollable instead of a fixed-height form. Wide windows
keep label/input columns; narrow windows stack them without losing any field.
Initial placement and DPI suggestions stay inside the owner's monitor work area,
including monitors with negative coordinates. Native scrollbar/page/thumb/wheel
navigation uses bounded content geometry and the Windows wheel preference; dialog
keyboard navigation reveals the newly focused input. DPI reflow preserves the
logical scroll position and regenerates geometry rather than accumulating scale
rounding. The existing hidden self-test checks actual HWND positions for every
input and Save in default/narrow viewports, without showing/focusing or saving.
Validation fixtures disable Save and reject save commands before shortcut,
autostart or persistence work; ordinary client Save behavior is unchanged.
History supports manual cloud Refresh (no autocopy), Clear with confirmation and
copy latest/selected/original; Recovery supports list/recover/explicit deletion.
History double-click, Enter/Space and Ctrl+C now invoke the same cached selected
copy as the actual WPF handlers; Enter/Space retain WPF's modifier behavior while
Ctrl+C requires Control only. F5 refresh is unmodified and respects the disabled
Refresh button. These are local owned-window messages, not new global hotkeys.
Copy original follows the actual selected row's non-null `raw_body`, including
an explicitly empty original, rather than merely the presence of any history.
Mapping/selection tests do not claim a new physical clipboard or keyboard trial.

The separately ignored full-client visual previews require a fresh marked
signed-out fixture root and close after 180 seconds. They prohibit authentication,
clipboard, capture, hotkeys and installed-state discovery, and check unchanged
synthetic settings/history and absence of tokens on normal exit. The C# fixture's
`--client-visual-fixture` uses the actual original `MainWindow`, PNG and ICO, not a
recreated reference. Actual default/minimum native previews have been compared
with it at 144 DPI. Recovery now shows local time, pending sealed chunks, language
and audit state; short viewports use two compact lines without hiding those
fields. Its footer distinguishes recovery from live dictation and never promises
automatic paste. Empty History has the original retention hint. Account email
and state are published together after persistence, through a nonblocking display
snapshot; reading the header cannot block behind browser login or a network/cache
operation. The snapshot is display metadata, not signature verification or a
replacement for the authenticated bearer path. Sign-in/out availability matches
the current account state, including clearing a rejected saved grant.

Extra main quick actions, separate Settings, optional text view and some native
control metrics remain explicit differences; this is not full visual, live-account,
human keyboard/screen-reader or migration acceptance.
Frozen post-overlay performance evidence belongs to its original binary and
does not measure this later main-window styling.

Hidden startup now keeps the complete history/recovery/client models but defers
main-window list formatting until the window opens, matching the reference's lazy
main window. Tray delivery, notifications, dictation and recovery background work
continue while hidden. Reopening marks presentation dirty so it uses the latest
model. The explicit `--self-test` still renders its hidden control probes.

Client comparisons preserve each completed startup as an immutable full-precision
`startup-<implementation>-<trial>.json`, outside the timed readiness interval.
Failures publish a distinct `measurement-failure.json` and still fail the command;
partial data is never a complete verdict. Reusing an old complete/partial evidence
directory is refused without modifying its result. The normal 15-second
operational readiness budget and all statistical acceptance thresholds are unchanged.

The main History/Recovery sections now adapt to the original 720x540-DIP window
and 560x420 minimum instead of a fixed 570x730 all-controls canvas. Action rows
wrap at the minimum width. History initially shows the original list-oriented
view; **Show selected text** retains the migration client's read-only body view
without forcing both panes into a small viewport. Recovery has its own explanatory
text, list and explicit read-only Refresh. Native accessible selection/toggle state
is observed even when an automation client changes it without a notification.
Clipboard controls are disabled when the fixture's physical port is disabled.

Two ignored actual full-client visual fixtures,
`platform::tests::manual_native_client_visual_preview` and
`platform::tests::manual_native_client_minimum_visual_preview`, require fresh
explicitly marked signed-out synthetic roots and close after 180 seconds.
They never discover credentials or enable microphone, shortcuts, clipboard,
autostart or networking. Their immutable ready/closed files include actual
viewport, disabled capabilities and byte-exact settings/history checks.
Only safe section/toggle navigation is used for visual checks; actual keyboard,
screen-reader, monitor-switch and original-theme acceptance remain separate.

The microphone adapter deliberately preserves the existing WinMM/WaveIn path:
16 kHz mono PCM16, 40 ms buffers and a bounded 50-buffer disk-consumer queue.
WinMM is legacy; WASAPI is Microsoft's recommendation for new designs, but changing
capture engines and resampling concurrently would invalidate behavior/latency parity.
Overload and device/storage failures stop explicitly with durable recovery retained.
Tests exercise synthetic queues, not a real microphone.

Hold selects its focus target at Start, hands-free at Stop. The guard latches
window/process/thread/focused-control changes, waits for released modifiers,
checks the clipboard sequence captured inside the publication lock and sends
exactly one Ctrl+V without Enter. Stale/cancelled/auth-invalidated UI jobs cannot
paste. The overlay does not activate a foreground app. Real paste has not been tested.

The native overlay now follows the actual WPF `Views\DictationIndicator.cs`:
440x112 DIPs, a dark rounded-17 panel, the half-opacity OrangeRed recording dot
and blue finishing dot, Segoe UI 12/14 text, a three-line/20-DIP preview with
character ellipsis, and primary-work-area bottom-center placement. The elapsed
clock and growing preview update live; confirmed-but-unpasted completion dismisses
once after 900ms. Pending/error recovery is not mislabelled as that completed
state. The layered window is click-through/nonactivating, and drawing resources
are lazy and released on hide. Direct2D software rendering/DirectWrite replace
the old default-font bordered STATIC rectangle; there is no WebView.

The opt-in ignored `dictation_ui::visual_tests::manual_reference_overlay_visual_fixture`
is **visual-only**, not a microphone, shortcut, account, clipboard or paste test.
It requires a fresh working directory containing only `overlay-fixture.json`
with `{"scope":"native-overlay-visual-fixture"}`. It uses the real UI/controller
presentation path, process-only per-monitor-v2 awareness and five bounded synthetic
states, exports its own Pbgra32 rasters at 96/144/192 DPI, and closes normally.
Launch visible fixtures through the approved app UI tooling, not a foreground
shell workaround. No Run/paste control exists in this fixture.

The existing C# ClientFixture's `--overlay-visual-fixture <absolute-root>` requires
the analogous `reference-overlay-visual-fixture` marker. It creates only the actual
original `DictationIndicator`, **not** AppHost/device/authentication composition.
`scripts\compare_overlay_visuals.py` exports these owned rasters as PNG comparisons
and checks exact frame semantics and completion; it does not capture the desktop,
claim pixel identity or replace visual review, actual monitor/DPI transition,
screen-reader, performance or physical dictation acceptance.

The tray also retains the original Open(default)/Copy latest/Pause notifications/
Exit menu, live connection tooltip and double-click/keyboard/balloon activation.
Reconnect remains in the main window rather than adding a different tray popup.

Polishing remains opt-in, single physical request, bounded rolling windows and
literal unique nonoverlapping edits against the original text. Stop freezes late
responses and leaves unprocessed tails raw; failures stay in diagnostics/History.
Audit freezes before delivery and drains independently every 30 seconds while idle.
Only acknowledged upload deletes the outbox, with fixed original completion/expiry.
Pause does not block explicit manual copy or audit upload; account/backend changes do.

The installed/isolated CLI parser is tested without filesystem/IPC access.
Packaged `VoicePrompt.exe` and explicit `--installed` always select the installed
client plan; value-option order cannot turn them into the platform-only tray.
Installed mode rejects fixture/readiness/explicit-path flags before any personal
state is opened. Process tests exercise only those rejection paths, not a real
installed launch or upgrade.

## Full-client signed-out measurement

The separate `--signed-out-fixture <marked-root> --fixture-ready-file <new-path>
--instance-key <N-guid> --run-for-seconds <1-3600>` mode initializes the actual
client/recovery/dictation/audit/overlay lifecycle without enabling physical ports.
It refuses token caches, OAuth, installed state and reparse paths and publishes
only `full-client-signed-out-fixture` readiness, never foundation readiness.
The matching C# tool starts the actual WPF App with isolated paths/IPC; see
`scripts\measure_clients.py` and its guard tests. These are bounded signed-out
measurements, not real-device or whole-migration acceptance.

## Recovery codec parity, not the recovery runtime

`--recovery-load-fixture` and `--recovery-save-fixture` take an explicit absolute
`<lowercase-session-id>\manifest.enc` path. They use the existing DPAPI entropy,
bounded decrypt/parse and atomic publication; corrupt input leaves the old file
intact. Reparse points in any ancestor are refused. These are codec/parity hooks,
not the quota-aware recovery runtime. The connected tray's separate Recovery UI
and real journal are described below.

The fixture suite reads real C# v3/v4 journals (including pending and frozen audit
state), compares plaintext byte-for-byte and reopens Rust output through the actual
C# `RecoveryStore`. Synthetic v1/v2 fixtures verify both readers and upgrades without
inventing absolute cursors or target context.

```powershell
backend\.venv\Scripts\python.exe rust\windows-pilot\tests\test_recovery_parity.py `
  --pilot rust\target\release\VoicePromptRustPilot.exe `
  --reference rust\tests\reference\bin\Release\net8.0\ReferenceAudio.dll
```

`NativeRecoveryStore` now implements the real durable journal separately from
those codec hooks. A zero-byte `writer.lock` uses Windows share mode zero for
cross-process/session exclusion; stores in the same process share one root gate.
Only one active journal owner is allowed per ID. All mutations reread the latest
manifest under that gate. Root quota is 256 MiB by default and includes orphan
files, previous targets and staging space. Confirmations publish before audio
deletion; same-text legacy retries do not need another manifest publication.
Cleanup uses the original strict `>48 h` updated-at boundary and preserves active
or unreadable sessions. Deletion refuses nested directories before deleting files.

Eight native tests cover reopen/sample parity, quota failure before publication,
orphan accounting, confirmation failure without audio deletion, concurrent capture
and result updates, active ownership, corruption and retention. The external suite
also exercises two actual writer processes and real Windows junction rejection
before read/write/delete. Native-created journals reopen through actual C# Core;
C# v1-v4 fixtures also open through the full native store.

The extra root lock file contains no data and is ignored by legacy C# directory
listing. C# **does not cooperate with this new writer lock**: cutover still requires
verified application single-instance/upgrade coordination, and two real engines
must never share the same root. Tests use sequential C#/Rust inspection of isolated
fixtures, not overlapping capture engines.

`LegacySession` now provides single-producer chunking, C# JSON checkpoints, at most
two ASR requests, capture-order output, terminal auth/backoff progress and durable
stop-timeout recovery. `StreamingSession` provides independent two-second durable
capture, live provisional preview versus confirmed output, quiet-pause commits,
50-minute rollover, auth-aware replay from the persisted completed cursor and batch
Recovery with retained context. A received ACK/provisional event never releases
audio. Confirmation beyond a sent commit is rejected.

Synthetic session tests cover out-of-order legacy completion, bounded concurrency,
stop timeout/shutdown, restart audio, live fragmentation, provisional failure,
full replay with exact samples, partial-confirmed replay and batch overlap recovery.
They use real encrypted journals but fake ASR/model peers, not a microphone or paid AI.
Backend binding preserves the original URL/newline/email format and is checked again
under the token session lock; sign-out cancels a bound request before its text is used.

Device capture/hotkeys/overlay/paste, polishing/outbox scheduling and native Recovery
UI are now wired. Library/session checks are not complete interactive dictation E2E.

## Build-only migration installer

`scripts\Build-RustWindowsInstaller.ps1` builds the release executable and an
Inno Setup per-user x64 installer under ignored `rust\target\installer-output`.
It uses the existing installed compiler, never installs tooling, never runs the
installer and stages only the executable and repository icon. It bundles no OAuth
configuration, tokens, history, settings or recovery and needs no .NET runtime.

The source installer reuses the existing AppId/Run value, preserves user data and
requests graceful synchronous `--quit`; inability to exit aborts, with no force-kill.
The native compatible-instance adapter uses the exact C# mutex/auto-reset events.
Bidirectional tests run real C#/Rust processes in randomized fixture namespaces,
not the installed application's namespace.

`--installed` (also selected by packaged `VoicePrompt.exe`) is the runtime cutover
path: it uses the compatible instance guard and existing per-user configuration and
state. **Do not run it or the installer for unattended tests.** Actual installation,
upgrade/rollback and personal-state acceptance require the later explicit consent.
The built installer is a migration preview, not a released or accepted replacement.

The desktop-only lifecycle runner uses temporary synthetic caches. Its self-test
disables both OAuth configuration and the real clipboard and checks hidden startup
focus plus the actual native History list. It is not a production E2E or a real
clipboard/paste test. CI does not run this desktop-dependent command.

```powershell
backend\.venv\Scripts\python.exe rust\windows-pilot\tests\test_client_lifecycle.py `
  --pilot rust\target\release\VoicePromptRustPilot.exe
```

## Measure this limited foundation

Choose an existing, empty output directory. The collector does not overwrite old
reports. It launches only `VoicePromptRustPilot.exe`, uses fresh isolated instance
keys, verifies PID/readiness, stops each owned instance gracefully and drains
diagnostic streams. Errors are failures, not faster startup samples.

```powershell
.\scripts\Measure-RustNativePilot.ps1 `
  -Executable rust\target\release\VoicePromptRustPilot.exe `
  -OutputDirectory "$env:TEMP\voiceprompt-native-measurements" `
  -SourceRevision YOUR_BUILT_COMMIT
```

The defaults are 30 process starts and 301 memory/CPU samples over five minutes.
The collector measures new-process to observed **native tray + DPAPI readiness**,
not OS cold-cache startup, Google login, transcription, the cloud, or user-perceived
dictation latency. Readiness is published atomically without overwriting.
Connected mode refuses foundation readiness arguments to prevent mislabeled
performance evidence.
The native process has a one-hour failsafe; memory runs are limited to 55 minutes.

`native-foundation-startup.json` retains every trial and the binary hash;
`native-foundation-memory.json` uses the common process sampler. The startup
report explicitly marks comparison with the complete client as invalid.
Much lower numbers for this incomplete foundation cannot pass the full pilot
performance goals. Full feature-parity measurements must be repeated later.

use super::*;
use crate::dictation_ports::DictationPorts;
use crate::microphone::CaptureError;
use crate::recovery_store::tests::TemporaryRoot;
use crate::{AudioSender, BatchBackend, EventReceiver};
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use tokio::sync::Notify;
use voiceprompt_client_protocol::api::{
    ApiError, ApiErrorKind, DictationEdit, DictationRefinement,
};
use voiceprompt_client_protocol::dictation_stream::DictationMessage;
use voiceprompt_client_protocol::polishing::RefinementService;
use voiceprompt_client_protocol::recovery::AudioCheckpoint;

const URL: &str = "https://fixture.example.invalid";
const EMAIL: &str = "synthetic@example.invalid";
const RAW: &str = "synthetic controller transcript keeps every recorded sample and completes only after a confirmed stop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureProfile {
    Streaming,
    Legacy,
}

impl CaptureProfile {
    fn parse(engine: &str, pattern: &str) -> Result<Self, &'static str> {
        match (engine, pattern) {
            ("streaming", "ordered-quiet-v1") => Ok(Self::Streaming),
            ("legacy", "voiced-pause-overlap-v1") => Ok(Self::Legacy),
            _ => Err("Unknown or mismatched controller engine/pattern."),
        }
    }

    fn engine(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Legacy => "legacy",
        }
    }

    fn pattern(self) -> &'static str {
        match self {
            Self::Streaming => "ordered-quiet-v1",
            Self::Legacy => "voiced-pause-overlap-v1",
        }
    }

    fn sample(self, index: u64) -> i16 {
        if self == Self::Legacy {
            let position = index % (12 * 16_000);
            if (9_600..137_600).contains(&position) || position >= 147_200 {
                return 1_200 + (index % 257) as i16 - 128;
            }
        }
        (index % 97) as i16 - 48
    }
}

struct LegacyExpectedChunk {
    digest: String,
    wav_bytes: usize,
    end_byte: u64,
    overlaps_previous: bool,
    text: String,
}

fn legacy_wav_digest(wav: &[u8]) -> String {
    // Reuse the canonical recovery codec's WAV digest without another test dependency.
    let manifest = voiceprompt_client_protocol::recovery::RecoveryManifest::create(
        "cs".into(),
        String::new(),
        false,
        0,
    )
    .unwrap()
    .checkpoint(
        &[],
        (wav.len() - 44) as u64,
        &[AudioCheckpoint {
            wav,
            overlaps_previous: false,
            end_byte: 0,
        }],
        false,
        0,
    )
    .unwrap();
    manifest.chunks[0]
        .digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct LegacyOracle {
    chunks: Vec<LegacyExpectedChunk>,
    seen: Vec<bool>,
    completions: Vec<usize>,
    final_tail_audio_bytes: usize,
}

impl LegacyOracle {
    fn new(seconds: u64) -> Self {
        let mut chunker = voiceprompt_core::chunker::PcmChunker::new(120.0).unwrap();
        let mut chunks = Vec::new();
        let mut frame = [0; crate::microphone::BUFFER_BYTES];
        let mut record = |emitted: Vec<voiceprompt_core::chunker::AudioChunk>| {
            for chunk in emitted {
                let index = chunks.len();
                chunks.push(LegacyExpectedChunk {
                    digest: legacy_wav_digest(&chunk.wav),
                    wav_bytes: chunk.wav.len(),
                    end_byte: chunk.end_byte,
                    overlaps_previous: chunk.overlaps_previous,
                    text: format!("legacychunk{index:04} {RAW}"),
                });
            }
        };
        for index in 0..seconds * 25 {
            for (offset, bytes) in frame.chunks_exact_mut(2).enumerate() {
                bytes.copy_from_slice(
                    &CaptureProfile::Legacy
                        .sample(index * 640 + offset as u64)
                        .to_le_bytes(),
                );
            }
            record(chunker.append(&frame).unwrap());
        }
        let tail = chunker.snapshot();
        assert_eq!(tail.processed_bytes, seconds * 32_000);
        assert!(tail.frame.is_empty());
        let final_tail_audio_bytes = tail.audio.len();
        record(chunker.flush().unwrap());
        assert!(!chunks.is_empty());
        if seconds >= 12 {
            assert!(chunks.iter().any(|chunk| chunk.wav_bytes == 192_044));
            assert!(chunks.iter().any(|chunk| chunk.overlaps_previous));
            assert!(
                chunks
                    .iter()
                    .any(|chunk| chunk.end_byte < seconds * 32_000 && chunk.wav_bytes < 192_044)
            );
        }
        let mut digests: Vec<_> = chunks.iter().map(|chunk| &chunk.digest).collect();
        digests.sort();
        digests.dedup();
        assert_eq!(
            digests.len(),
            chunks.len(),
            "Each source-oracle WAV must identify exactly one chunk."
        );
        Self {
            seen: vec![false; chunks.len()],
            chunks,
            completions: Vec::new(),
            final_tail_audio_bytes,
        }
    }
}

fn failure() -> ApiError {
    ApiError {
        kind: ApiErrorKind::Forbidden,
        status: Some(403),
        problem: None,
        response_invalid: false,
    }
}

#[test]
fn writer_probe_distinguishes_queue_admission_received_and_completed_samples() {
    let ports = Ports::new();
    let (_capture, mut receiver) = ports.start_capture().unwrap();
    let frame = vec![0; crate::microphone::BUFFER_BYTES];
    ports.try_feed(&frame).unwrap();
    assert_eq!(
        ports.0.accepted_bytes.load(Ordering::Acquire),
        frame.len() as u64
    );
    assert_eq!(ports.0.writer_received_bytes.load(Ordering::Acquire), 0);
    let buffer = receiver.try_recv().unwrap();
    ports.capture_write_started(buffer.0.len());
    assert!(ports.0.writer_active.load(Ordering::Acquire));
    assert_eq!(
        ports.0.writer_received_bytes.load(Ordering::Acquire),
        frame.len() as u64
    );
    assert_eq!(ports.0.writer_completed_bytes.load(Ordering::Acquire), 0);
    ports.capture_write_finished(true);
    assert!(!ports.0.writer_active.load(Ordering::Acquire));
    assert_eq!(
        ports.0.writer_completed_bytes.load(Ordering::Acquire),
        frame.len() as u64
    );
}

#[test]
fn synthetic_capture_matches_native_packets_and_refuses_overflow_without_successful_drop() {
    let ports = Ports::new();
    let (_capture, mut receiver) = ports.start_capture().unwrap();
    let frame = vec![0; crate::microphone::BUFFER_BYTES];
    assert_eq!(frame.len(), 1280);
    assert_eq!(frame.len() * 25, 32000);
    for _ in 0..crate::microphone::OUTPUT_BUFFER_COUNT {
        assert!(ports.try_feed(&frame).is_ok());
    }
    assert_eq!(ports.try_feed(&frame), Err(AdmissionError::Overflow));
    let mut bytes = 0;
    while let Ok(buffer) = receiver.try_recv() {
        bytes += buffer.0.len();
    }
    assert_eq!(bytes, 1280 * 50);
    assert!(ports.try_feed(&frame).is_ok());
}

#[test]
fn admission_failure_is_typed_exact_and_immutable_before_cleanup() {
    for closed_receiver in [false, true] {
        let ports = Ports::new();
        let (capture, receiver) = ports.start_capture().unwrap();
        let frame = [0; crate::microphone::BUFFER_BYTES];
        ports.try_feed(&frame).unwrap();
        if closed_receiver {
            drop(receiver);
        } else {
            Ports::stop_capture(capture).unwrap();
        }
        let expected = if closed_receiver {
            AdmissionError::WorkerFailure
        } else {
            AdmissionError::NotRunning
        };
        assert_eq!(ports.try_feed(&frame), Err(expected));
        let first = ports.0.admission_failure.get().unwrap().clone();
        assert_eq!(first["attempted_sequence"], 2);
        assert_eq!(first["last_accepted_sequence"], 1);
        assert_eq!(first["accepted_bytes"], 1280);
        assert_eq!(first["rejected_bytes"], 1280);
        assert_eq!(first["writer_received_bytes"], 0);
        assert_eq!(first["writer_completed_bytes"], 0);
        assert!(first["last_published_saved_bytes"].is_null());
        assert_eq!(ports.try_feed(&frame), Err(expected));
        assert_eq!(ports.0.admission_failure.get().unwrap(), &first);
        assert_eq!(ports.0.accepted_bytes.load(Ordering::Acquire), 1280);
    }
}

#[derive(Default)]
struct BackendState {
    audio_gate: Mutex<Option<oneshot::Receiver<()>>>,
    batches: Mutex<Vec<Vec<u8>>>,
    stream: Mutex<Vec<u8>>,
    starts: Mutex<Vec<u64>>,
    batch_blocked: AtomicBool,
    failed: AtomicBool,
    batch_released: AtomicBool,
    active: AtomicUsize,
    count_only: AtomicBool,
    stream_bytes: AtomicU64,
    confirmed_to: AtomicU64,
    ordered_samples: AtomicBool,
    confirmed_cursors: Mutex<Vec<u64>>,
    legacy: Mutex<Option<LegacyOracle>>,
    legacy_second_completed: Notify,
    changed: Notify,
}

struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Default)]
struct Backend(Arc<BackendState>);

impl BatchBackend for Backend {
    async fn transcribe(
        &self,
        wav: &[u8],
        language: &str,
        context: &str,
    ) -> Result<String, ApiError> {
        let active = self.0.active.fetch_add(1, Ordering::AcqRel) + 1;
        let _active = Active(&self.0.active);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(language, "cs");
        assert_eq!(context, format!("{URL}\n{EMAIL}"));
        let legacy = {
            let mut configured = self.0.legacy.lock().unwrap();
            configured.as_mut().map(|oracle| {
                assert!(
                    active <= 2,
                    "Legacy exceeded its existing two-request limit."
                );
                let digest = legacy_wav_digest(wav);
                let index = oracle
                    .chunks
                    .iter()
                    .position(|chunk| chunk.digest == digest)
                    .expect("Unexpected legacy WAV; source order/content/overlap changed.");
                assert!(!oracle.seen[index], "A legacy chunk was sent twice.");
                assert_eq!(oracle.chunks[index].wav_bytes, wav.len());
                oracle.seen[index] = true;
                (
                    index,
                    oracle.chunks.len() >= 2,
                    oracle.chunks[index].text.clone(),
                )
            })
        };
        if let Some((index, pair, text)) = legacy {
            if index == 0 && pair {
                self.0.legacy_second_completed.notified().await;
            }
            self.0
                .legacy
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .completions
                .push(index);
            if index == 1 {
                self.0.legacy_second_completed.notify_one();
            }
            return Ok(text);
        }
        self.0.batches.lock().unwrap().push(wav.to_vec());
        self.0.changed.notify_waiters();
        while self.0.batch_blocked.load(Ordering::Acquire)
            && !self.0.batch_released.load(Ordering::Acquire)
        {
            self.0.changed.notified().await;
        }
        if self.0.failed.load(Ordering::Acquire) {
            return Err(failure());
        }
        Ok(RAW.into())
    }
}

#[derive(Clone)]
struct StreamSender {
    backend: Backend,
    end: Arc<AtomicU64>,
    provisional: Arc<AtomicBool>,
    events: mpsc::Sender<Result<DictationMessage, ApiError>>,
}

impl AudioSender for StreamSender {
    async fn audio(&self, pcm: &[u8]) -> Result<(), ApiError> {
        let gate = self.backend.0.audio_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.await.map_err(|_| failure())?;
        }
        self.end.fetch_add(pcm.len() as u64, Ordering::AcqRel);
        let start = self
            .backend
            .0
            .stream_bytes
            .fetch_add(pcm.len() as u64, Ordering::AcqRel);
        if self.backend.0.count_only.load(Ordering::Acquire) {
            if self.backend.0.ordered_samples.load(Ordering::Acquire) {
                for (index, bytes) in pcm.chunks_exact(2).enumerate() {
                    assert_eq!(
                        i16::from_le_bytes([bytes[0], bytes[1]]),
                        ((start / 2 + index as u64) % 97) as i16 - 48,
                        "Controller PCM was duplicated, reordered or replaced."
                    );
                }
            } else {
                let pattern = [12i16.to_le_bytes(), (-12i16).to_le_bytes()].concat();
                for (index, byte) in pcm.iter().enumerate() {
                    assert_eq!(*byte, pattern[((start + index as u64) % 4) as usize]);
                }
            }
        } else {
            self.backend.0.stream.lock().unwrap().extend_from_slice(pcm);
        }
        if !self.provisional.swap(true, Ordering::AcqRel) {
            self.events
                .send(Ok(DictationMessage::Delta {
                    item_id: "synthetic".into(),
                    text: "provisional must never be delivered".into(),
                }))
                .await
                .map_err(|_| failure())?;
        }
        Ok(())
    }

    async fn commit(&self, finish: bool) -> Result<(), ApiError> {
        if self.backend.0.count_only.load(Ordering::Acquire) {
            let end = self.end.load(Ordering::Acquire);
            let previous = self.backend.0.confirmed_to.swap(end, Ordering::AcqRel);
            if end > previous {
                self.backend.0.confirmed_cursors.lock().unwrap().push(end);
                self.events
                    .send(Ok(DictationMessage::Confirmed {
                        item_id: "synthetic".into(),
                        text: if previous == 0 {
                            RAW.into()
                        } else {
                            String::new()
                        },
                        byte_end: end,
                    }))
                    .await
                    .map_err(|_| failure())?;
            }
            if finish {
                self.events
                    .send(Ok(DictationMessage::Done { byte_end: end }))
                    .await
                    .map_err(|_| failure())?;
            }
            return Ok(());
        }
        if !finish {
            return Ok(());
        }
        if self.backend.0.failed.load(Ordering::Acquire) {
            return self
                .events
                .send(Err(failure()))
                .await
                .map_err(|_| failure());
        }
        let end = self.end.load(Ordering::Acquire);
        self.events
            .send(Ok(DictationMessage::Confirmed {
                item_id: "synthetic".into(),
                text: RAW.into(),
                byte_end: end,
            }))
            .await
            .map_err(|_| failure())?;
        self.events
            .send(Ok(DictationMessage::Done { byte_end: end }))
            .await
            .map_err(|_| failure())
    }
}

struct StreamReceiver(mpsc::Receiver<Result<DictationMessage, ApiError>>);

impl EventReceiver for StreamReceiver {
    async fn receive(&mut self) -> Result<DictationMessage, ApiError> {
        self.0.recv().await.ok_or_else(failure)?
    }
}

impl StreamingBackend for Backend {
    type Sender = StreamSender;
    type Receiver = StreamReceiver;

    async fn connect(
        &self,
        language: &str,
        start: u64,
        context: &str,
    ) -> Result<(StreamSender, StreamReceiver), ApiError> {
        assert_eq!(language, "cs");
        assert_eq!(context, format!("{URL}\n{EMAIL}"));
        if self.0.ordered_samples.load(Ordering::Acquire) {
            assert_eq!(start, self.0.stream_bytes.load(Ordering::Acquire));
        }
        self.0.starts.lock().unwrap().push(start);
        let (events, receiver) = mpsc::channel(16);
        Ok((
            StreamSender {
                backend: self.clone(),
                end: Arc::new(AtomicU64::new(start)),
                provisional: Arc::new(AtomicBool::new(false)),
                events,
            },
            StreamReceiver(receiver),
        ))
    }
}

#[derive(Default)]
struct Refiner {
    calls: AtomicUsize,
    failed: AtomicBool,
}

impl RefinementService for Refiner {
    async fn refine(&self, text: &str, _: &str) -> Result<DictationRefinement, ApiError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.failed.load(Ordering::Acquire) {
            return Err(failure());
        }
        Ok(DictationRefinement {
            text: text.replacen("synthetic", "polished", 1),
            edits: vec![DictationEdit {
                original: "synthetic".into(),
                replacement: "polished".into(),
            }],
        })
    }
}

struct CaptureState {
    sender: Option<mpsc::Sender<CaptureBuffer>>,
    receiver: Option<mpsc::Receiver<CaptureBuffer>>,
    tail: Vec<u8>,
}

struct Probe {
    auth: watch::Sender<AuthState>,
    backend: Arc<Backend>,
    refiner: Arc<Refiner>,
    capture: Mutex<CaptureState>,
    opened: AtomicUsize,
    stopped: AtomicUsize,
    fail_open: AtomicBool,
    fail_stop: AtomicBool,
    prepare_capture: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    clock: Instant,
    accepted_bytes: AtomicU64,
    attempted_sequence: AtomicU64,
    last_accepted_sequence: AtomicU64,
    admission_failure: std::sync::OnceLock<serde_json::Value>,
    observed_queue_slots: AtomicUsize,
    writer_received_bytes: AtomicU64,
    writer_completed_bytes: AtomicU64,
    writer_packet_bytes: AtomicU64,
    writer_active: AtomicBool,
    writer_started_ns: AtomicU64,
    writer_last_duration_ns: AtomicU64,
    capture_trace: std::sync::OnceLock<Arc<crate::lock_trace::Lock>>,
}

#[derive(Clone)]
struct Ports(Arc<Probe>);

struct Capture(Arc<Probe>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdmissionError {
    NotRunning,
    WorkerFailure,
    Overflow,
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotRunning => "Synthetic capture is not running.",
            Self::WorkerFailure => "Synthetic capture receiver closed.",
            Self::Overflow => "Synthetic capture queue overflowed.",
        })
    }
}

impl Ports {
    fn new() -> Self {
        let (auth, _) = watch::channel(AuthState::SignedIn);
        let (sender, receiver) = mpsc::channel(crate::microphone::OUTPUT_BUFFER_COUNT);
        Self(Arc::new(Probe {
            auth,
            backend: Arc::new(Backend::default()),
            refiner: Arc::new(Refiner::default()),
            capture: Mutex::new(CaptureState {
                sender: Some(sender),
                receiver: Some(receiver),
                tail: Vec::new(),
            }),
            opened: AtomicUsize::new(0),
            stopped: AtomicUsize::new(0),
            fail_open: AtomicBool::new(false),
            fail_stop: AtomicBool::new(false),
            prepare_capture: Mutex::new(None),
            clock: Instant::now(),
            accepted_bytes: AtomicU64::new(0),
            attempted_sequence: AtomicU64::new(0),
            last_accepted_sequence: AtomicU64::new(0),
            admission_failure: std::sync::OnceLock::new(),
            observed_queue_slots: AtomicUsize::new(0),
            writer_received_bytes: AtomicU64::new(0),
            writer_completed_bytes: AtomicU64::new(0),
            writer_packet_bytes: AtomicU64::new(0),
            writer_active: AtomicBool::new(false),
            writer_started_ns: AtomicU64::new(0),
            writer_last_duration_ns: AtomicU64::new(0),
            capture_trace: std::sync::OnceLock::new(),
        }))
    }

    async fn feed(&self, bytes: &[u8]) {
        for packet in bytes.chunks(crate::microphone::BUFFER_BYTES) {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let ready = {
                        let state = self.0.capture.lock().unwrap();
                        let sender = state
                            .sender
                            .as_ref()
                            .expect("Synthetic capture was stopped.");
                        assert!(!sender.is_closed(), "Synthetic capture consumer closed.");
                        sender.capacity() != 0
                    };
                    if ready {
                        self.try_feed(packet).unwrap();
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("Explicit synthetic bulk-driver backpressure timed out.");
        }
    }

    fn try_feed(&self, bytes: &[u8]) -> Result<(), AdmissionError> {
        assert!(bytes.len() <= crate::microphone::BUFFER_BYTES && bytes.len() % 2 == 0);
        let state = self.0.capture.lock().unwrap();
        let sequence = self.0.attempted_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let result = match state.sender.as_ref() {
            Some(sender) => {
                let result = sender
                    .try_send(CaptureBuffer(bytes.to_vec()))
                    .map_err(|error| match error {
                        mpsc::error::TrySendError::Full(_) => AdmissionError::Overflow,
                        mpsc::error::TrySendError::Closed(_) => AdmissionError::WorkerFailure,
                    });
                if !sender.is_closed() {
                    self.0.observed_queue_slots.fetch_max(
                        crate::microphone::OUTPUT_BUFFER_COUNT - sender.capacity(),
                        Ordering::AcqRel,
                    );
                }
                result
            }
            None => Err(AdmissionError::NotRunning),
        };
        match result {
            Ok(()) => {
                self.0
                    .accepted_bytes
                    .fetch_add(bytes.len() as u64, Ordering::AcqRel);
                self.0
                    .last_accepted_sequence
                    .store(sequence, Ordering::Release);
            }
            Err(error) => {
                self.0.admission_failure.get_or_init(|| {
                    let cursors = self.0.capture_trace.get().map(|trace| trace.snapshot());
                    serde_json::json!({
                        "status":"failed-admission","error":error.to_string(),
                        "kind":format!("{error:?}"),
                        "first_failure_is_immutable":true,
                        "producer_must_stop_after_this_failure_for_final_accepted_count":true,
                        "attempted_sequence":sequence,
                        "last_accepted_sequence":self.0.last_accepted_sequence.load(Ordering::Acquire),
                        "accepted_bytes":self.0.accepted_bytes.load(Ordering::Acquire),
                        "rejected_bytes":bytes.len(),
                        "sender_present":state.sender.is_some(),
                        "writer_received_bytes":self.0.writer_received_bytes.load(Ordering::Acquire),
                        "writer_completed_bytes":self.0.writer_completed_bytes.load(Ordering::Acquire),
                        "last_published_saved_bytes":cursors.as_ref().map(|value|value["saved_bytes"].clone()),
                        "last_published_confirmed_bytes":cursors.as_ref().map(|value|value["confirmed_bytes"].clone()),
                        "cursors_are_not_a_fresh_disk_snapshot":true,
                        "not_a_success_or_accepted_packet":true
                    })
                });
            }
        }
        result
    }
}

impl DictationPorts for Ports {
    type Backend = Backend;
    type Refiner = Refiner;
    type Capture = Capture;

    fn auth_state(&self) -> watch::Receiver<AuthState> {
        self.0.auth.subscribe()
    }

    async fn recovery_context(&self, backend: &str) -> Option<String> {
        assert_eq!(backend, URL);
        (*self.0.auth.borrow() == AuthState::SignedIn).then(|| format!("{URL}\n{EMAIL}"))
    }

    fn backend(&self, label: String) -> Result<Arc<Backend>, NativeError> {
        assert_eq!(label, URL);
        Ok(self.0.backend.clone())
    }

    fn refiner(&self, email: String) -> Arc<Refiner> {
        assert_eq!(email, EMAIL);
        self.0.refiner.clone()
    }

    fn start_capture(&self) -> Result<(Capture, mpsc::Receiver<CaptureBuffer>), CaptureError> {
        self.0.opened.fetch_add(1, Ordering::AcqRel);
        if self.0.fail_open.load(Ordering::Acquire) {
            return Err(CaptureError::Worker);
        }
        if let Some(prepare) = self.0.prepare_capture.lock().unwrap().take() {
            prepare();
        }
        let receiver = self
            .0
            .capture
            .lock()
            .unwrap()
            .receiver
            .take()
            .ok_or(CaptureError::Worker)?;
        Ok((Capture(self.0.clone()), receiver))
    }

    fn stop_capture(capture: Capture) -> Result<(), CaptureError> {
        capture.0.stopped.fetch_add(1, Ordering::AcqRel);
        let mut state = capture.0.capture.lock().unwrap();
        let sender = state.sender.take().ok_or(CaptureError::Worker)?;
        if !state.tail.is_empty() {
            sender
                .try_send(CaptureBuffer(std::mem::take(&mut state.tail)))
                .map_err(|_| CaptureError::Overflow)?;
        }
        drop(sender);
        if capture.0.fail_stop.load(Ordering::Acquire) {
            Err(CaptureError::Worker)
        } else {
            Ok(())
        }
    }

    fn capture_write_started(&self, bytes: usize) {
        self.0
            .writer_received_bytes
            .fetch_add(bytes as u64, Ordering::AcqRel);
        self.0
            .writer_packet_bytes
            .store(bytes as u64, Ordering::Release);
        self.0
            .writer_started_ns
            .store(self.0.clock.elapsed().as_nanos() as u64, Ordering::Release);
        self.0.writer_active.store(true, Ordering::Release);
    }

    fn capture_write_finished(&self, success: bool) {
        let elapsed = (self.0.clock.elapsed().as_nanos() as u64)
            .saturating_sub(self.0.writer_started_ns.load(Ordering::Acquire));
        self.0
            .writer_last_duration_ns
            .store(elapsed, Ordering::Release);
        if success {
            self.0.writer_completed_bytes.fetch_add(
                self.0.writer_packet_bytes.load(Ordering::Acquire),
                Ordering::AcqRel,
            );
        }
        self.0.writer_active.store(false, Ordering::Release);
    }

    fn register_capture_trace(&self, trace: Arc<crate::lock_trace::Lock>) {
        assert!(
            self.0.capture_trace.set(trace).is_ok(),
            "Synthetic capture trace can be registered once."
        );
    }
}

struct Harness {
    worker: Option<tokio::task::JoinHandle<()>>,
    ports: Ports,
    commands: mpsc::Sender<DictationCommand>,
    shutdown: watch::Sender<bool>,
    settings: watch::Sender<AppSettings>,
    status: watch::Receiver<ClientStatus>,
    ui: mpsc::Receiver<UiRequest>,
    history: Arc<NativeHistoryStore>,
    recovery: NativeRecoveryStore,
    policy: Arc<DeliveryPolicy>,
    root: PathBuf,
    _directory: Option<TemporaryRoot>,
}

impl Harness {
    fn new(streaming: bool, refinement: bool) -> Self {
        let directory = TemporaryRoot::new();
        Self::new_at(directory.0.clone(), streaming, refinement, Some(directory))
    }

    fn new_at(
        root: PathBuf,
        streaming: bool,
        refinement: bool,
        directory: Option<TemporaryRoot>,
    ) -> Self {
        let recovery = open_recovery(root.join("recovery")).unwrap();
        let history = Arc::new(NativeHistoryStore::new(root.join("history.json")).unwrap());
        let ports = Ports::new();
        let (commands, command_receiver) = mpsc::channel(8);
        let (shutdown, stopping) = watch::channel(false);
        let (settings, configured) = watch::channel(AppSettings {
            backend_base_url: URL.into(),
            dictation_enabled: true,
            dictation_language: "cs".into(),
            dictation_streaming_enabled: streaming,
            dictation_refinement_enabled: refinement,
            ..Default::default()
        });
        let (status, observed) = watch::channel(ClientStatus::default());
        let (requests, ui) = mpsc::channel(8);
        let policy = Arc::new(DeliveryPolicy::default());
        let worker = tokio::spawn(
            DictationHost {
                ports: ports.clone(),
                recovery: recovery.clone(),
                history: history.clone(),
                settings: configured,
                status,
                ui: requests,
                policy: policy.clone(),
                commands: command_receiver,
                shutdown: stopping,
                devices_enabled: true,
            }
            .run(),
        );
        Self {
            worker: Some(worker),
            ports,
            commands,
            shutdown,
            settings,
            status: observed,
            ui,
            history,
            recovery,
            policy,
            root,
            _directory: directory,
        }
    }

    async fn begin(&mut self, mode: DictationMode) {
        self.commands
            .send(DictationCommand::Begin {
                generation: 1,
                mode,
            })
            .await
            .unwrap();
        self.stage(DictationStage::Recording).await;
        assert!(self.policy.dictation_busy());
    }

    async fn stage(&mut self, stage: DictationStage) {
        self.view(|view| view.stage == stage).await;
    }

    async fn view(&mut self, predicate: impl Fn(&DictationView) -> bool) {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if predicate(&self.status.borrow_and_update().dictation) {
                    return;
                }
                self.status.changed().await.unwrap();
            }
        })
        .await;
        if let Err(error) = result {
            self.close().await;
            panic!("Actual controller did not publish the expected stage: {error}");
        }
    }

    async fn idle(&mut self) {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            while self.policy.dictation_busy() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        if let Err(error) = result {
            self.close().await;
            panic!("Controller did not release its busy policy: {error}");
        }
    }

    async fn stop(&self) {
        self.commands
            .send(DictationCommand::Stop { generation: 1 })
            .await
            .unwrap();
    }

    async fn feed_many(&self, buffer: &[u8], count: usize) {
        for _ in 0..count {
            self.ports.feed(buffer).await;
        }
    }

    async fn delivery(&mut self) -> (String, Arc<AtomicBool>, oneshot::Sender<DeliveryResult>) {
        let request = match tokio::time::timeout(Duration::from_secs(5), self.ui.recv()).await {
            Ok(Some(request)) => request,
            Ok(None) => {
                self.close().await;
                panic!("Controller UI channel closed before final handoff.");
            }
            Err(error) => {
                self.close().await;
                panic!("Controller never handed off the final text: {error}");
            }
        };
        match request {
            UiRequest::Deliver {
                generation,
                text,
                cancelled,
                reply,
            } => {
                assert_eq!(generation, 1);
                (text, cancelled, reply)
            }
            _ => panic!("Unexpected controller UI request."),
        }
    }

    fn entries(&self) -> Vec<HistoryEntry> {
        self.history.all(time::OffsetDateTime::now_utc()).unwrap()
    }

    async fn close(&mut self) {
        self.shutdown.send_replace(true);
        tokio::time::timeout(Duration::from_secs(5), self.worker.take().unwrap())
            .await
            .expect("Controller shutdown did not join.")
            .unwrap();
        assert!(!self.policy.dictation_busy());
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.shutdown.send_replace(true);
            worker.abort();
        }
    }
}

fn pcm(samples: usize) -> Vec<u8> {
    (0..samples)
        .flat_map(|index| {
            let value: i16 = if index % 2 == 0 { 12000 } else { -12000 };
            value.to_le_bytes()
        })
        .collect()
}

fn volume_pcm(samples: usize) -> Vec<u8> {
    (0..samples)
        .flat_map(|index| {
            let value: i16 = if index % 2 == 0 { 12 } else { -12 };
            value.to_le_bytes()
        })
        .collect()
}

const HOUR_SCOPE: &str = "native-controller-realtime-synthetic-pcm";

fn hour_publish(root: &std::path::Path, name: &str, value: serde_json::Value) {
    crate::platform::atomic_write(
        &root.join(name),
        &serde_json::to_vec(&value).unwrap(),
        crate::platform::WriteMode::CreateNew,
    )
    .unwrap();
}

async fn hour_control(root: &std::path::Path, name: &str, nonce: &str) {
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let path = root.join(name);
            crate::platform::guard_recovery_path(&path).unwrap();
            if path.try_exists().unwrap() {
                assert_eq!(std::fs::read_to_string(path).unwrap(), nonce);
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Synthetic controller-hour observer did not respond.");
}

struct HourUpload(Arc<AtomicUsize>);
impl crate::audit_outbox::AuditUploader for HourUpload {
    async fn context(&self) -> Option<String> {
        Some(format!("{URL}\n{EMAIL}"))
    }

    async fn upload(
        &self,
        state: &voiceprompt_client_protocol::recovery::RecoveryManifest,
    ) -> Result<(), ApiError> {
        assert_eq!(state.context, format!("{URL}\n{EMAIL}"));
        assert!(state.capture_complete);
        assert_eq!(state.captured_bytes, state.confirmed_bytes);
        assert!(state.audit.is_some());
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

struct CaptureMonitor {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl CaptureMonitor {
    fn sample(
        probe: &Probe,
        recovery: &NativeRecoveryStore,
        started: Instant,
        profile: CaptureProfile,
    ) -> serde_json::Value {
        let engine = probe
            .capture_trace
            .get()
            .expect("Capture trace registered before observer")
            .snapshot();
        let accepted = probe.accepted_bytes.load(Ordering::Acquire);
        let received = probe.writer_received_bytes.load(Ordering::Acquire);
        serde_json::json!({
            "phase":"capturing","engine":profile.engine(),"pattern":profile.pattern(),
            "elapsed_seconds":started.elapsed().as_secs_f64(),
            "captured_bytes":accepted,"accepted_bytes":accepted,"writer_received_bytes":received,
            "writer_completed_bytes":probe.writer_completed_bytes.load(Ordering::Acquire),
            "saved_bytes":engine["saved_bytes"],"confirmed_bytes":engine["confirmed_bytes"],
            "provider_bytes":if profile == CaptureProfile::Streaming { Some(probe.backend.0.stream_bytes.load(Ordering::Acquire)) } else { None },
            "observed_max_queue_slots":probe.observed_queue_slots.load(Ordering::Acquire),
            "observed_queued_bytes":accepted.saturating_sub(received),
            "engine_lock":engine,"recovery_io":recovery.fixture_io_progress(),
            "observer_uses_no_target_mutex":true
        })
    }

    fn start(
        harness: &Harness,
        seconds: u64,
        root: &std::path::Path,
        started: Instant,
        profile: CaptureProfile,
    ) -> Self {
        let root = root.to_path_buf();
        let probe = harness.ports.0.clone();
        let recovery = harness.recovery.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("voiceprompt-noports-observer".into())
            .spawn(move || {
                for point in (10..=seconds).step_by(10) {
                    let due = started + Duration::from_secs(point);
                    while Instant::now() < due {
                        if stopped.load(Ordering::Acquire) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    if stopped.load(Ordering::Acquire) {
                        return;
                    }
                    hour_publish(
                        &root,
                        &format!("progress-{point:04}.json"),
                        Self::sample(&probe, &recovery, started, profile),
                    );
                }
            })
            .unwrap();
        Self { stop, thread }
    }

    async fn finish(self, aborted: bool) {
        if aborted {
            self.stop.store(true, Ordering::Release);
        }
        tokio::task::spawn_blocking(move || self.thread.join().unwrap())
            .await
            .unwrap();
    }
}

async fn run_controller_capture(
    harness: &mut Harness,
    seconds: u64,
    diagnostics: Option<&std::path::Path>,
    profile: CaptureProfile,
) -> serde_json::Value {
    if profile == CaptureProfile::Legacy {
        *harness.ports.0.backend.0.legacy.lock().unwrap() = Some(LegacyOracle::new(seconds));
    }
    let own_io = harness.recovery.fixture_own_io();
    let probe = Arc::downgrade(&harness.ports.0);
    own_io.set_admission(move || {
        probe.upgrade().map_or_else(
            || serde_json::json!({"unavailable":"capture probe released"}),
            |probe| {
                serde_json::json!({
                    "observed_probe_monotonic_ns":probe.clock.elapsed().as_nanos() as u64,
                    "attempted_sequence":probe.attempted_sequence.load(Ordering::Acquire),
                    "last_accepted_sequence":probe.last_accepted_sequence.load(Ordering::Acquire),
                    "accepted_bytes":probe.accepted_bytes.load(Ordering::Acquire),
                    "received_bytes":probe.writer_received_bytes.load(Ordering::Acquire),
                    "completed_append_bytes":probe.writer_completed_bytes.load(Ordering::Acquire),
                    "writer_active":probe.writer_active.load(Ordering::Acquire)
                })
            },
        )
    });
    if let Some(root) = diagnostics {
        own_io.set_sink(root.join("first-rename-failure.json"));
    }
    harness
        .ports
        .0
        .backend
        .0
        .count_only
        .store(true, Ordering::Release);
    harness
        .ports
        .0
        .backend
        .0
        .ordered_samples
        .store(true, Ordering::Release);
    harness.begin(DictationMode::Toggle).await;
    let started = tokio::time::Instant::now();
    let monitor = diagnostics
        .map(|root| CaptureMonitor::start(harness, seconds, root, started.into_std(), profile));
    const FRAME_BYTES: usize = crate::microphone::BUFFER_BYTES;
    const FRAME_MILLISECONDS: u64 = FRAME_BYTES as u64 * 1000 / 32000;
    const FRAMES_PER_SECOND: u64 = 1000 / FRAME_MILLISECONDS;
    let mut timer = tokio::time::interval(Duration::from_millis(FRAME_MILLISECONDS));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let mut maximum_lateness = Duration::ZERO;
    let mut frame = [0; FRAME_BYTES];
    for index in 0..seconds * FRAMES_PER_SECOND {
        timer.tick().await;
        let due = started + Duration::from_millis(index * FRAME_MILLISECONDS);
        maximum_lateness =
            maximum_lateness.max(tokio::time::Instant::now().saturating_duration_since(due));
        for (offset, bytes) in frame.chunks_exact_mut(2).enumerate() {
            let sample = profile.sample(index * (FRAME_BYTES / 2) as u64 + offset as u64);
            bytes.copy_from_slice(&sample.to_le_bytes());
        }
        if let Err(error) = harness.ports.try_feed(&frame) {
            if let Some(root) = diagnostics {
                let probe = &harness.ports.0;
                let active = probe.writer_active.load(Ordering::Acquire);
                let age = (probe.clock.elapsed().as_nanos() as u64)
                    .saturating_sub(probe.writer_started_ns.load(Ordering::Acquire));
                hour_publish(
                    root,
                    "producer-failure.json",
                    serde_json::json!({
                        "phase":"producer-overflow-or-closed","error":error.to_string(),
                        "frame_index":index,"frame_bytes":FRAME_BYTES,
                        "frame_interval_ms":FRAME_MILLISECONDS,
                        "elapsed_seconds":started.elapsed().as_secs_f64(),
                        "lateness_ms":tokio::time::Instant::now().saturating_duration_since(due).as_secs_f64()*1000.0,
                        "maximum_producer_lateness_ms":maximum_lateness.as_secs_f64()*1000.0,
                        "produced_bytes":index*FRAME_BYTES as u64,
                        "source_queue_slots":crate::microphone::OUTPUT_BUFFER_COUNT,
                        "capture_sample_bytes_not_dropped_as_success":true,
                        "accepted_bytes":probe.accepted_bytes.load(Ordering::Acquire),
                        "writer_received_bytes":probe.writer_received_bytes.load(Ordering::Acquire),
                        "writer_completed_bytes":probe.writer_completed_bytes.load(Ordering::Acquire),
                        "writer_active":active,"writer_active_age_ms":if active {Some(age as f64/1_000_000.0)}else{None},
                        "writer_last_duration_ms":probe.writer_last_duration_ns.load(Ordering::Acquire) as f64/1_000_000.0,
                        "recovery_io":harness.recovery.fixture_io_progress()
                        ,"engine_lock":probe.capture_trace.get().map(|trace|trace.snapshot())
                        ,"observed_max_queue_slots":probe.observed_queue_slots.load(Ordering::Acquire)
                        ,"immutable_admission_failure":probe.admission_failure.get()
                        ,"first_rename_failure":own_io.first_record()
                    }),
                );
            }
            if let Some(monitor) = monitor {
                monitor.finish(true).await;
            }
            harness.close().await;
            panic!("Synthetic capture producer failed: {error}");
        }
    }
    // The first timer tick is immediate; wait through the final frame's full duration.
    tokio::time::sleep_until(started + Duration::from_secs(seconds)).await;
    frame.fill(0);
    let elapsed = started.elapsed().as_secs_f64();
    if let Some(root) = diagnostics {
        hour_publish(
            root,
            "producer-complete.json",
            serde_json::json!({
                "phase":"producer-complete","elapsed_seconds":elapsed,
                "captured_bytes":seconds*32000
            }),
        );
    }
    harness.stop().await;
    let (text, cancelled, reply) = harness.delivery().await;
    assert_eq!(harness.entries()[0].body, text);
    assert!(!cancelled.load(Ordering::Acquire));
    assert!(harness.policy.dictation_busy());
    let rows = harness.recovery.list().unwrap();
    assert_eq!(rows.len(), 1);
    let state = harness.recovery.fixture_snapshot(&rows[0].id).unwrap();
    let expected = seconds * 32000;
    assert_eq!(state.captured_bytes, expected);
    assert!(state.capture_complete);
    assert!(harness.ports.0.backend.0.batches.lock().unwrap().is_empty());
    let starts = harness.ports.0.backend.0.starts.lock().unwrap().clone();
    let cursors = harness
        .ports
        .0
        .backend
        .0
        .confirmed_cursors
        .lock()
        .unwrap()
        .clone();
    let mut legacy_result = None;
    let provider_bytes = if profile == CaptureProfile::Streaming {
        assert_eq!(state.confirmed_bytes, expected);
        assert!(state.audit.is_some());
        assert_eq!(
            harness
                .ports
                .0
                .backend
                .0
                .stream_bytes
                .load(Ordering::Acquire),
            expected
        );
        assert_eq!(starts[0], 0);
        assert!(cursors.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(cursors.last().copied(), Some(expected));
        if seconds > 3000 {
            assert!(
                starts.len() >= 2,
                "Current orchestration never exercised real50min rotation."
            );
        }
        expected
    } else {
        assert!(!state.streaming && state.audit.is_none() && state.tail.is_empty());
        assert_eq!(state.confirmed_bytes, 0);
        assert!(starts.is_empty() && cursors.is_empty());
        assert_eq!(
            harness
                .ports
                .0
                .backend
                .0
                .stream_bytes
                .load(Ordering::Acquire),
            0
        );
        let configured = harness.ports.0.backend.0.legacy.lock().unwrap();
        let oracle = configured.as_ref().unwrap();
        assert!(oracle.seen.iter().all(|seen| *seen));
        assert_eq!(state.chunks.len(), oracle.chunks.len());
        for (actual, expected) in state.chunks.iter().zip(&oracle.chunks) {
            assert_eq!(
                actual
                    .digest
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
                expected.digest
            );
            assert_eq!(actual.audio_bytes as usize, expected.wav_bytes);
            assert_eq!(actual.end_byte, expected.end_byte);
            assert_eq!(actual.overlaps_previous, expected.overlaps_previous);
            assert_eq!(actual.text.as_deref(), Some(expected.text.as_str()));
        }
        if oracle.chunks.len() >= 2 {
            assert_eq!(
                &oracle.completions[..2],
                &[1, 0],
                "The provider did not complete its first pair out of order."
            );
        }
        let original = voiceprompt_client_protocol::dictation_text::stitch(
            oracle
                .chunks
                .iter()
                .map(|chunk| (chunk.text.as_str(), chunk.overlaps_previous)),
        );
        assert_eq!(
            harness.entries()[0].raw_body.as_deref(),
            Some(original.as_str())
        );
        assert_eq!(oracle.chunks.last().unwrap().end_byte, expected);
        let bytes = oracle
            .chunks
            .iter()
            .map(|chunk| chunk.wav_bytes as u64 - 44)
            .sum::<u64>();
        assert_ne!(
            bytes, expected,
            "The legacy profile must exercise real silence gating/overlap, not all-raw WAV concatenation."
        );
        let chunks: Vec<_> = oracle
            .chunks
            .iter()
            .map(|chunk| {
                serde_json::json!({
                    "digest":chunk.digest,"wav_bytes":chunk.wav_bytes,"end_byte":chunk.end_byte,
                    "overlaps_previous":chunk.overlaps_previous,"text":chunk.text
                })
            })
            .collect();
        legacy_result = Some(serde_json::json!({
            "chunks":chunks,"provider_completion_order":oracle.completions,
            "original_text":original,"expected_tail_audio_bytes":oracle.final_tail_audio_bytes,
            "expected_tail_frame_bytes":0,"final_manifest_tail_bytes":state.tail.len()
        }));
        bytes
    };
    assert_eq!(harness.ports.0.opened.load(Ordering::Acquire), 1);
    assert_eq!(harness.ports.0.stopped.load(Ordering::Acquire), 1);
    assert!(harness.ports.0.refiner.calls.load(Ordering::Acquire) > 0);
    reply.send(DeliveryResult::CopiedOnly).unwrap();
    harness.stage(DictationStage::PasteSkipped).await;
    harness.idle().await;
    if let Some(monitor) = monitor {
        monitor.finish(false).await;
    }
    let mut result = serde_json::json!({
        "engine":profile.engine(),"pattern":profile.pattern(),
        "audio_seconds":seconds,"actual_capture_seconds":elapsed,
        "captured_bytes":expected,"confirmed_bytes":state.confirmed_bytes,
        "verified_provider_bytes":provider_bytes,"stream_starts":starts,
        "confirmed_cursors":cursors,"maximum_producer_lateness_ms":maximum_lateness.as_secs_f64()*1000.0,
        "history_saved_before_handoff":true,"capture_joined":true,"polisher_exercised":true,
        "delivery_acknowledged":true
        ,"capture_frame_bytes":FRAME_BYTES,"capture_frame_interval_ms":FRAME_MILLISECONDS
        ,"accepted_bytes":harness.ports.0.accepted_bytes.load(Ordering::Acquire),
        "writer_received_bytes":harness.ports.0.writer_received_bytes.load(Ordering::Acquire),
        "writer_completed_bytes":harness.ports.0.writer_completed_bytes.load(Ordering::Acquire),
        "observed_max_queue_slots":harness.ports.0.observed_queue_slots.load(Ordering::Acquire),
        "engine_lock":harness.ports.0.capture_trace.get().map(|trace|trace.snapshot()),
        "recovery_io":harness.recovery.fixture_io_progress()
    });
    if let Some(legacy) = legacy_result {
        result["legacy"] = legacy;
    }
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_realtime_controller_smoke_joins_exact_pcm_and_outbox_without_physical_ports() {
    let mut harness = Harness::new(true, true);
    let outbox = HourOutbox::start(&harness);
    let result = run_controller_capture(&mut harness, 2, None, CaptureProfile::Streaming).await;
    for key in [
        "accepted_bytes",
        "writer_received_bytes",
        "writer_completed_bytes",
        "confirmed_bytes",
    ] {
        assert_eq!(result[key], 64000);
    }
    assert!(harness.ports.0.admission_failure.get().is_none());
    hour_cleanup(&mut harness, &outbox).await;
    harness.close().await;
    outbox.close().await;
    println!("short-controller-smoke: {result}; scope=2s-NoPorts-only; hour-acceptance=false");
}

#[test]
fn controller_capture_profile_rejects_unknown_and_cross_engine_patterns() {
    for (engine, pattern) in [
        ("", "ordered-quiet-v1"),
        ("unknown", "ordered-quiet-v1"),
        ("legacy", "ordered-quiet-v1"),
        ("streaming", "voiced-pause-overlap-v1"),
    ] {
        assert!(CaptureProfile::parse(engine, pattern).is_err());
    }
    for profile in [CaptureProfile::Streaming, CaptureProfile::Legacy] {
        assert_eq!(
            CaptureProfile::parse(profile.engine(), profile.pattern()),
            Ok(profile)
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_realtime_legacy_controller_uses_source_oracle_delivery_ack_and_no_audit() {
    let mut harness = Harness::new(false, true);
    let outbox = HourOutbox::start(&harness);
    let result = run_controller_capture(&mut harness, 3, None, CaptureProfile::Legacy).await;
    assert_eq!(result["captured_bytes"], 96_000);
    assert_eq!(result["confirmed_bytes"], 0);
    assert!(result["delivery_acknowledged"].as_bool().unwrap());
    hour_cleanup(&mut harness, &outbox).await;
    harness.close().await;
    outbox.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_profile_two_request_out_of_order_binding_matches_complete_source_oracle() {
    let mut harness = Harness::new(false, true);
    *harness.ports.0.backend.0.legacy.lock().unwrap() = Some(LegacyOracle::new(12));
    harness.begin(DictationMode::Toggle).await;
    let input: Vec<_> = (0..12 * 16_000)
        .flat_map(|index| CaptureProfile::Legacy.sample(index).to_le_bytes())
        .collect();
    harness.ports.feed(&input).await;
    harness.stop().await;
    let (text, cancelled, reply) = harness.delivery().await;
    assert_eq!(harness.entries()[0].body, text);
    assert!(!cancelled.load(Ordering::Acquire));
    {
        let configured = harness.ports.0.backend.0.legacy.lock().unwrap();
        let oracle = configured.as_ref().unwrap();
        assert!(oracle.seen.iter().all(|seen| *seen));
        assert_eq!(&oracle.completions[..2], &[1, 0]);
        let expected = voiceprompt_client_protocol::dictation_text::stitch(
            oracle
                .chunks
                .iter()
                .map(|chunk| (chunk.text.as_str(), chunk.overlaps_previous)),
        );
        assert_eq!(
            harness.entries()[0].raw_body.as_deref(),
            Some(expected.as_str())
        );
    }
    reply.send(DeliveryResult::CopiedOnly).unwrap();
    harness.stage(DictationStage::PasteSkipped).await;
    harness.idle().await;
    assert!(harness.recovery.list().unwrap().is_empty());
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_realtime_storage_failure_publishes_exact_producer_context_before_join_and_panic() {
    for export_producer_context in [true, false] {
        controlled_realtime_storage_failure(export_producer_context).await;
    }
}

async fn controlled_realtime_storage_failure(export_producer_context: bool) {
    let directory = TemporaryRoot::new();
    let diagnostics = directory.0.join("diagnostics");
    std::fs::create_dir(&diagnostics).unwrap();
    let mut harness = Harness::new_at(directory.0.join("state"), true, true, None);
    if !export_producer_context {
        harness
            .recovery
            .fixture_own_io()
            .set_sink(diagnostics.join("first-rename-failure.json"));
    }
    let recovery = harness.recovery.clone();
    let root = harness.root.clone();
    let held = Arc::new(std::sync::OnceLock::new());
    let locked = held.clone();
    *harness.ports.0.prepare_capture.lock().unwrap() = Some(Box::new(move || {
        let rows = recovery.list().unwrap();
        assert_eq!(rows.len(), 1);
        let path =
            std::fs::canonicalize(root.join("recovery").join(&rows[0].id).join("manifest.enc"))
                .unwrap();
        let before = std::fs::read(&path).unwrap();
        let trace = recovery.fixture_own_io();
        let opening = trace.opening(
            &path,
            "controlled-live-manifest-read",
            crate::io_trace::READ_ACCESS,
            1,
            0,
        );
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let handle = trace.track(
            handle,
            &path,
            "controlled-live-manifest-read",
            crate::io_trace::READ_ACCESS,
            1,
            0,
        );
        drop(opening);
        assert!(locked.set((handle, path, before)).is_ok());
    }));
    let output = diagnostics.clone();
    let producer = tokio::spawn(async move {
        run_controller_capture(
            &mut harness,
            3,
            export_producer_context.then_some(output.as_path()),
            CaptureProfile::Streaming,
        )
        .await
    });
    let result = tokio::time::timeout(Duration::from_secs(10), producer)
        .await
        .unwrap();
    assert!(
        result.unwrap_err().is_panic(),
        "Storage failure cannot look like successful capture."
    );
    let (_, path, original_cipher) = held
        .get()
        .expect("The synthetic file fault was not prepared before capture.");
    if !export_producer_context {
        let witness: serde_json::Value = serde_json::from_slice(
            &std::fs::read(diagnostics.join("first-rename-failure.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            witness["owner_classification"],
            "OWN_LIVE_DESTINATION_HANDLE_OBSERVED"
        );
        assert_eq!(witness["HRESULT"], "0x80070005");
        assert!(!diagnostics.join("producer-failure.json").exists());
        assert_eq!(&std::fs::read(path).unwrap(), original_cipher);
        return;
    }
    let failure: serde_json::Value =
        serde_json::from_slice(&std::fs::read(diagnostics.join("producer-failure.json")).unwrap())
            .unwrap();
    let admission = &failure["immutable_admission_failure"];
    assert_eq!(admission["kind"], "NotRunning");
    let accepted = admission["accepted_bytes"].as_u64().unwrap();
    let sequence = admission["last_accepted_sequence"].as_u64().unwrap();
    assert_eq!(accepted, sequence * 1280);
    assert_eq!(
        admission["attempted_sequence"].as_u64().unwrap(),
        sequence + 1
    );
    assert_eq!(failure["frame_index"].as_u64().unwrap(), sequence);
    assert_eq!(failure["accepted_bytes"], admission["accepted_bytes"]);
    assert_eq!(failure["produced_bytes"], accepted);
    let completed = admission["writer_completed_bytes"].as_u64().unwrap();
    assert!(completed >= 64_000 && completed <= accepted);
    assert_eq!(admission["last_published_saved_bytes"], 0);
    assert_eq!(admission["last_published_confirmed_bytes"], 0);
    let witness: serde_json::Value = serde_json::from_slice(
        &std::fs::read(diagnostics.join("first-rename-failure.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(failure["first_rename_failure"], witness);
    assert_eq!(
        witness["owner_classification"],
        "OWN_LIVE_DESTINATION_HANDLE_OBSERVED"
    );
    assert_eq!(witness["HRESULT"], "0x80070005");
    assert_eq!(
        witness["admission"]["accepted_bytes"].as_u64().unwrap() % 1280,
        0
    );
    assert_eq!(&std::fs::read(path).unwrap(), original_cipher);
    assert!(!diagnostics.join("producer-complete.json").exists());
    assert!(!diagnostics.join("transcription-complete.json").exists());
    println!(
        "short-controller-failed-producer-evidence: {failure}; durable=0; all-admitted-saved=false; no retry"
    );
}

struct HourOutbox {
    engine: Arc<crate::audit_outbox::AuditOutbox<HourUpload>>,
    uploads: Arc<AtomicUsize>,
    stop: watch::Sender<bool>,
    worker: tokio::task::JoinHandle<()>,
}

impl HourOutbox {
    fn start(harness: &Harness) -> Self {
        let uploads = Arc::new(AtomicUsize::new(0));
        let engine = Arc::new(crate::audit_outbox::AuditOutbox::new(
            harness.recovery.clone(),
            harness.history.clone(),
            Arc::new(HourUpload(uploads.clone())),
        ));
        let (stop, mut stopping) = watch::channel(false);
        let policy = harness.policy.clone();
        let background = engine.clone();
        let worker = tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = stopping.changed() => return,
                    _ = timer.tick() => {
                        let result = background.drain(&policy, time::OffsetDateTime::now_utc()).await.unwrap();
                        assert_eq!(result.failed, 0);
                    }
                }
            }
        });
        Self {
            engine,
            uploads,
            stop,
            worker,
        }
    }

    async fn close(self) {
        self.stop.send_replace(true);
        self.worker.await.unwrap();
    }
}

async fn hour_cleanup(harness: &mut Harness, outbox: &HourOutbox) {
    let result = outbox
        .engine
        .drain(&harness.policy, time::OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert_eq!(result.failed, 0);
    if !harness.settings.borrow().dictation_streaming_enabled {
        assert_eq!(result.acknowledged, 0);
        assert_eq!(outbox.uploads.load(Ordering::Acquire), 0);
        assert!(harness.recovery.list().unwrap().is_empty());
        assert_eq!(harness.entries().len(), 1);
        assert!(harness.ui.try_recv().is_err());
        harness.history.clear().unwrap();
        return;
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while outbox.uploads.load(Ordering::Acquire) == 0
            || !harness.recovery.list().unwrap().is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(outbox.uploads.load(Ordering::Acquire), 1);
    assert!(harness.recovery.list().unwrap().is_empty());
    assert_eq!(harness.entries().len(), 1);
    assert!(
        harness.ui.try_recv().is_err(),
        "Audit must not queue automatic paste."
    );
    harness.history.clear().unwrap();
    assert!(harness.entries().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "One marked-root current controller wall-clock capture; physical, GUI, account and real network ports absent."]
async fn realtime_synthetic_controller_hour() {
    let root = std::env::var_os("VOICEPROMPT_SYNTHETIC_HOUR_DIRECTORY")
        .map(PathBuf::from)
        .expect("An explicit synthetic observer directory is required.");
    crate::fixtures::validate(&root, None).unwrap();
    let options: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("capture-options.json")).unwrap()).unwrap();
    assert_eq!(options.as_object().unwrap().len(), 5);
    assert_eq!(options["scope"], HOUR_SCOPE);
    let profile = CaptureProfile::parse(
        options["engine"].as_str().unwrap(),
        options["pattern"].as_str().unwrap(),
    )
    .unwrap();
    let seconds = options["audio_seconds"].as_u64().unwrap();
    assert!((2..=3600).contains(&seconds));
    assert!(profile == CaptureProfile::Streaming || matches!(seconds, 3 | 600));
    let nonce = options["nonce"].as_str().unwrap();
    crate::fixtures::validate(&root, Some(nonce)).unwrap();
    let expected_names = ["capture-options.json", "migration-fixture.json"];
    let mut names: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, expected_names);
    let owner_trace = std::env::var("VOICEPROMPT_LOCK_TRACE").as_deref() == Ok("owner-phases");
    if owner_trace {
        crate::lock_trace::verify_fixture_symbols();
    }
    let state_root = root.join("controller-state");
    let mut warm = Harness::new_at(
        state_root.clone(),
        profile == CaptureProfile::Streaming,
        true,
        None,
    );
    warm.recovery
        .fixture_own_io()
        .set_sink(root.join("first-rename-failure.json"));
    let warm_outbox = HourOutbox::start(&warm);
    run_controller_capture(&mut warm, 2, None, profile).await;
    hour_cleanup(&mut warm, &warm_outbox).await;
    warm.close().await;
    warm_outbox.close().await;
    drop(warm);
    if owner_trace {
        crate::lock_trace::freeze_context_capture();
        hour_publish(
            &root,
            "lock-trace-ready.json",
            serde_json::json!({
                "known_fixture_symbols_verified":true,"stack_capture_frozen_after_warmup":true,
                "max_contexts":64,"max_stack_bytes":16384,"acquisition_context_only":true,
                "not_current_suspended_stack":true
            }),
        );
    }
    let mut harness = Harness::new_at(
        state_root.clone(),
        profile == CaptureProfile::Streaming,
        true,
        None,
    );
    let outbox = HourOutbox::start(&harness);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!harness.policy.dictation_busy());
    assert!(harness.entries().is_empty() && harness.recovery.list().unwrap().is_empty());
    hour_publish(
        &root,
        "ready.json",
        serde_json::json!({
            "scope":HOUR_SCOPE,"engine":profile.engine(),"pattern":profile.pattern(),
            "pid":std::process::id(),"physical_ports":false,
            "network_or_auth":false,"full_client_profile":false,"current_controller":true,
            "renderer":false,"main_ui_or_tray":false,"real_auth_or_pubsub":false,
            "polisher_and_outbox":true
        }),
    );
    hour_control(&root, "begin", nonce).await;
    let mut result = run_controller_capture(&mut harness, seconds, Some(&root), profile).await;
    hour_publish(
        &root,
        "transcription-complete.json",
        serde_json::json!({
            "phase":"transcription-complete","engine":profile.engine(),
            "confirmed_bytes":result["confirmed_bytes"]
        }),
    );
    hour_cleanup(&mut harness, &outbox).await;
    assert!(!harness.policy.dictation_busy());
    assert!(harness.entries().is_empty() && harness.recovery.list().unwrap().is_empty());
    hour_publish(
        &root,
        "shutdown-complete.json",
        serde_json::json!({
            "phase":"capture-stopped-and-joined","controller_and_outbox_still_running":true
        }),
    );
    result["scope"] = HOUR_SCOPE.into();
    result["current_controller"] = true.into();
    result["renderer"] = false.into();
    result["outbox_acknowledged"] = if profile == CaptureProfile::Streaming {
        true.into()
    } else {
        serde_json::Value::Null
    };
    if profile == CaptureProfile::Legacy {
        result["audit_uploads"] = 0.into();
    }
    hour_publish(&root, "idle-ready.json", result);
    hour_control(&root, "finish", nonce).await;
    harness.close().await;
    outbox.close().await;
    drop(harness);
    hour_publish(
        &root,
        "normal-exit.json",
        serde_json::json!({
            "phase":"normal-exit","physical_ports":false,"current_controller":true,
            "engine":profile.engine(),"pattern":profile.pattern()
        }),
    );
}
#[tokio::test]
async fn both_modes_and_engines_drain_stop_tail_and_persist_history_before_synthetic_ui_handoff() {
    for streaming in [false, true] {
        for mode in [DictationMode::Hold, DictationMode::Toggle] {
            let mut harness = Harness::new(streaming, false);
            harness.begin(mode).await;
            let mut bytes = pcm(16000);
            harness.ports.feed(&bytes[..16000]).await;
            harness.ports.feed(&bytes[16000..]).await;
            let tail = pcm(640);
            harness.ports.0.capture.lock().unwrap().tail = tail.clone();
            bytes.extend_from_slice(&tail);
            harness.stop().await;
            let (text, cancelled, reply) = harness.delivery().await;
            assert_eq!(text, RAW);
            assert!(!cancelled.load(Ordering::Acquire));
            assert_eq!(harness.entries()[0].body, text);
            let persisted = voiceprompt_client_protocol::history::parse_history(
                &std::fs::read(harness.root.join("history.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(persisted[0].body, text);
            assert_eq!(persisted[0].raw_body, None);
            if streaming {
                assert_eq!(*harness.ports.0.backend.0.stream.lock().unwrap(), bytes);
                assert!(harness.ports.0.backend.0.batches.lock().unwrap().is_empty());
            } else {
                assert_eq!(
                    &harness.ports.0.backend.0.batches.lock().unwrap()[0][44..],
                    bytes
                );
            }
            // This acknowledges the synthetic handoff only; no desktop/clipboard port exists here.
            reply.send(DeliveryResult::Pasted).unwrap();
            harness.stage(DictationStage::Pasted).await;
            harness.idle().await;
            assert!(cancelled.load(Ordering::Acquire));
            assert_eq!(harness.ports.0.opened.load(Ordering::Acquire), 1);
            assert_eq!(harness.ports.0.stopped.load(Ordering::Acquire), 1);
            assert!(harness.ui.try_recv().is_err());
            if streaming {
                let rows = harness.recovery.list().unwrap();
                assert_eq!(rows.len(), 1);
                let journal = harness.recovery.open(&rows[0].id).unwrap();
                let saved = journal.snapshot().unwrap();
                assert_eq!(saved.captured_bytes, bytes.len() as u64);
                assert_eq!(saved.confirmed_bytes, bytes.len() as u64);
                assert!(saved.audit.is_some());
            } else {
                assert!(harness.recovery.list().unwrap().is_empty());
            }
            harness.close().await;
        }
    }
}

#[tokio::test]
async fn polishing_success_and_failure_preserve_original_history_and_never_deliver_before_save() {
    for streaming in [false, true] {
        for failed in [false, true] {
            let mut harness = Harness::new(streaming, true);
            harness
                .ports
                .0
                .refiner
                .failed
                .store(failed, Ordering::Release);
            harness.begin(DictationMode::Hold).await;
            harness.ports.feed(&pcm(16000)).await;
            harness.stop().await;
            let (text, _, reply) = harness.delivery().await;
            let history = harness.entries();
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].raw_body.as_deref(), Some(RAW));
            assert_eq!(history[0].body, text);
            assert!(harness.ports.0.refiner.calls.load(Ordering::Acquire) > 0);
            if failed {
                assert_eq!(text, RAW);
                assert!(history[0].refinement_fallback_blocks > 0);
            } else {
                assert_eq!(text, RAW.replacen("synthetic", "polished", 1));
            }
            reply.send(DeliveryResult::CopiedOnly).unwrap();
            harness.stage(DictationStage::PasteSkipped).await;
            harness.idle().await;
            harness.close().await;
        }
    }
}

#[tokio::test]
async fn discard_deletes_only_its_session_but_interrupt_and_auth_change_retain_all_queued_audio() {
    for streaming in [false, true] {
        for reason in [
            StartupInterruption::Discarded,
            StartupInterruption::Interrupted,
            StartupInterruption::AuthChanged,
        ] {
            let mut harness = Harness::new(streaming, false);
            harness.begin(DictationMode::Toggle).await;
            let bytes = pcm(16000);
            harness.ports.feed(&bytes).await;
            match reason {
                StartupInterruption::Discarded => harness
                    .commands
                    .send(DictationCommand::Discard)
                    .await
                    .unwrap(),
                StartupInterruption::Interrupted => harness
                    .commands
                    .send(DictationCommand::Interrupt)
                    .await
                    .unwrap(),
                StartupInterruption::AuthChanged => {
                    harness.ports.0.auth.send_replace(AuthState::SignedOut);
                }
                _ => unreachable!(),
            }
            harness.idle().await;
            assert!(harness.entries().is_empty());
            assert!(harness.ui.try_recv().is_err());
            let rows = harness.recovery.list().unwrap();
            if reason == StartupInterruption::Discarded {
                assert!(rows.is_empty());
            } else {
                assert_eq!(rows.len(), 1);
                let journal = harness.recovery.open(&rows[0].id).unwrap();
                let manifest = journal.snapshot().unwrap();
                assert_eq!(manifest.captured_bytes, bytes.len() as u64);
                assert!(manifest.capture_complete);
                if manifest.chunks[0].text.is_none() {
                    assert_eq!(&journal.read_audio(0).unwrap()[44..], bytes);
                } else {
                    if streaming {
                        assert_eq!(manifest.confirmed_text, RAW);
                        assert_eq!(manifest.confirmed_bytes, bytes.len() as u64);
                        assert_eq!(*harness.ports.0.backend.0.stream.lock().unwrap(), bytes);
                    } else {
                        assert_eq!(manifest.chunks[0].text.as_deref(), Some(RAW));
                        assert_eq!(
                            &harness.ports.0.backend.0.batches.lock().unwrap()[0][44..],
                            bytes
                        );
                    }
                }
            }
            harness.close().await;
        }
    }
}

#[tokio::test]
async fn capture_start_and_stop_failures_are_visible_and_never_handoff_text() {
    for streaming in [false, true] {
        let mut harness = Harness::new(streaming, false);
        harness.ports.0.fail_open.store(true, Ordering::Release);
        harness
            .commands
            .send(DictationCommand::Begin {
                generation: 1,
                mode: DictationMode::Hold,
            })
            .await
            .unwrap();
        harness.stage(DictationStage::Failed).await;
        harness.idle().await;
        assert!(harness.status.borrow().issue.is_some());
        assert_eq!(harness.ports.0.stopped.load(Ordering::Acquire), 0);
        assert!(harness.ui.try_recv().is_err());
        harness.close().await;

        let mut harness = Harness::new(streaming, false);
        harness.ports.0.fail_stop.store(true, Ordering::Release);
        harness.begin(DictationMode::Hold).await;
        harness.ports.feed(&pcm(16000)).await;
        harness.stop().await;
        harness.idle().await;
        assert!(harness.status.borrow().issue.is_some());
        assert_eq!(harness.ports.0.stopped.load(Ordering::Acquire), 1);
        assert!(harness.ui.try_recv().is_err());
        assert!(harness.entries().is_empty());
        assert_eq!(harness.recovery.list().unwrap().len(), 1);
        harness.close().await;
    }
}

#[tokio::test]
async fn failed_history_write_keeps_recovery_and_never_requests_clipboard_or_paste() {
    for streaming in [false, true] {
        let mut harness = Harness::new(streaming, false);
        std::fs::create_dir(harness.root.join("history.json")).unwrap();
        harness.begin(DictationMode::Hold).await;
        harness.ports.feed(&pcm(16000)).await;
        harness.stop().await;
        harness.idle().await;
        assert!(harness.status.borrow().issue.is_some());
        assert!(harness.ui.try_recv().is_err());
        assert_eq!(harness.recovery.list().unwrap().len(), 1);
        harness.close().await;
    }
}

#[tokio::test]
async fn recovery_finishes_in_history_without_capture_or_any_automatic_delivery() {
    for streaming in [false, true] {
        let mut harness = Harness::new(streaming, true);
        let journal = harness
            .recovery
            .create(
                "cs".into(),
                format!("{URL}\n{EMAIL}"),
                streaming,
                time::OffsetDateTime::now_utc(),
            )
            .unwrap();
        let id = journal.id().to_owned();
        let bytes = pcm(16000);
        let wav = voiceprompt_core::wav::to_wav(&bytes).unwrap();
        journal
            .checkpoint(
                &[],
                bytes.len() as u64,
                &[AudioCheckpoint {
                    wav: &wav,
                    overlaps_previous: false,
                    end_byte: bytes.len() as u64,
                }],
                true,
                time::OffsetDateTime::now_utc(),
            )
            .unwrap();
        drop(journal);
        harness
            .commands
            .send(DictationCommand::Recover { id, generation: 1 })
            .await
            .unwrap();
        harness.stage(DictationStage::Saved).await;
        harness.idle().await;
        assert!(harness.status.borrow().dictation.recovering);
        assert_eq!(harness.entries()[0].body, RAW);
        assert_eq!(harness.ports.0.opened.load(Ordering::Acquire), 0);
        assert_eq!(harness.ports.0.stopped.load(Ordering::Acquire), 0);
        assert_eq!(harness.ports.0.refiner.calls.load(Ordering::Acquire), 0);
        assert!(harness.ui.try_recv().is_err());
        harness.close().await;
    }
}

#[tokio::test]
async fn shutdown_during_recording_joins_capture_and_preserves_audio_without_delivery() {
    for streaming in [false, true] {
        let mut harness = Harness::new(streaming, false);
        harness.begin(DictationMode::Toggle).await;
        harness.ports.feed(&pcm(16000)).await;
        harness.close().await;
        assert!(harness.entries().is_empty());
        assert!(harness.ui.try_recv().is_err());
        assert_eq!(harness.ports.0.stopped.load(Ordering::Acquire), 1);
        assert_eq!(harness.recovery.list().unwrap().len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_fresh_manifest_inspection_never_serializes_controller_capture_or_cancellation() {
    use crate::recovery_store::ManifestReadPhase;
    for streaming in [false, true] {
        for phase in [
            ManifestReadPhase::Open,
            ManifestReadPhase::Read,
            ManifestReadPhase::Unprotect,
        ] {
            for reason in [
                StartupInterruption::Stopped,
                StartupInterruption::Discarded,
                StartupInterruption::Interrupted,
                StartupInterruption::AuthChanged,
                StartupInterruption::Shutdown,
            ] {
                let mut harness = Harness::new(streaming, false);
                if streaming {
                    harness
                        .ports
                        .0
                        .backend
                        .0
                        .count_only
                        .store(true, Ordering::Release);
                }
                harness.begin(DictationMode::Toggle).await;
                let bytes = if streaming {
                    volume_pcm(32000)
                } else {
                    pcm(32000)
                };
                harness.ports.feed(&bytes).await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while harness
                        .ports
                        .0
                        .writer_completed_bytes
                        .load(Ordering::Acquire)
                        != 64000
                    {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
                let rows = harness.recovery.list().unwrap();
                let id = rows[0].id.clone();
                let pin = harness.recovery.fixture_pin_manifest(&id).unwrap();
                let (release, entered) = harness.recovery.pause_next_manifest_read(phase);
                let reader = tokio::task::spawn_blocking(move || pin.read());
                tokio::time::timeout(Duration::from_secs(5), entered)
                    .await
                    .unwrap()
                    .unwrap();
                let manifest_reads = harness.recovery.fixture_manifest_read_count();
                harness.ports.feed(&bytes).await;
                let progress = tokio::time::timeout(Duration::from_secs(5), async {
                    while harness
                        .ports
                        .0
                        .writer_completed_bytes
                        .load(Ordering::Acquire)
                        != 128000
                        || harness.ports.0.capture_trace.get().unwrap().snapshot()["saved_bytes"].as_u64() != Some(128000)
                        || streaming && harness.ports.0.capture_trace.get().unwrap().snapshot()["confirmed_bytes"].as_u64() != Some(128000)
                    {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await;
                if let Err(error) = progress {
                    drop(release);
                    let _ = reader.await;
                    harness.close().await;
                    panic!("Fresh manifest inspection blocked actual capture append: {error}");
                }
                let reader_still_blocked = !reader.is_finished();
                match reason {
                    StartupInterruption::Stopped => {
                        harness.stop().await;
                        let (_, _, reply) = harness.delivery().await;
                        reply.send(DeliveryResult::CopiedOnly).unwrap();
                        harness.stage(DictationStage::PasteSkipped).await;
                        harness.idle().await;
                    }
                    StartupInterruption::Discarded => {
                        harness
                            .commands
                            .send(DictationCommand::Discard)
                            .await
                            .unwrap();
                        harness.idle().await;
                    }
                    StartupInterruption::Interrupted => {
                        harness
                            .commands
                            .send(DictationCommand::Interrupt)
                            .await
                            .unwrap();
                        harness.idle().await;
                    }
                    StartupInterruption::AuthChanged => {
                        harness.ports.0.auth.send_replace(AuthState::SignedOut);
                        harness.idle().await;
                    }
                    StartupInterruption::Shutdown => {}
                }
                harness.close().await;
                let no_hot_disk_reads =
                    harness.recovery.fixture_manifest_read_count() == manifest_reads;
                let still_blocked_after_control = !reader.is_finished();
                drop(release);
                assert!(
                    reader.await.unwrap().is_err(),
                    "A released owner cannot publish a late inspection result."
                );
                assert!(reader_still_blocked && still_blocked_after_control);
                assert!(
                    no_hot_disk_reads,
                    "Hot checkpoint/result/history/audit getters reread the encrypted manifest."
                );
                assert_eq!(
                    harness.ports.0.accepted_bytes.load(Ordering::Acquire),
                    128000
                );
                assert_eq!(
                    harness
                        .ports
                        .0
                        .writer_completed_bytes
                        .load(Ordering::Acquire),
                    128000
                );
                if reason != StartupInterruption::Stopped {
                    assert!(harness.entries().is_empty());
                    assert!(harness.ui.try_recv().is_err());
                }
                let journal = harness.recovery.open(&id).unwrap();
                let state = journal.published_snapshot().unwrap();
                assert_eq!(state.context, format!("{URL}\n{EMAIL}"));
                assert_eq!(state.captured_bytes, 128000);
                assert!(state.confirmed_bytes <= state.captured_bytes);
                for chunk in state.chunks.iter().filter(|chunk| chunk.text.is_none()) {
                    let audio = journal.read_audio(chunk.index).unwrap();
                    let end = chunk.end_byte as usize;
                    let all = [bytes.as_slice(), bytes.as_slice()].concat();
                    assert_eq!(&audio[44..], &all[end - (audio.len() - 44)..end]);
                }
                drop(journal);
                harness.recovery.delete_idle(&id).unwrap();
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_native_checkpoint_keeps_stop_discard_auth_and_shutdown_responsive_and_owned() {
    for streaming in [false, true] {
        for reason in [
            StartupInterruption::Stopped,
            StartupInterruption::Discarded,
            StartupInterruption::Interrupted,
            StartupInterruption::AuthChanged,
            StartupInterruption::Shutdown,
        ] {
            let mut harness = Harness::new(streaming, false);
            harness.begin(DictationMode::Toggle).await;
            let (release, blocked) = harness.recovery.pause_next_fixture_write();
            let mut bytes = pcm(32000);
            harness.ports.feed(&bytes).await;
            tokio::time::timeout(Duration::from_secs(5), blocked)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                harness.recovery.fixture_io_progress()["phase"],
                "fixture-pause"
            );
            let packet = pcm(crate::microphone::BUFFER_BYTES / 2);
            {
                for _ in 0..crate::microphone::OUTPUT_BUFFER_COUNT {
                    harness.ports.feed(&packet).await;
                    bytes.extend_from_slice(&packet);
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    while harness
                        .ports
                        .0
                        .writer_completed_bytes
                        .load(Ordering::Acquire)
                        < bytes.len() as u64
                    {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
            }
            for _ in 0..crate::microphone::OUTPUT_BUFFER_COUNT {
                harness.ports.feed(&packet).await;
                bytes.extend_from_slice(&packet);
            }
            assert_eq!(
                harness.ports.try_feed(&packet),
                Err(AdmissionError::Overflow)
            );
            match reason {
                StartupInterruption::Stopped => harness.stop().await,
                StartupInterruption::Discarded => harness
                    .commands
                    .send(DictationCommand::Discard)
                    .await
                    .unwrap(),
                StartupInterruption::Interrupted => harness
                    .commands
                    .send(DictationCommand::Interrupt)
                    .await
                    .unwrap(),
                StartupInterruption::AuthChanged => {
                    harness.ports.0.auth.send_replace(AuthState::SignedOut);
                }
                StartupInterruption::Shutdown => {
                    harness.shutdown.send_replace(true);
                }
            }
            let responsive = tokio::time::timeout(Duration::from_millis(500), async {
                while harness.ports.0.stopped.load(Ordering::Acquire) != 1 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await;
            // Release the blocked write even if a responsiveness assertion fails.
            drop(release);
            assert!(
                responsive.is_ok(),
                "Stop/cancel was starved by an owned blocking checkpoint."
            );
            if reason == StartupInterruption::Stopped {
                let (_, _, reply) = harness.delivery().await;
                reply.send(DeliveryResult::CopiedOnly).unwrap();
                harness.stage(DictationStage::PasteSkipped).await;
                harness.idle().await;
                assert_eq!(harness.entries()[0].body, RAW);
                if streaming {
                    assert_eq!(*harness.ports.0.backend.0.stream.lock().unwrap(), bytes);
                } else {
                    assert_eq!(
                        &harness.ports.0.backend.0.batches.lock().unwrap()[0][44..],
                        bytes
                    );
                }
            } else {
                harness.idle().await;
                assert!(harness.entries().is_empty());
                assert!(harness.ui.try_recv().is_err());
            }
            let rows = harness.recovery.list().unwrap();
            if reason == StartupInterruption::Discarded
                || reason == StartupInterruption::Stopped && !streaming
            {
                assert!(rows.is_empty());
            } else {
                assert_eq!(rows.len(), 1);
                let journal = harness.recovery.open(&rows[0].id).unwrap();
                let manifest = journal.snapshot().unwrap();
                assert_eq!(manifest.context, format!("{URL}\n{EMAIL}"));
                assert_eq!(manifest.captured_bytes, bytes.len() as u64);
                assert!(manifest.capture_complete);
                assert!(manifest.confirmed_bytes <= manifest.captured_bytes);
                for chunk in manifest.chunks.iter().filter(|chunk| chunk.text.is_none()) {
                    let audio = journal.read_audio(chunk.index).unwrap();
                    let length = audio.len() - 44;
                    let end = usize::try_from(chunk.end_byte).unwrap();
                    assert_eq!(&audio[44..], &bytes[end - length..end]);
                }
                drop(journal);
            }
            harness.close().await;
            assert_eq!(harness.ports.0.stopped.load(Ordering::Acquire), 1);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sampled_monitor_does_not_wait_for_blocked_engine_or_journal_mutexes() {
    let mut harness = Harness::new(true, false);
    harness.begin(DictationMode::Hold).await;
    let (release, blocked) = harness.recovery.pause_next_fixture_write();
    harness.ports.feed(&pcm(32000)).await;
    tokio::time::timeout(Duration::from_secs(5), blocked)
        .await
        .unwrap()
        .unwrap();
    let probe = harness.ports.0.clone();
    let recovery = harness.recovery.clone();
    let sampled = tokio::time::timeout(
        Duration::from_millis(200),
        tokio::task::spawn_blocking(move || {
            CaptureMonitor::sample(&probe, &recovery, Instant::now(), CaptureProfile::Streaming)
        }),
    )
    .await;
    drop(release);
    let sample = sampled
        .expect("Monitor blocked on a target mutex.")
        .unwrap();
    assert_eq!(sample["observer_uses_no_target_mutex"], true);
    assert_eq!(sample["recovery_io"]["phase"], "fixture-pause");
    harness
        .commands
        .send(DictationCommand::Interrupt)
        .await
        .unwrap();
    harness.idle().await;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_checkpoint_exposes_stop_deadline_without_false_join_or_lost_accepted_audio() {
    for reason in [
        StartupInterruption::Stopped,
        StartupInterruption::AuthChanged,
        StartupInterruption::Shutdown,
    ] {
        let mut harness = Harness::new(true, false);
        harness.begin(DictationMode::Toggle).await;
        let (release, blocked) = harness.recovery.pause_next_fixture_write();
        let mut bytes = pcm(32000);
        harness.ports.feed(&bytes).await;
        tokio::time::timeout(Duration::from_secs(5), blocked)
            .await
            .unwrap()
            .unwrap();
        let packet = pcm(crate::microphone::BUFFER_BYTES / 2);
        for _ in 0..crate::microphone::OUTPUT_BUFFER_COUNT {
            harness.ports.feed(&packet).await;
            bytes.extend_from_slice(&packet);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness
                .ports
                .0
                .writer_completed_bytes
                .load(Ordering::Acquire)
                < bytes.len() as u64
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        for _ in 0..crate::microphone::OUTPUT_BUFFER_COUNT {
            harness.ports.feed(&packet).await;
            bytes.extend_from_slice(&packet);
        }
        assert_eq!(
            harness.ports.try_feed(&packet),
            Err(AdmissionError::Overflow)
        );
        harness.stop().await;
        harness.stage(DictationStage::Finishing).await;
        match reason {
            StartupInterruption::Stopped => {}
            StartupInterruption::AuthChanged => {
                harness.ports.0.auth.send_replace(AuthState::SignedOut);
            }
            StartupInterruption::Shutdown => {
                harness.shutdown.send_replace(true);
            }
            _ => unreachable!(),
        }
        let deadline = tokio::time::timeout(Duration::from_secs(3), async {
            while !harness
                .status
                .borrow()
                .dictation
                .issue
                .is_some_and(|issue| issue.contains("stop deadline"))
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        let still_owned = !harness.worker.as_ref().unwrap().is_finished();
        let stage = harness.status.borrow().dictation.stage;
        let busy = harness.policy.dictation_busy();
        let joined = if deadline.is_ok() {
            tokio::time::timeout(Duration::from_secs(5), harness.worker.as_mut().unwrap())
                .await
                .is_ok()
        } else {
            true
        };
        // A failed assertion must not strand the real blocking writer.
        drop(release);
        assert!(deadline.is_ok());
        assert!(still_owned && busy);
        assert_eq!(stage, DictationStage::Finishing);
        assert!(
            !joined,
            "A blocked writer must not produce a joined-normal PASS."
        );
        if reason != StartupInterruption::Shutdown {
            harness.idle().await;
        }
        harness.close().await;
        assert!(harness.entries().is_empty());
        assert!(harness.ui.try_recv().is_err());
        let rows = harness.recovery.list().unwrap();
        assert_eq!(rows.len(), 1);
        let journal = harness.recovery.open(&rows[0].id).unwrap();
        let state = journal.snapshot().unwrap();
        assert!(state.capture_complete);
        assert_eq!(state.captured_bytes, bytes.len() as u64);
        assert_eq!(state.context, format!("{URL}\n{EMAIL}"));
        for chunk in state.chunks.iter().filter(|chunk| chunk.text.is_none()) {
            let audio = journal.read_audio(chunk.index).unwrap();
            let end = usize::try_from(chunk.end_byte).unwrap();
            assert_eq!(&audio[44..], &bytes[end - (audio.len() - 44)..end]);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_seal_does_not_starve_timers_and_pending_cancellation_cannot_deliver_late_text() {
    for streaming in [false, true] {
        for reason in [
            StartupInterruption::Discarded,
            StartupInterruption::AuthChanged,
            StartupInterruption::Shutdown,
        ] {
            let mut harness = Harness::new(streaming, false);
            harness.begin(DictationMode::Hold).await;
            let (release, blocked) = harness.recovery.pause_next_fixture_write();
            let bytes = pcm(640);
            harness.ports.feed(&bytes).await;
            harness.stop().await;
            tokio::time::timeout(Duration::from_secs(5), blocked)
                .await
                .unwrap()
                .unwrap();
            match reason {
                StartupInterruption::Discarded => harness
                    .commands
                    .send(DictationCommand::Discard)
                    .await
                    .unwrap(),
                StartupInterruption::AuthChanged => {
                    harness.ports.0.auth.send_replace(AuthState::SignedOut);
                }
                StartupInterruption::Shutdown => {
                    harness.shutdown.send_replace(true);
                }
                _ => unreachable!(),
            }
            let timer = tokio::time::timeout(Duration::from_millis(200), async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                assert!(
                    harness.policy.dictation_busy(),
                    "A blocked seal cannot report an idle success."
                );
            })
            .await;
            drop(release);
            assert!(timer.is_ok());
            harness.idle().await;
            assert!(harness.entries().is_empty());
            assert!(harness.ui.try_recv().is_err());
            let rows = harness.recovery.list().unwrap();
            if reason == StartupInterruption::Discarded {
                assert!(rows.is_empty());
            } else {
                assert_eq!(rows.len(), 1);
                let manifest = harness.recovery.fixture_snapshot(&rows[0].id).unwrap();
                assert_eq!(manifest.context, format!("{URL}\n{EMAIL}"));
                assert_eq!(manifest.captured_bytes, bytes.len() as u64);
                assert!(manifest.capture_complete);
            }
            harness.close().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_pinned_audio_read_allows_capture_progress_and_joins_before_cancelled_cleanup() {
    for reason in [
        StartupInterruption::Stopped,
        StartupInterruption::Discarded,
        StartupInterruption::Interrupted,
        StartupInterruption::AuthChanged,
        StartupInterruption::Shutdown,
    ] {
        let mut harness = Harness::new(true, false);
        let (unblock_audio, audio_gate) = oneshot::channel();
        *harness.ports.0.backend.0.audio_gate.lock().unwrap() = Some(audio_gate);
        let (release, blocked) = harness.recovery.pause_next_pinned_read();
        harness.begin(DictationMode::Hold).await;
        let mut bytes = pcm(32000);
        harness.ports.feed(&bytes).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let rows = harness.recovery.list().unwrap();
                if harness
                    .recovery
                    .fixture_snapshot(&rows[0].id)
                    .unwrap()
                    .captured_bytes
                    == 64_000
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        unblock_audio.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), blocked)
            .await
            .unwrap()
            .unwrap();
        let frame = pcm(crate::microphone::BUFFER_BYTES / 2);
        for _ in 0..20 {
            harness.ports.feed(&frame).await;
            bytes.extend_from_slice(&frame);
        }
        let progressed = tokio::time::timeout(Duration::from_secs(1), async {
            while harness
                .ports
                .0
                .writer_completed_bytes
                .load(Ordering::Acquire)
                < bytes.len() as u64
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        if progressed.is_err() {
            drop(release);
            harness.close().await;
            panic!("Slow encrypted read blocked capture append.");
        }
        match reason {
            StartupInterruption::Stopped => harness.stop().await,
            StartupInterruption::Discarded => harness
                .commands
                .send(DictationCommand::Discard)
                .await
                .unwrap(),
            StartupInterruption::Interrupted => harness
                .commands
                .send(DictationCommand::Interrupt)
                .await
                .unwrap(),
            StartupInterruption::AuthChanged => {
                harness.ports.0.auth.send_replace(AuthState::SignedOut);
            }
            StartupInterruption::Shutdown => {
                harness.shutdown.send_replace(true);
            }
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while harness.ports.0.stopped.load(Ordering::Acquire) != 1 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            harness.policy.dictation_busy(),
            "Reader I/O cannot be detached as completed cleanup."
        );
        assert!(!harness.worker.as_ref().unwrap().is_finished());
        drop(release);
        if reason == StartupInterruption::Stopped {
            let (_, _, reply) = harness.delivery().await;
            reply.send(DeliveryResult::CopiedOnly).unwrap();
            harness.stage(DictationStage::PasteSkipped).await;
            harness.idle().await;
            assert_eq!(*harness.ports.0.backend.0.stream.lock().unwrap(), bytes);
        } else {
            harness.idle().await;
            assert!(harness.entries().is_empty());
            assert!(harness.ui.try_recv().is_err());
        }
        let rows = harness.recovery.list().unwrap();
        if reason == StartupInterruption::Discarded {
            assert!(rows.is_empty());
        } else {
            assert_eq!(rows.len(), 1);
            let state = harness.recovery.fixture_snapshot(&rows[0].id).unwrap();
            assert_eq!(state.context, format!("{URL}\n{EMAIL}"));
            assert_eq!(state.captured_bytes, bytes.len() as u64);
            assert!(state.capture_complete);
        }
        harness.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_write_failure_preserves_the_durable_prefix_binding_and_is_not_false_full_capture() {
    for streaming in [false, true] {
        let mut harness = Harness::new(streaming, false);
        harness.begin(DictationMode::Hold).await;
        let prefix = pcm(32000);
        harness.ports.feed(&prefix).await;
        let rows = harness.recovery.list().unwrap();
        assert_eq!(rows.len(), 1);
        let id = rows[0].id.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let saved = harness.recovery.fixture_snapshot(&id).unwrap();
                if saved.captured_bytes == prefix.len() as u64 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let path = harness.root.join("recovery").join(&id).join("manifest.enc");
        let original_cipher = std::fs::read(&path).unwrap();
        let (release, blocked) = harness.recovery.pause_next_fixture_write();
        harness.ports.feed(&prefix).await;
        tokio::time::timeout(Duration::from_secs(5), blocked)
            .await
            .unwrap()
            .unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let frame = pcm(crate::microphone::BUFFER_BYTES / 2);
        for _ in 0..5 {
            harness.ports.try_feed(&frame).unwrap();
        }
        let admitted = 2 * prefix.len() as u64 + 5 * frame.len() as u64;
        drop(release);
        harness.idle().await;
        assert_eq!(
            harness.ports.try_feed(&frame),
            Err(AdmissionError::NotRunning)
        );
        let context = harness.ports.0.admission_failure.get().unwrap().clone();
        assert_eq!(context["accepted_bytes"], admitted);
        assert_eq!(context["last_accepted_sequence"], admitted / 1280);
        assert_eq!(context["attempted_sequence"], admitted / 1280 + 1);
        let received = context["writer_received_bytes"].as_u64().unwrap();
        let completed = context["writer_completed_bytes"].as_u64().unwrap();
        assert!((128_000..=admitted).contains(&received));
        assert!((128_000..=received).contains(&completed));
        assert_eq!(context["last_published_saved_bytes"], 64000);
        assert_eq!(context["last_published_confirmed_bytes"], 0);
        assert_eq!(context["cursors_are_not_a_fresh_disk_snapshot"], true);
        assert_eq!(std::fs::read(&path).unwrap(), original_cipher);
        hour_publish(
            &harness.root,
            "immutable-admission-failure.json",
            context.clone(),
        );
        harness.close().await;
        assert_eq!(harness.ports.0.admission_failure.get().unwrap(), &context);
        assert_eq!(
            harness.ports.0.accepted_bytes.load(Ordering::Acquire),
            admitted
        );
        drop(held);
        assert!(harness.status.borrow().issue.is_some());
        assert!(harness.entries().is_empty());
        assert!(harness.ui.try_recv().is_err());
        let journal = harness.recovery.open(&id).unwrap();
        let saved = journal.snapshot().unwrap();
        assert_eq!(saved.context, format!("{URL}\n{EMAIL}"));
        assert_eq!(saved.captured_bytes, prefix.len() as u64);
        assert!(
            !saved.capture_complete,
            "Unwritten samples must not appear as durable completion."
        );
        if streaming {
            assert_eq!(&journal.read_audio(0).unwrap()[44..], prefix);
        } else {
            let tail = voiceprompt_client_protocol::legacy_checkpoint::parse(&saved.tail).unwrap();
            assert_eq!(tail.audio, prefix);
            assert!(tail.frame.is_empty());
        }
        drop(journal);
        println!(
            "actual-storage-error-admission: streaming={streaming}; {context}; durable=64000; all-admitted-saved=false; delivery=false"
        );
    }
}

#[tokio::test]
async fn queued_ui_delivery_is_cancelled_on_auth_change_and_explicit_discard() {
    for discard in [false, true] {
        let mut harness = Harness::new(false, false);
        harness.begin(DictationMode::Hold).await;
        harness.ports.feed(&pcm(16000)).await;
        harness.stop().await;
        let (_, cancelled, reply) = harness.delivery().await;
        if discard {
            harness
                .commands
                .send(DictationCommand::Discard)
                .await
                .unwrap();
        } else {
            harness.ports.0.auth.send_replace(AuthState::SignedOut);
        }
        harness.idle().await;
        assert!(cancelled.load(Ordering::Acquire));
        assert!(reply.send(DeliveryResult::Pasted).is_err());
        if discard {
            assert!(harness.entries().is_empty());
        } else {
            assert_eq!(harness.entries()[0].body, RAW);
        }
        harness.close().await;
    }
}

#[tokio::test]
async fn provisional_stream_and_failed_batch_fallback_never_become_delivered_history() {
    let mut harness = Harness::new(true, false);
    harness
        .ports
        .0
        .backend
        .0
        .failed
        .store(true, Ordering::Release);
    harness.begin(DictationMode::Toggle).await;
    let bytes = pcm(16000);
    harness.ports.feed(&bytes).await;
    harness
        .view(|view| view.preview.contains("provisional"))
        .await;
    assert!(harness.entries().is_empty());
    assert!(harness.ui.try_recv().is_err());
    harness.stop().await;
    harness.idle().await;
    assert!(harness.status.borrow().issue.is_some());
    assert!(harness.entries().is_empty());
    assert!(harness.ui.try_recv().is_err());
    let rows = harness.recovery.list().unwrap();
    assert_eq!(rows.len(), 1);
    let journal = harness.recovery.open(&rows[0].id).unwrap();
    let manifest = journal.snapshot().unwrap();
    assert_eq!(manifest.captured_bytes, bytes.len() as u64);
    assert_eq!(manifest.confirmed_bytes, 0);
    assert_eq!(&journal.read_audio(0).unwrap()[44..], bytes);
    drop(journal);
    harness.close().await;
}

#[tokio::test]
async fn auth_discard_and_shutdown_cancel_and_join_a_blocked_completion() {
    for reason in [
        StartupInterruption::AuthChanged,
        StartupInterruption::Discarded,
        StartupInterruption::Shutdown,
    ] {
        let mut harness = Harness::new(false, false);
        harness
            .ports
            .0
            .backend
            .0
            .batch_blocked
            .store(true, Ordering::Release);
        harness.begin(DictationMode::Hold).await;
        harness.ports.feed(&pcm(16000)).await;
        harness.stop().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.ports.0.backend.0.batches.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        match reason {
            StartupInterruption::AuthChanged => {
                harness.ports.0.auth.send_replace(AuthState::SignedOut);
            }
            StartupInterruption::Discarded => harness
                .commands
                .send(DictationCommand::Discard)
                .await
                .unwrap(),
            StartupInterruption::Shutdown => {
                harness.shutdown.send_replace(true);
            }
            _ => unreachable!(),
        }
        harness.idle().await;
        assert_eq!(harness.ports.0.backend.0.active.load(Ordering::Acquire), 0);
        assert!(harness.entries().is_empty());
        assert!(harness.ui.try_recv().is_err());
        let rows = harness.recovery.list().unwrap();
        assert_eq!(
            rows.len(),
            usize::from(reason != StartupInterruption::Discarded)
        );
        harness.close().await;
    }
}

#[tokio::test]
async fn unavailable_ui_is_reported_after_durable_history_without_retry_or_delivery() {
    for streaming in [false, true] {
        let mut harness = Harness::new(streaming, false);
        harness.begin(DictationMode::Hold).await;
        harness.ports.feed(&pcm(16000)).await;
        harness.ui.close();
        harness.stop().await;
        harness.stage(DictationStage::PasteSkipped).await;
        harness.idle().await;
        assert_eq!(harness.entries()[0].body, RAW);
        assert!(harness.status.borrow().issue.is_some());
        assert!(harness.ui.try_recv().is_err());
        harness.close().await;
    }
}

#[tokio::test]
async fn disabled_settings_or_signed_out_start_never_acquire_capture_or_backend() {
    for disabled in [false, true] {
        let mut harness = Harness::new(false, false);
        if disabled {
            harness
                .settings
                .send_modify(|settings| settings.dictation_enabled = false);
        } else {
            harness.ports.0.auth.send_replace(AuthState::SignedOut);
        }
        harness
            .commands
            .send(DictationCommand::Begin {
                generation: 1,
                mode: DictationMode::Hold,
            })
            .await
            .unwrap();
        harness
            .commands
            .send(DictationCommand::Stop { generation: 999 })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.commands.capacity() != 8 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(harness.ports.0.opened.load(Ordering::Acquire), 0);
        assert!(harness.ports.0.backend.0.batches.lock().unwrap().is_empty());
        assert!(harness.ports.0.backend.0.starts.lock().unwrap().is_empty());
        assert!(harness.recovery.list().unwrap().is_empty());
        harness.close().await;
    }
}

#[tokio::test]
async fn count_only_volume_fixture_confirms_durable_prefixes_before_stop_and_joins_cleanly() {
    let mut harness = Harness::new(true, false);
    harness
        .ports
        .0
        .backend
        .0
        .count_only
        .store(true, Ordering::Release);
    harness.begin(DictationMode::Toggle).await;
    for prefix in 1..=20u64 {
        harness.feed_many(&volume_pcm(6400), 5).await;
        let confirmed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let observed = harness.ports.0.capture_trace.get().unwrap().snapshot();
                if observed["confirmed_bytes"].as_u64() == Some(prefix * 64000) {
                    assert!(observed["saved_bytes"].as_u64().unwrap() >= prefix * 64000);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        if let Err(error) = confirmed {
            harness.close().await;
            panic!("Accelerated functional prefix was not durably confirmed: {error}");
        }
    }
    harness
        .view(|view| view.preview.contains("synthetic controller"))
        .await;
    assert!(
        harness
            .ports
            .0
            .backend
            .0
            .confirmed_to
            .load(Ordering::Acquire)
            > 0
    );
    harness.stop().await;
    let (text, _, reply) = harness.delivery().await;
    assert_eq!(text, RAW);
    reply.send(DeliveryResult::CopiedOnly).unwrap();
    harness.stage(DictationStage::PasteSkipped).await;
    harness.idle().await;
    assert_eq!(
        harness
            .ports
            .0
            .backend
            .0
            .stream_bytes
            .load(Ordering::Acquire),
        1_280_000
    );
    assert_eq!(
        harness
            .ports
            .0
            .backend
            .0
            .confirmed_to
            .load(Ordering::Acquire),
        1_280_000
    );
    harness.close().await;
    let owned_path = harness.root.clone();
    drop(harness);
    assert!(!owned_path.exists());
}

#[tokio::test]
#[ignore = "Explicit115200000-byte full controller synthetic volume proof; no physical ports and not a wall-clock or GUI hour."]
async fn accelerated_controller_hour_sample_volume() {
    assert_eq!(
        std::env::var("VOICEPROMPT_SYNTHETIC_CONTROLLER_VOLUME").as_deref(),
        Ok("115200000-count-only-noports"),
        "Explicit synthetic volume invocation required; never infer physical permission."
    );
    let began = Instant::now();
    let mut harness = Harness::new(true, false);
    harness
        .ports
        .0
        .backend
        .0
        .count_only
        .store(true, Ordering::Release);
    harness.begin(DictationMode::Toggle).await;
    let buffer = volume_pcm(6400);
    let expected = 115_200_000;
    harness.feed_many(&buffer, 9000).await;
    tokio::time::timeout(Duration::from_secs(30), async {
        while harness
            .ports
            .0
            .backend
            .0
            .stream_bytes
            .load(Ordering::Acquire)
            < expected - 64_000
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("Controller volume stream did not drain.");
    harness.stop().await;
    let (text, _, reply) = harness.delivery().await;
    assert_eq!(text, RAW);
    assert_eq!(harness.entries()[0].body, text);
    reply.send(DeliveryResult::CopiedOnly).unwrap();
    harness.stage(DictationStage::PasteSkipped).await;
    harness.idle().await;
    let streamed = harness
        .ports
        .0
        .backend
        .0
        .stream_bytes
        .load(Ordering::Acquire);
    assert_eq!(streamed, expected);
    assert!(harness.ports.0.backend.0.stream.lock().unwrap().is_empty());
    assert!(harness.ports.0.backend.0.batches.lock().unwrap().is_empty());
    let rows = harness.recovery.list().unwrap();
    assert_eq!(rows.len(), 1);
    let journal = harness.recovery.open(&rows[0].id).unwrap();
    let manifest = journal.snapshot().unwrap();
    assert_eq!(manifest.captured_bytes, expected);
    assert_eq!(manifest.confirmed_bytes, expected);
    assert!(manifest.capture_complete);
    assert_eq!(manifest.audit.as_ref().unwrap().text, RAW);
    drop(journal);
    harness.close().await;
    assert_eq!(harness.ports.0.opened.load(Ordering::Acquire), 1);
    assert_eq!(harness.ports.0.stopped.load(Ordering::Acquire), 1);
    let owned_path = harness.root.clone();
    drop(harness);
    assert!(
        !owned_path.exists(),
        "Owned controller volume fixture was not cleaned."
    );
    println!(
        "{{\"scope\":\"full-controller-synthetic-hour-sample-volume\",\"pcm_bytes\":{expected},\"streamed_bytes\":{streamed},\"all_sample_bytes_checked\":true,\"durable_history_and_audit\":true,\"capture_joined\":true,\"cleanup_complete\":true,\"elapsed_seconds\":{},\"real_hour\":false,\"native_gui\":false,\"physical_ports\":false,\"full_migration_acceptance\":false}}",
        began.elapsed().as_secs_f64()
    );
}

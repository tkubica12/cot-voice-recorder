use super::*;
use crate::dictation_ports::DictationPorts;
use crate::microphone::CaptureError;
use crate::recovery_store::tests::TemporaryRoot;
use crate::{AudioSender, BatchBackend, EventReceiver};
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

fn failure() -> ApiError {
    ApiError {
        kind: ApiErrorKind::Forbidden,
        status: Some(403),
        problem: None,
        response_invalid: false,
    }
}

#[derive(Default)]
struct BackendState {
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
        self.0.active.fetch_add(1, Ordering::AcqRel);
        let _active = Active(&self.0.active);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(language, "cs");
        assert_eq!(context, format!("{URL}\n{EMAIL}"));
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
        self.end.fetch_add(pcm.len() as u64, Ordering::AcqRel);
        let start = self
            .backend
            .0
            .stream_bytes
            .fetch_add(pcm.len() as u64, Ordering::AcqRel);
        if self.backend.0.count_only.load(Ordering::Acquire) {
            let pattern = [12i16.to_le_bytes(), (-12i16).to_le_bytes()].concat();
            for (index, byte) in pcm.iter().enumerate() {
                assert_eq!(*byte, pattern[((start + index as u64) % 4) as usize]);
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
}

#[derive(Clone)]
struct Ports(Arc<Probe>);

struct Capture(Arc<Probe>);

impl Ports {
    fn new() -> Self {
        let (auth, _) = watch::channel(AuthState::SignedIn);
        let (sender, receiver) = mpsc::channel(50);
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
        }))
    }

    fn feed(&self, bytes: &[u8]) {
        self.0
            .capture
            .lock()
            .unwrap()
            .sender
            .as_ref()
            .unwrap()
            .try_send(CaptureBuffer(bytes.to_vec()))
            .unwrap();
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
    directory: TemporaryRoot,
}

impl Harness {
    fn new(streaming: bool, refinement: bool) -> Self {
        let directory = TemporaryRoot::new();
        let recovery = open_recovery(directory.0.join("recovery")).unwrap();
        let history = Arc::new(NativeHistoryStore::new(directory.0.join("history.json")).unwrap());
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
            directory,
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
            let sender = self
                .ports
                .0
                .capture
                .lock()
                .unwrap()
                .sender
                .as_ref()
                .unwrap()
                .clone();
            sender.send(CaptureBuffer(buffer.to_vec())).await.unwrap();
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

#[tokio::test]
async fn both_modes_and_engines_drain_stop_tail_and_persist_history_before_synthetic_ui_handoff() {
    for streaming in [false, true] {
        for mode in [DictationMode::Hold, DictationMode::Toggle] {
            let mut harness = Harness::new(streaming, false);
            harness.begin(mode).await;
            let mut bytes = pcm(16000);
            harness.ports.feed(&bytes[..16000]);
            harness.ports.feed(&bytes[16000..]);
            let tail = pcm(640);
            harness.ports.0.capture.lock().unwrap().tail = tail.clone();
            bytes.extend_from_slice(&tail);
            harness.stop().await;
            let (text, cancelled, reply) = harness.delivery().await;
            assert_eq!(text, RAW);
            assert!(!cancelled.load(Ordering::Acquire));
            assert_eq!(harness.entries()[0].body, text);
            let persisted = voiceprompt_client_protocol::history::parse_history(
                &std::fs::read(harness.directory.0.join("history.json")).unwrap(),
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
            harness.ports.feed(&pcm(16000));
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
            harness.ports.feed(&bytes);
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
                assert_eq!(&journal.read_audio(0).unwrap()[44..], bytes);
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
        harness.ports.feed(&pcm(16000));
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
        std::fs::create_dir(harness.directory.0.join("history.json")).unwrap();
        harness.begin(DictationMode::Hold).await;
        harness.ports.feed(&pcm(16000));
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
        harness.ports.feed(&pcm(16000));
        harness.close().await;
        assert!(harness.entries().is_empty());
        assert!(harness.ui.try_recv().is_err());
        assert_eq!(harness.ports.0.stopped.load(Ordering::Acquire), 1);
        assert_eq!(harness.recovery.list().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn queued_ui_delivery_is_cancelled_on_auth_change_and_explicit_discard() {
    for discard in [false, true] {
        let mut harness = Harness::new(false, false);
        harness.begin(DictationMode::Hold).await;
        harness.ports.feed(&pcm(16000));
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
    harness.ports.feed(&bytes);
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
        harness.ports.feed(&pcm(16000));
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
        harness.ports.feed(&pcm(16000));
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
    harness.feed_many(&volume_pcm(6400), 100).await;
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
    let owned_path = harness.directory.0.clone();
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
    let owned_path = harness.directory.0.clone();
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

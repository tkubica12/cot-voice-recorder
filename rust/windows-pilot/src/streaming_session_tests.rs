use super::*;
use crate::recovery_store::tests::TemporaryRoot;
use crate::{DEFAULT_RECOVERY_QUOTA, NativeRecoveryStore};
use std::sync::atomic::AtomicUsize;
use tokio::sync::mpsc;

#[derive(Clone, Copy)]
enum Mode {
    Healthy,
    ProvisionalFailure,
    DisconnectOnce,
    Recovery,
    PartialDisconnect,
    InvalidConfirmation,
}

struct Service {
    mode: Mode,
    connections: AtomicUsize,
    committed: AtomicU64,
    starts: Mutex<Vec<u64>>,
    sent: Mutex<Vec<Vec<u8>>>,
    batches: Mutex<Vec<Vec<u8>>>,
    changed: Notify,
    connection_events: Mutex<Vec<mpsc::Sender<Event>>>,
}

impl Service {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            connections: AtomicUsize::new(0),
            committed: AtomicU64::new(0),
            starts: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
            batches: Mutex::new(Vec::new()),
            changed: Notify::new(),
            connection_events: Mutex::new(Vec::new()),
        })
    }
}

enum Event {
    Message(DictationMessage),
    Failure,
}

#[derive(Clone)]
struct Sender {
    events: mpsc::Sender<Event>,
    connection: Arc<Mutex<Connection>>,
    service: Arc<Service>,
    index: usize,
}

struct Connection {
    position: u64,
    provisional_sent: bool,
}

struct Receiver(mpsc::Receiver<Event>);

fn unavailable() -> ApiError {
    ApiError {
        kind: ApiErrorKind::Retryable,
        status: Some(503),
        problem: None,
        response_invalid: false,
    }
}

impl AudioSender for Sender {
    async fn audio(&self, pcm: &[u8]) -> Result<(), ApiError> {
        let provisional = {
            let mut connection = self.connection.lock().unwrap();
            connection.position += pcm.len() as u64;
            let first = !connection.provisional_sent;
            connection.provisional_sent = true;
            first
        };
        self.service.sent.lock().unwrap()[self.index].extend_from_slice(pcm);
        self.service.changed.notify_one();
        if provisional {
            self.events
                .send(Event::Message(DictationMessage::Delta {
                    item_id: "i".into(),
                    text: "synthetic ".into(),
                }))
                .await
                .map_err(|_| unavailable())?;
            self.events
                .send(Event::Message(DictationMessage::Delta {
                    item_id: "i".into(),
                    text: "preview".into(),
                }))
                .await
                .map_err(|_| unavailable())?;
        }
        Ok(())
    }
    async fn commit(&self, finish: bool) -> Result<(), ApiError> {
        self.service
            .committed
            .store(self.connection.lock().unwrap().position, Ordering::Release);
        self.service.changed.notify_one();
        if matches!(self.service.mode, Mode::PartialDisconnect) && self.index == 0 {
            if !finish {
                let end = self.connection.lock().unwrap().position;
                self.events
                    .send(Event::Message(DictationMessage::Confirmed {
                        item_id: "i".into(),
                        text: "part1".into(),
                        byte_end: end,
                    }))
                    .await
                    .map_err(|_| unavailable())?;
            }
            self.events
                .send(Event::Failure)
                .await
                .map_err(|_| unavailable())?;
            return Ok(());
        }
        if !finish {
            return Ok(());
        }
        if matches!(self.service.mode, Mode::ProvisionalFailure)
            || matches!(self.service.mode, Mode::DisconnectOnce) && self.index == 0
        {
            self.events
                .send(Event::Failure)
                .await
                .map_err(|_| unavailable())?;
            return Ok(());
        }
        let mut end = self.connection.lock().unwrap().position;
        if matches!(self.service.mode, Mode::InvalidConfirmation) {
            end += 640;
        }
        let text = if matches!(self.service.mode, Mode::PartialDisconnect) {
            "part2"
        } else {
            "synthetic confirmed"
        };
        self.events
            .send(Event::Message(DictationMessage::Confirmed {
                item_id: "i".into(),
                text: text.into(),
                byte_end: end,
            }))
            .await
            .map_err(|_| unavailable())?;
        self.events
            .send(Event::Message(DictationMessage::Done { byte_end: end }))
            .await
            .map_err(|_| unavailable())?;
        Ok(())
    }
}

impl EventReceiver for Receiver {
    async fn receive(&mut self) -> Result<DictationMessage, ApiError> {
        match self.0.recv().await {
            Some(Event::Message(message)) => Ok(message),
            _ => Err(unavailable()),
        }
    }
}

impl BatchBackend for Arc<Service> {
    async fn transcribe(&self, wav: &[u8], _: &str, _: &str) -> Result<String, ApiError> {
        self.batches.lock().unwrap().push(wav[44..].to_vec());
        Ok("tail pending".into())
    }
}

impl StreamingBackend for Arc<Service> {
    type Sender = Sender;
    type Receiver = Receiver;
    async fn connect(&self, _: &str, start: u64, _: &str) -> Result<(Sender, Receiver), ApiError> {
        let index = self.connections.fetch_add(1, Ordering::AcqRel);
        self.starts.lock().unwrap().push(start);
        self.sent.lock().unwrap().push(Vec::new());
        self.changed.notify_one();
        let (events, receiver) = mpsc::channel(64);
        self.connection_events.lock().unwrap().push(events.clone());
        Ok((
            Sender {
                events,
                connection: Arc::new(Mutex::new(Connection {
                    position: start,
                    provisional_sent: false,
                })),
                service: self.clone(),
                index,
            },
            Receiver(receiver),
        ))
    }
}

fn journal(directory: &TemporaryRoot) -> (NativeRecoveryStore, NativeRecoveryJournal) {
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create(
            "auto".into(),
            "synthetic context".into(),
            true,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    (store, journal)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_progresses_while_send_checkpoint_or_confirmation_io_is_paused() {
    let mut outcomes = Vec::new();
    for confirming in [false, true] {
        let directory = TemporaryRoot::new();
        let (store, journal) = journal(&directory);
        let service = Service::new(Mode::Healthy);
        let session = Arc::new(StreamingSession::start(journal, Arc::new(service), true).unwrap());
        let bytes: Vec<_> = (0..35_200)
            .flat_map(|index| ((index % 97) as i16 - 48).to_le_bytes())
            .collect();
        session.append(&bytes[..32_000]).unwrap();
        let (_stop, stopped) = watch::channel(false);
        if confirming {
            checkpoint_for_send(
                session.inner.clone(),
                stopped.clone(),
                32_000,
                "regression-prepare",
            )
            .await
            .unwrap();
        }
        let (release, entered) = store.pause_next_fixture_write();
        let owner = session.inner.clone();
        let context = bytes[32_000usize.saturating_sub(OVERLAP_BYTES)..32_000].to_vec();
        let io = tokio::spawn(async move {
            if confirming {
                tokio::task::spawn_blocking(move || {
                    let mut state = owner.lock_named("regression-confirm")?;
                    owner.confirm(&mut state, 32_000, "first".into(), false, 1, context)
                })
                .await
                .unwrap()
            } else {
                checkpoint_for_send(owner, stopped, 32_000, "regression-send-checkpoint")
                    .await
                    .map(|_| ())
            }
        });
        tokio::time::timeout(Duration::from_secs(5), entered)
            .await
            .unwrap()
            .unwrap();
        let capture = session.clone();
        let packets = bytes[32_000..].to_vec();
        let mut append = tokio::task::spawn_blocking(move || {
            for packet in packets.chunks_exact(1_280) {
                capture.append(packet)?;
            }
            Ok::<_, SessionError>(())
        });
        let progressed = tokio::time::timeout(Duration::from_secs(1), &mut append).await;
        let completed_while_paused = progressed.is_ok();
        let published_capture = session.progress().borrow().captured_bytes;
        let buffered = {
            let pcm = session.inner.lock_pcm().unwrap();
            (pcm.buffer_start, pcm.buffer.clone())
        };
        // Release the real filesystem writer even when the RED assertion will fail.
        drop(release);
        io.await.unwrap().unwrap();
        match progressed {
            Ok(result) => result.unwrap().unwrap(),
            Err(_) => append.await.unwrap().unwrap(),
        }
        session.inner.cancel.send_replace(true);
        session.finish_capture().unwrap();
        let saved = session.snapshot().unwrap();
        assert_eq!(saved.captured_bytes, bytes.len() as u64);
        assert!(saved.capture_complete);
        for chunk in saved.chunks.iter().filter(|chunk| chunk.text.is_none()) {
            let audio = session.inner.journal.read_audio(chunk.index).unwrap();
            let end = chunk.end_byte as usize;
            assert_eq!(&audio[44..], &bytes[end - (audio.len() - 44)..end]);
        }
        Arc::try_unwrap(session)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        outcomes.push((confirming, completed_while_paused, published_capture));
        assert_eq!(buffered, (32_000, bytes[32_000..].to_vec()));
    }
    assert!(
        outcomes
            .iter()
            .all(|(_, done, captured)| *done && *captured == 70_400),
        "Capture waited for unrelated durable I/O: (confirming, completed, captured)={outcomes:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sender_and_capture_seals_use_one_real_persistence_worker_and_the_original_queue_bound() {
    use crate::microphone::{BUFFER_BYTES, CaptureBuffer, OUTPUT_BUFFER_COUNT};
    for sender_owned in [false, true] {
        let directory = TemporaryRoot::new();
        let (store, journal) = journal(&directory);
        let service = Service::new(Mode::Healthy);
        let session = Arc::new(StreamingSession::start(journal, Arc::new(service), true).unwrap());
        let expected = 192_000;
        let bytes: Vec<_> = (0..expected / 2)
            .flat_map(|index| ((index % 97) as i16 - 48).to_le_bytes())
            .collect();
        let completed = Arc::new(AtomicU64::new(0));
        let returned = completed.clone();
        let writer = session.clone();
        let (queue, receiver) = mpsc::channel(OUTPUT_BUFFER_COUNT);
        let (stopped, stopped_state) = watch::channel(false);
        let capture = tokio::task::spawn_blocking(move || {
            let discard = AtomicBool::new(false);
            crate::dictation::persist_capture_with_room(
                receiver,
                &discard,
                &stopped,
                || writer.reserve_capture(&discard),
                |pcm, slot| {
                    slot.unwrap().append(pcm)?;
                    returned.fetch_add(pcm.len() as u64, Ordering::Release);
                    Ok(())
                },
            )
        });
        if sender_owned {
            for packet in bytes[..32_000].chunks_exact(BUFFER_BYTES) {
                assert!(queue.send(CaptureBuffer(packet.to_vec())).await.is_ok());
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                while completed.load(Ordering::Acquire) != 32_000 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
        }
        let (release, entered) = store.pause_next_fixture_write();
        let owner = session.inner.clone();
        let (_stop, stopped_sender) = watch::channel(false);
        let sending = sender_owned.then(|| {
            tokio::spawn(async move {
                checkpoint_for_send(owner, stopped_sender, 32_000, "real-worker-sender-seal").await
            })
        });
        if !sender_owned {
            for packet in bytes[..64_000].chunks_exact(BUFFER_BYTES) {
                assert!(queue.send(CaptureBuffer(packet.to_vec())).await.is_ok());
            }
        }
        tokio::time::timeout(Duration::from_secs(5), entered)
            .await
            .unwrap()
            .unwrap();
        let inflight = if sender_owned { 32_000 } else { 64_000 };
        let pcm_end = 128_000;
        {
            for packet in bytes[inflight..pcm_end].chunks_exact(BUFFER_BYTES) {
                assert!(queue.send(CaptureBuffer(packet.to_vec())).await.is_ok());
            }
        }
        let full = tokio::time::timeout(Duration::from_secs(5), async {
            while session.inner.capture.captured.load(Ordering::Acquire) != pcm_end as u64 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        let buffered = {
            let pcm = session.inner.lock_pcm().unwrap();
            assert_eq!(pcm.sealing_bytes + pcm.buffer.len(), 128_000);
            assert_eq!(pcm.reserved_bytes, 0);
            (pcm.buffer_start, pcm.buffer.clone(), pcm.buffer.capacity())
        };
        let window_filled_before_release = full.is_ok();
        if window_filled_before_release {
            for packet in bytes[pcm_end..].chunks_exact(BUFFER_BYTES) {
                assert!(queue.try_send(CaptureBuffer(packet.to_vec())).is_ok());
            }
        }
        let bounded = matches!(
            queue.try_send(CaptureBuffer(vec![0; BUFFER_BYTES])),
            Err(mpsc::error::TrySendError::Full(_))
        );
        let completed_while_paused = completed.load(Ordering::Acquire);
        let owned_while_paused = !capture.is_finished() && !*stopped_state.borrow();
        let durable = session.progress().borrow().saved_bytes;
        drop(release);
        if let Some(sending) = sending {
            assert!(sending.await.unwrap().unwrap());
        }
        drop(queue);
        capture.await.unwrap().unwrap();
        session.inner.cancel.send_replace(true);
        session.finish_capture().unwrap();
        let saved = session.snapshot().unwrap();
        let mut previous = 0;
        for chunk in &saved.chunks {
            let audio = session.inner.journal.read_audio(chunk.index).unwrap();
            assert_eq!(chunk.end_byte, previous + (audio.len() - 44) as u64);
            assert_eq!(
                &audio[44..],
                &bytes[previous as usize..chunk.end_byte as usize]
            );
            previous = chunk.end_byte;
        }
        Arc::try_unwrap(session)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        assert!(
            window_filled_before_release,
            "Single capture worker blocked in its own durable seal; sender_owned={sender_owned}"
        );
        assert!(bounded && owned_while_paused && *stopped_state.borrow());
        assert_eq!(completed_while_paused, pcm_end as u64);
        assert_eq!(completed.load(Ordering::Acquire), expected as u64);
        assert_eq!(
            buffered,
            (inflight as u64, bytes[inflight..pcm_end].to_vec(), 128_000)
        );
        assert_eq!(durable, 0);
        let unique_ram = inflight + buffered.1.len() + OUTPUT_BUFFER_COUNT * BUFFER_BYTES;
        assert_eq!(unique_ram, expected);
        assert!(unique_ram <= 192_000);
        assert_eq!(previous, expected as u64);
        assert_eq!(saved.captured_bytes, expected as u64);
        assert!(saved.capture_complete);
        assert_eq!(saved.context, "synthetic context");
        println!(
            "single-persist_capture:sender_owned={sender_owned};unique_ram={unique_ram};upper_bound=192000;source_queue_slots={OUTPUT_BUFFER_COUNT};joined=true"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealer_failure_wakes_reserved_capture_waiters_and_keeps_unpublished_pcm_and_cipher() {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = TemporaryRoot::new();
    let (store, journal) = journal(&directory);
    let id = journal.id().to_owned();
    let path = directory.0.join(&id).join("manifest.enc");
    let before = std::fs::read(&path).unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    let service = Service::new(Mode::Healthy);
    let session = Arc::new(StreamingSession::start(journal, Arc::new(service), true).unwrap());
    let (release, entered) = store.pause_next_fixture_write();
    session.append(&vec![1; 64_000]).unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    session.append(&vec![2; 64_000]).unwrap();
    let writer = session.clone();
    let mut waiting =
        tokio::task::spawn_blocking(move || writer.reserve_capture(&AtomicBool::new(false)));
    let blocked = tokio::time::timeout(Duration::from_millis(100), &mut waiting)
        .await
        .is_err();
    drop(release);
    let failure = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(failure, Err(SessionError::Storage(_))) && blocked);
    {
        let state = session.inner.lock().unwrap();
        assert_eq!(state.sealing.as_ref().unwrap().pcm.0, vec![1; 64_000]);
        assert_eq!(state.manifest.captured_bytes, 0);
    }
    {
        let pcm = session.inner.lock_pcm().unwrap();
        assert_eq!(pcm.sealing_bytes, 64_000);
        assert_eq!(pcm.buffer, vec![2; 64_000]);
        assert_eq!(pcm.reserved_bytes, 0);
    }
    let progress = session.progress().borrow().clone();
    assert!(progress.storage_failed && !progress.capture_complete);
    assert_eq!(
        (progress.captured_bytes, progress.saved_bytes),
        (128_000, 0)
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(matches!(
        Arc::try_unwrap(session).ok().unwrap().shutdown().await,
        Err(SessionError::Storage(_))
    ));
    drop(held);
    assert_eq!(store.fixture_snapshot(&id).unwrap().captured_bytes, 0);
    store.delete_idle(&id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_wakes_room_waiters_but_joins_the_started_seal_before_releasing_journal_ownership()
{
    let directory = TemporaryRoot::new();
    let (store, journal) = journal(&directory);
    let id = journal.id().to_owned();
    let service = Service::new(Mode::Healthy);
    let session = Arc::new(StreamingSession::start(journal, Arc::new(service), true).unwrap());
    let (release, entered) = store.pause_next_fixture_write();
    session.append(&vec![1; 64_000]).unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    session.append(&vec![2; 64_000]).unwrap();
    let waiter = session.clone();
    let mut waiting =
        tokio::task::spawn_blocking(move || waiter.reserve_capture(&AtomicBool::new(false)));
    let blocked = tokio::time::timeout(Duration::from_millis(100), &mut waiting)
        .await
        .is_err();
    session.inner.cancel();
    let wake = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(blocked && matches!(wake, Err(SessionError::Closed)));
    let session = Arc::try_unwrap(session).ok().unwrap();
    let finishing = tokio::task::spawn_blocking(move || {
        session.finish_capture().unwrap();
        session
    });
    let mut shutdown = tokio::spawn(async move { finishing.await.unwrap().shutdown().await });
    let premature = tokio::time::timeout(Duration::from_millis(100), &mut shutdown).await;
    let joined_too_soon = premature.is_ok();
    drop(release);
    match premature {
        Ok(result) => result.unwrap().unwrap(),
        Err(_) => shutdown.await.unwrap().unwrap(),
    }
    assert!(!joined_too_soon);
    let saved = store.fixture_snapshot(&id).unwrap();
    assert!(saved.capture_complete && saved.captured_bytes == 128_000);
    let reader = store.open(&id).unwrap();
    let mut bytes = Vec::new();
    for chunk in &saved.chunks {
        bytes.extend_from_slice(&reader.read_audio(chunk.index).unwrap()[44..]);
    }
    assert_eq!(bytes, [vec![1; 64_000], vec![2; 64_000]].concat());
    drop(reader);
    store.delete_idle(&id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_slot_enforces_the_same_native_packet_guard_in_tests_and_releases_every_reservation()
 {
    let directory = TemporaryRoot::new();
    let (_, journal) = journal(&directory);
    let service = Service::new(Mode::Healthy);
    let session = StreamingSession::start(journal, Arc::new(service), true).unwrap();
    let discard = AtomicBool::new(false);
    let mut slot = session.reserve_capture(&discard).unwrap().unwrap();
    assert!(matches!(
        slot.append(&[0; 1_282]),
        Err(SessionError::Storage(_))
    ));
    {
        let pcm = session.inner.lock_pcm().unwrap();
        assert_eq!(
            (pcm.captured, pcm.buffer.len(), pcm.reserved_bytes),
            (0, 0, 1_280)
        );
    }
    assert_eq!(session.progress().borrow().captured_bytes, 0);
    drop(slot);
    assert_eq!(session.inner.lock_pcm().unwrap().reserved_bytes, 0);
    let mut slot = session.reserve_capture(&discard).unwrap().unwrap();
    slot.append(&[1; 1_280]).unwrap();
    drop(slot);
    let mut slot = session.reserve_capture(&discard).unwrap().unwrap();
    slot.append(&[2; 640]).unwrap();
    assert_eq!(session.inner.lock_pcm().unwrap().reserved_bytes, 640);
    drop(slot);
    let mut slot = session.reserve_capture(&discard).unwrap().unwrap();
    session.inner.cancel();
    assert!(matches!(slot.append(&[3; 640]), Err(SessionError::Closed)));
    drop(slot);
    assert_eq!(session.inner.lock_pcm().unwrap().reserved_bytes, 0);
    assert_eq!(session.progress().borrow().captured_bytes, 1_920);
    session.finish_capture().unwrap();
    assert_eq!(session.snapshot().unwrap().captured_bytes, 1_920);
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reserved_next_frame_cannot_be_stolen_by_another_append_or_hold_the_pcm_mutex() {
    let directory = TemporaryRoot::new();
    let (_, journal) = journal(&directory);
    let service = Service::new(Mode::Healthy);
    let session = Arc::new(StreamingSession::start(journal, Arc::new(service), true).unwrap());
    let mut slot = session
        .reserve_capture(&AtomicBool::new(false))
        .unwrap()
        .unwrap();
    let state = session.inner.lock().unwrap();
    session.append(&vec![1; 126_720]).unwrap();
    assert_eq!(session.inner.lock_pcm().unwrap().reserved_bytes, 1_280);
    slot.append(&[2; 1_280]).unwrap();
    drop(slot);
    assert_eq!(
        session.inner.capture.captured.load(Ordering::Acquire),
        128_000
    );
    assert_eq!(session.inner.lock_pcm().unwrap().reserved_bytes, 0);
    drop(state);
    let session = Arc::try_unwrap(session).ok().unwrap();
    session.finish_capture().unwrap();
    let saved = session.snapshot().unwrap();
    assert_eq!(saved.captured_bytes, 128_000);
    assert!(saved.chunks.iter().all(|chunk| chunk.audio_bytes as usize
        <= voiceprompt_client_protocol::recovery::MAX_AUDIO_BYTES));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sealer_worker_panic_and_poison_are_visible_and_release_room_waiters_without_saved_success()
{
    for poison in [false, true] {
        let directory = TemporaryRoot::new();
        let (_, journal) = journal(&directory);
        let service = Service::new(Mode::Healthy);
        let session = Arc::new(StreamingSession::start(journal, Arc::new(service), true).unwrap());
        session.append(&[1; 1_280]).unwrap();
        let owner = session.inner.clone();
        let job = tokio::task::spawn_blocking(move || {
            if poison {
                let _guard = owner.capture.pcm.lock().unwrap();
                panic!("Controlled own PCM mutex poison.");
            }
            panic!("Controlled joined seal worker panic.");
        });
        assert!(matches!(
            join_seal(&session.inner, job).await,
            Err(SessionError::Worker)
        ));
        assert!(session.progress().borrow().storage_failed);
        assert_eq!(session.progress().borrow().saved_bytes, 0);
        assert!(matches!(
            session.reserve_capture(&AtomicBool::new(false)),
            Err(SessionError::Storage(_))
        ));
        let shutdown = Arc::try_unwrap(session).ok().unwrap().shutdown().await;
        if poison {
            assert!(matches!(shutdown, Ok(()) | Err(SessionError::Storage(_))));
        } else {
            shutdown.unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_checkpoint_race_orders_skip_already_durable_through_and_legitimate_later_cursor_is_saved()
 {
    for sender_first in [false, true] {
        let directory = TemporaryRoot::new();
        let (_, journal) = journal(&directory);
        let service = Service::new(Mode::Healthy);
        let session = StreamingSession::start(journal, Arc::new(service), true).unwrap();
        let (_stop, stopped) = watch::channel(false);
        {
            let mut state = session.inner.lock().unwrap();
            session.append(&vec![0; 64_000]).unwrap();
            // Hold D to deterministically select which owner publishes first.
            if sender_first {
                session.inner.checkpoint(&mut state, false).unwrap();
            }
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while session.progress().borrow().saved_bytes != 64_000 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        session.append(&[1; 2_560]).unwrap();
        let before = session.snapshot().unwrap();
        assert!(
            checkpoint_for_send(
                session.inner.clone(),
                stopped.clone(),
                64_000,
                "already-covered"
            )
            .await
            .unwrap()
        );
        assert_eq!(session.snapshot().unwrap(), before);
        assert_eq!(before.chunks.len(), 1);
        assert!(
            checkpoint_for_send(session.inner.clone(), stopped, 66_560, "new-through")
                .await
                .unwrap()
        );
        let covered = session.snapshot().unwrap();
        assert_eq!(covered.captured_bytes, 66_560);
        assert_eq!(covered.chunks.len(), 2);
        session.finish_capture().unwrap();
        session.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_inflight_seal_keeps_both_pcm_owners_and_never_promotes_the_durable_prefix() {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = TemporaryRoot::new();
    let (store, journal) = journal(&directory);
    let id = journal.id().to_owned();
    let path = directory.0.join(&id).join("manifest.enc");
    let before = std::fs::read(&path).unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    let service = Service::new(Mode::Healthy);
    let session = Arc::new(StreamingSession::start(journal, Arc::new(service), true).unwrap());
    let bytes: Vec<_> = (0..35_200)
        .flat_map(|index| ((index % 97) as i16 - 48).to_le_bytes())
        .collect();
    session.append(&bytes[..32_000]).unwrap();
    let (release, entered) = store.pause_next_fixture_write();
    let owner = session.inner.clone();
    let (_stop, stopped) = watch::channel(false);
    let sealing = tokio::spawn(async move {
        checkpoint_for_send(owner, stopped, 32_000, "failed-sender-seal").await
    });
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    for packet in bytes[32_000..].chunks_exact(1_280) {
        session.append(packet).unwrap();
    }
    drop(release);
    let error = sealing.await.unwrap().unwrap_err();
    let progress = session.progress().borrow().clone();
    {
        let state = session.inner.lock().unwrap();
        let owned = state.sealing.as_ref().unwrap();
        assert_eq!(owned.end, 32_000);
        assert_eq!(owned.pcm.0, bytes[..32_000]);
        assert_eq!(state.manifest.captured_bytes, 0);
        assert!(!state.manifest.capture_complete);
    }
    {
        let pcm = session.inner.lock_pcm().unwrap();
        assert_eq!(pcm.buffer_start, 32_000);
        assert_eq!(pcm.buffer, bytes[32_000..]);
    }
    assert!(matches!(
        session.append(&[0, 0]),
        Err(SessionError::Storage(_))
    ));
    assert!(matches!(
        session.finish_capture(),
        Err(SessionError::Storage(_))
    ));
    assert!(progress.storage_failed && !progress.capture_complete);
    assert_eq!((progress.captured_bytes, progress.saved_bytes), (70_400, 0));
    assert!(error.to_string().contains("MoveFileExW"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(held);
    assert_eq!(store.fixture_snapshot(&id).unwrap().captured_bytes, 0);
    Arc::try_unwrap(session)
        .ok()
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finish_and_cancel_join_the_actual_pumps_paused_confirmation_without_losing_new_pcm() {
    for cancel in [false, true] {
        let directory = TemporaryRoot::new();
        let (store, journal) = journal(&directory);
        let id = journal.id().to_owned();
        let service = Service::new(Mode::Healthy);
        let session =
            Arc::new(StreamingSession::start(journal, Arc::new(service.clone()), false).unwrap());
        session.append(&vec![0; 64_000]).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while service.committed.load(Ordering::Acquire) != 64_000 {
                service.changed.notified().await;
            }
        })
        .await
        .unwrap();
        let (release, entered) = store.pause_next_fixture_write();
        let events = service.connection_events.lock().unwrap()[0].clone();
        events
            .send(Event::Message(DictationMessage::Confirmed {
                item_id: "i".into(),
                text: "prefix".into(),
                byte_end: 64_000,
            }))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), entered)
            .await
            .unwrap()
            .unwrap();
        let writer = session.clone();
        let mut append = tokio::task::spawn_blocking(move || {
            for _ in 0..75 {
                writer.append(&[0; 1_280])?;
            }
            Ok::<_, SessionError>(())
        });
        let progressed = tokio::time::timeout(Duration::from_secs(1), &mut append).await;
        if progressed.is_err() {
            drop(release);
            append.await.unwrap().unwrap();
            Arc::try_unwrap(session)
                .ok()
                .unwrap()
                .shutdown()
                .await
                .unwrap();
            panic!("Actual receive_text confirmation blocked PCM progress.");
        }
        progressed.unwrap().unwrap().unwrap();
        let session = Arc::try_unwrap(session).ok().unwrap();
        if cancel {
            session.inner.cancel.send_replace(true);
            assert!(matches!(session.append(&[0, 0]), Err(SessionError::Closed)));
        }
        let mut finishing = tokio::spawn(async move {
            let session = tokio::task::spawn_blocking(move || {
                session.finish_capture()?;
                Ok::<_, SessionError>(session)
            })
            .await
            .unwrap()?;
            if !cancel {
                assert!(
                    session
                        .wait_for_completion(Duration::from_secs(5), false)
                        .await?
                        .is_some()
                );
            }
            session.shutdown().await
        });
        let premature = tokio::time::timeout(Duration::from_millis(100), &mut finishing).await;
        let joined_while_paused = premature.is_ok();
        drop(release);
        match premature {
            Ok(result) => result.unwrap().unwrap(),
            Err(_) => finishing.await.unwrap().unwrap(),
        }
        let saved = store.fixture_snapshot(&id).unwrap();
        assert!(!joined_while_paused);
        assert!(saved.capture_complete);
        assert_eq!(saved.captured_bytes, 160_000);
        assert_eq!(saved.context, "synthetic context");
        assert_eq!(saved.confirmed_bytes, if cancel { 64_000 } else { 160_000 });
        assert_eq!(saved.chunks.len(), 2);
        for chunk in saved.chunks.iter().filter(|chunk| chunk.text.is_none()) {
            let reader = store.open(&id).unwrap();
            assert!(
                reader.read_audio(chunk.index).unwrap()[44..]
                    .iter()
                    .all(|byte| *byte == 0)
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_and_confirmation_use_only_owned_publications_while_fresh_manifest_io_is_blocked()
{
    use crate::recovery_store::ManifestReadPhase;
    for phase in [
        ManifestReadPhase::Open,
        ManifestReadPhase::Read,
        ManifestReadPhase::Unprotect,
    ] {
        let directory = TemporaryRoot::new();
        let (store, journal) = journal(&directory);
        let id = journal.id().to_owned();
        let service = Service::new(Mode::Healthy);
        let session =
            Arc::new(StreamingSession::start(journal, Arc::new(service.clone()), false).unwrap());
        let bytes: Vec<_> = (0..64000)
            .flat_map(|index| ((index % 3000) as i16 + 1000).to_le_bytes())
            .collect();
        let writer = session.clone();
        let first = bytes[..64000].to_vec();
        tokio::task::spawn_blocking(move || writer.append(&first))
            .await
            .unwrap()
            .unwrap();
        let pin = store.fixture_pin_manifest(&id).unwrap();
        let (release, entered) = store.pause_next_manifest_read(phase);
        let reader = tokio::task::spawn_blocking(move || pin.read());
        tokio::time::timeout(Duration::from_secs(5), entered)
            .await
            .unwrap()
            .unwrap();
        let reads = store.fixture_manifest_read_count();
        let writer = session.clone();
        let rest = bytes[64000..].to_vec();
        tokio::task::spawn_blocking(move || writer.append(&rest))
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if service
                    .sent
                    .lock()
                    .unwrap()
                    .first()
                    .is_some_and(|sent| !sent.is_empty())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let events = service.connection_events.lock().unwrap()[0].clone();
        events.send(Event::Failure).await.unwrap();
        let replayed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if service
                    .sent
                    .lock()
                    .unwrap()
                    .get(1)
                    .is_some_and(|sent| sent.len() == bytes.len())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        if replayed.is_err() {
            drop(release);
            let _ = reader.await;
            Arc::try_unwrap(session)
                .ok()
                .unwrap()
                .shutdown()
                .await
                .unwrap();
            panic!("Blocked manifest inspection stalled reconnect replay.");
        }
        assert_eq!(*service.starts.lock().unwrap(), vec![0, 0]);
        assert_eq!(service.sent.lock().unwrap()[1], bytes);
        let writer = session.clone();
        tokio::task::spawn_blocking(move || writer.finish_capture())
            .await
            .unwrap()
            .unwrap();
        let complete = session
            .wait_for_completion(Duration::from_secs(2), false)
            .await
            .unwrap();
        let state = session.snapshot().unwrap();
        let no_fresh_reads = store.fixture_manifest_read_count() == reads;
        let still_blocked = !reader.is_finished();
        drop(release);
        assert!(reader.await.unwrap().is_err());
        assert!(complete.is_some() && no_fresh_reads && still_blocked);
        assert_eq!(
            (state.captured_bytes, state.confirmed_bytes),
            (128000, 128000)
        );
        assert_eq!(state.context, "synthetic context");
        Arc::try_unwrap(session)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_durable_checkpoint_timeline_records_io_while_pcm_append_remains_memory_only() {
    let directory = TemporaryRoot::new();
    let (_, journal) = journal(&directory);
    let service = Service::new(Mode::Healthy);
    let session =
        Arc::new(StreamingSession::start(journal, Arc::new(service.clone()), false).unwrap());
    let capture = crate::mutation_trace::Capture::new();
    let writer = session.clone();
    let recording = capture.clone();
    tokio::task::spawn_blocking(move || {
        let _scope = recording.enter();
        writer.append(&vec![0; 32_000]).unwrap();
        let mut state = writer
            .inner
            .lock_named("explicit-timeline-checkpoint")
            .unwrap();
        writer.inner.checkpoint(&mut state, false).unwrap();
    })
    .await
    .unwrap();
    let timeline = capture.snapshot();
    assert_eq!(timeline["metadata_unavailable"], false);
    assert_eq!(timeline["dropped_events"], 0);
    assert!(timeline["active"].as_array().unwrap().is_empty());
    let completed = timeline["completed"].as_array().unwrap();
    for phase in [
        "append",
        "engine-lock-wait",
        "engine-lock-held",
        "root-lock-wait",
        "root-lock-held",
        "path-reparse-preflight",
        "quota-scan",
        "checkpoint-clone-hash-validate",
        "manifest-size-preflight-encode",
        "dpapi-protect",
        "staging-create",
        "staging-write",
        "staging-flush",
        "staging-close",
        "staging-rename",
        "manifest-reader-publication",
        "own-durable-publication-view",
        "manifest-cursor-update",
        "cursor-progress-publication",
    ] {
        assert!(
            completed.iter().any(|event| event["phase"] == phase),
            "Missing actual mutation phase: {phase}"
        );
        assert!(
            !completed
                .iter()
                .any(|event| event["phase"] == "post-publication-readback")
        );
        assert_eq!(
            completed
                .iter()
                .filter(|event| event["phase"] == "encrypted-read")
                .count(),
            0
        );
    }
    for event in completed {
        assert!(event["completed_ns"].as_u64().unwrap() >= event["started_ns"].as_u64().unwrap());
    }
    assert_eq!(
        completed
            .iter()
            .filter(|event| event["phase"] == "staging-rename")
            .count(),
        2
    );
    assert_eq!(
        completed
            .iter()
            .filter(|event| event["phase"] == "manifest-size-preflight-encode")
            .count(),
        1
    );
    assert!(
        !completed
            .iter()
            .any(|event| event["phase"] == "manifest-encode")
    );
    assert!(
        completed
            .iter()
            .filter(|event| event["phase"] == "quota-scan")
            .all(|event| {
                event["entries"].as_u64().unwrap() == event["metadata_probes"].as_u64().unwrap()
            })
    );
    println!("writer-checkpoint-mutation-timeline:{timeline}");
    session.finish_capture().unwrap();
    session
        .wait_for_completion(Duration::from_secs(5), false)
        .await
        .unwrap();
    Arc::try_unwrap(session)
        .ok()
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_waits_for_a_pinned_read_and_replays_only_the_original_confirmed_cursor() {
    let directory = TemporaryRoot::new();
    let (store, journal) = journal(&directory);
    let bytes = vec![0; 64000];
    let wav = to_wav(&bytes).unwrap();
    journal
        .checkpoint(
            &[],
            64000,
            &[AudioCheckpoint {
                wav: &wav,
                end_byte: 64000,
                overlaps_previous: false,
            }],
            false,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    let (release, blocked) = store.pause_next_pinned_read();
    let service = Service::new(Mode::Healthy);
    let session = StreamingSession::start(journal, Arc::new(service.clone()), false).unwrap();
    tokio::time::timeout(Duration::from_secs(5), blocked)
        .await
        .unwrap()
        .unwrap();
    let events = service.connection_events.lock().unwrap()[0].clone();
    events.send(Event::Failure).await.unwrap();
    session.finish_capture().unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        service.connections.load(Ordering::Acquire),
        1,
        "Reconnect cannot detach old read ownership."
    );
    drop(release);
    assert_eq!(
        session
            .wait_for_completion(Duration::from_secs(5), false)
            .await
            .unwrap()
            .as_deref(),
        Some("synthetic confirmed")
    );
    assert_eq!(*service.starts.lock().unwrap(), [0, 0]);
    assert!(service.sent.lock().unwrap()[0].is_empty());
    assert_eq!(service.sent.lock().unwrap()[1], bytes);
    assert_eq!(session.snapshot().unwrap().confirmed_bytes, 64000);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn async_readers_do_not_bypass_a_pending_synchronous_capture_writer() {
    let directory = TemporaryRoot::new();
    let (_store, journal) = journal(&directory);
    let service = Service::new(Mode::Healthy);
    let session = StreamingSession::start(journal, Arc::new(service), false).unwrap();
    session
        .inner
        .synchronous_waiters
        .store(1, Ordering::Release);
    let reader = session.inner.clone();
    let pending = tokio::spawn(async move {
        drop(reader.async_lock("fixture-async-reader").await.unwrap());
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    let bypassed_writer = pending.is_finished();
    session
        .inner
        .synchronous_waiters
        .store(0, Ordering::Release);
    pending.await.unwrap();
    assert!(
        !bypassed_writer,
        "An immediate async reader bypassed a queued capture write."
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn confirmed_prefix_survives_failure_and_replay_starts_at_the_durable_cursor() {
    let directory = TemporaryRoot::new();
    let (_store, journal) = journal(&directory);
    let service = Service::new(Mode::PartialDisconnect);
    let session = StreamingSession::start(journal, Arc::new(service.clone()), false).unwrap();
    session.append(&[0; 96000]).unwrap();
    session.finish_capture().unwrap();
    assert_eq!(
        session
            .wait_for_completion(Duration::from_secs(5), false)
            .await
            .unwrap()
            .as_deref(),
        Some("part1 part2")
    );
    assert_eq!(*service.starts.lock().unwrap(), [0, 64000]);
    assert_eq!(service.sent.lock().unwrap()[1], [0; 32000]);
    assert_eq!(session.snapshot().unwrap().confirmed_bytes, 96000);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_confirmation_beyond_the_sent_commit_never_releases_audio_or_delivers_text() {
    let directory = TemporaryRoot::new();
    let (store, journal) = journal(&directory);
    let id = journal.id().to_owned();
    let service = Service::new(Mode::InvalidConfirmation);
    let session = StreamingSession::start(journal, Arc::new(service), false).unwrap();
    session.append(&[1; 6400]).unwrap();
    session.finish_capture().unwrap();
    assert!(
        session
            .wait_for_completion(Duration::from_millis(100), false)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(session.snapshot().unwrap().confirmed_bytes, 0);
    session.shutdown().await.unwrap();
    assert_eq!(
        store.open(&id).unwrap().read_audio(0).unwrap(),
        to_wav(&[1; 6400]).unwrap()
    );
}

#[tokio::test]
async fn healthy_stream_confirms_exact_samples_and_never_uses_provisional_as_final() {
    let directory = TemporaryRoot::new();
    let (_store, journal) = journal(&directory);
    let service = Service::new(Mode::Healthy);
    let session = StreamingSession::start(journal, Arc::new(service.clone()), false).unwrap();
    session.append(&[1; 6400]).unwrap();
    session.finish_capture().unwrap();
    assert_eq!(
        session
            .wait_for_completion(Duration::from_secs(5), false)
            .await
            .unwrap()
            .as_deref(),
        Some("synthetic confirmed")
    );
    assert_eq!(service.sent.lock().unwrap()[0], [1; 6400]);
    let state = session.snapshot().unwrap();
    assert_eq!(state.confirmed_bytes, 6400);
    assert!(state.chunks.iter().all(|chunk| chunk.text.is_some()));
    assert!(state.used_streaming);
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn provisional_failure_stop_timeout_retains_all_audio_and_empty_confirmed_text() {
    let directory = TemporaryRoot::new();
    let (store, journal) = journal(&directory);
    let id = journal.id().to_owned();
    let service = Service::new(Mode::ProvisionalFailure);
    let session = StreamingSession::start(journal, Arc::new(service), false).unwrap();
    session.append(&[1; 6400]).unwrap();
    session.finish_capture().unwrap();
    assert!(
        session
            .wait_for_completion(Duration::from_millis(100), false)
            .await
            .unwrap()
            .is_none()
    );
    let state = session.snapshot().unwrap();
    assert_eq!(state.confirmed_bytes, 0);
    assert_eq!(state.confirmed_text, "");
    tokio::time::timeout(Duration::from_secs(1), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    let journal = store.open(&id).unwrap();
    assert_eq!(journal.read_audio(0).unwrap(), to_wav(&[1; 6400]).unwrap());
}

#[tokio::test]
async fn reconnect_replays_unconfirmed_pcm_without_silence_or_sample_loss() {
    let directory = TemporaryRoot::new();
    let (_store, journal) = journal(&directory);
    let service = Service::new(Mode::DisconnectOnce);
    let session = StreamingSession::start(journal, Arc::new(service.clone()), false).unwrap();
    let pcm: Vec<_> = (0..48000)
        .flat_map(|index| ((index % 3000) as i16 + 1000).to_le_bytes())
        .collect();
    session.append(&pcm).unwrap();
    session.finish_capture().unwrap();
    assert!(
        session
            .wait_for_completion(Duration::from_secs(5), false)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(*service.starts.lock().unwrap(), [0, 0]);
    assert_eq!(service.sent.lock().unwrap()[0], pcm);
    assert_eq!(service.sent.lock().unwrap()[1], pcm);
    assert_eq!(
        session.snapshot().unwrap().confirmed_bytes,
        pcm.len() as u64
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn batch_recovery_preserves_confirmed_context_and_dedupes_only_the_seam() {
    let directory = TemporaryRoot::new();
    let (_store, journal) = journal(&directory);
    let first = to_wav(&[1; 6400]).unwrap();
    let second = to_wav(&[2; 6400]).unwrap();
    journal
        .checkpoint(
            &[],
            12800,
            &[
                AudioCheckpoint {
                    wav: &first,
                    overlaps_previous: false,
                    end_byte: 6400,
                },
                AudioCheckpoint {
                    wav: &second,
                    overlaps_previous: false,
                    end_byte: 12800,
                },
            ],
            true,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    journal
        .confirm_streaming(
            6400,
            "existing tail".into(),
            vec![1; 6400],
            false,
            Some(2),
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    let service = Service::new(Mode::Recovery);
    let session = StreamingSession::start(journal, Arc::new(service.clone()), true).unwrap();
    assert_eq!(
        session
            .wait_for_completion(Duration::from_secs(5), false)
            .await
            .unwrap()
            .as_deref(),
        Some("existing tail pending")
    );
    let expected: Vec<_> = [vec![1; 6400], vec![2; 6400]].concat();
    assert_eq!(service.batches.lock().unwrap()[0], expected);
    assert_eq!(service.connections.load(Ordering::Acquire), 0);
    let state = session.snapshot().unwrap();
    assert_eq!(state.confirmed_bytes, 12800);
    assert!(state.used_fallback && state.used_streaming);
    session.shutdown().await.unwrap();
}

async fn until(service: &Service, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let changed = service.changed.notified();
            if ready() {
                return;
            }
            changed.await;
        }
    })
    .await
    .expect("Synthetic engine did not reach the explicit service boundary.");
}

#[tokio::test(start_paused = true)]
async fn virtual_fifty_minute_rotation_continues_capture_from_the_confirmed_cursor() {
    let directory = TemporaryRoot::new();
    let (_store, journal) = journal(&directory);
    let service = Service::new(Mode::Healthy);
    let session = StreamingSession::start(journal, Arc::new(service.clone()), false).unwrap();
    session.append(&[1; 6400]).unwrap();
    until(&service, || {
        service
            .sent
            .lock()
            .unwrap()
            .first()
            .is_some_and(|pcm| pcm.len() == 6400)
    })
    .await;

    tokio::time::advance(Duration::from_secs(50 * 60)).await;
    until(&service, || service.starts.lock().unwrap().len() == 2).await;
    let rotated = session.snapshot().unwrap();
    assert!(!rotated.capture_complete);
    assert_eq!(rotated.confirmed_bytes, 6400);
    assert_eq!(*service.starts.lock().unwrap(), [0, 6400]);

    session.append(&[2; 6400]).unwrap();
    session.finish_capture().unwrap();
    assert_eq!(
        session
            .wait_for_completion(Duration::from_secs(5), false)
            .await
            .unwrap()
            .as_deref(),
        Some("synthetic confirmed synthetic confirmed")
    );
    assert_eq!(
        *service.sent.lock().unwrap(),
        [vec![1; 6400], vec![2; 6400]]
    );
    let finished = session.snapshot().unwrap();
    assert!(finished.capture_complete);
    assert_eq!(finished.confirmed_bytes, 12800);
    session.shutdown().await.unwrap();
}

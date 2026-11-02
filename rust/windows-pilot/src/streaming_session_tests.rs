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
    starts: Mutex<Vec<u64>>,
    sent: Mutex<Vec<Vec<u8>>>,
    batches: Mutex<Vec<Vec<u8>>>,
    changed: Notify,
}

impl Service {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            connections: AtomicUsize::new(0),
            starts: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
            batches: Mutex::new(Vec::new()),
            changed: Notify::new(),
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

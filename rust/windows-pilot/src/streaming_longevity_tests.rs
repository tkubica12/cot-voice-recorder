use super::*;
use crate::platform::{WriteMode, atomic_write, guard_recovery_path};
use crate::{DEFAULT_RECOVERY_QUOTA, NativeRecoveryStore};
use serde_json::json;
use std::path::Path;
use tokio::sync::mpsc;

const SCOPE: &str = "native-streaming-engine-realtime-synthetic-pcm";

struct Service {
    sent: AtomicU64,
    starts: Mutex<Vec<u64>>,
}

#[derive(Clone)]
struct Sender {
    service: Arc<Service>,
    position: Arc<Mutex<(u64, u64)>>,
    events: mpsc::Sender<DictationMessage>,
}

struct Receiver(mpsc::Receiver<DictationMessage>);

fn sample(index: u64) -> i16 {
    (index % 97) as i16 - 48
}

impl AudioSender for Sender {
    async fn audio(&self, pcm: &[u8]) -> Result<(), ApiError> {
        assert_eq!(pcm.len() % 2, 0);
        let mut position = self.position.lock().unwrap();
        for (index, bytes) in pcm.chunks_exact(2).enumerate() {
            assert_eq!(
                i16::from_le_bytes([bytes[0], bytes[1]]),
                sample(position.0 / 2 + index as u64),
                "Synthetic PCM was duplicated, reordered or replaced."
            );
        }
        position.0 += pcm.len() as u64;
        self.service
            .sent
            .fetch_add(pcm.len() as u64, Ordering::AcqRel);
        Ok(())
    }

    async fn commit(&self, finish: bool) -> Result<(), ApiError> {
        let (end, changed) = {
            let mut position = self.position.lock().unwrap();
            let changed = position.0 > position.1;
            position.1 = position.0;
            (position.0, changed)
        };
        if changed {
            self.events
                .send(DictationMessage::Confirmed {
                    item_id: format!("synthetic-{end}"),
                    text: "synthetic".into(),
                    byte_end: end,
                })
                .await
                .unwrap();
        }
        if finish {
            self.events
                .send(DictationMessage::Done { byte_end: end })
                .await
                .unwrap();
        }
        Ok(())
    }
}

impl EventReceiver for Receiver {
    async fn receive(&mut self) -> Result<DictationMessage, ApiError> {
        Ok(self
            .0
            .recv()
            .await
            .expect("Synthetic provider ended unexpectedly."))
    }
}

impl BatchBackend for Arc<Service> {
    async fn transcribe(&self, _: &[u8], _: &str, _: &str) -> Result<String, ApiError> {
        panic!("Healthy synthetic streaming must not silently use batch fallback.")
    }
}

impl StreamingBackend for Arc<Service> {
    type Sender = Sender;
    type Receiver = Receiver;

    async fn connect(&self, _: &str, start: u64, _: &str) -> Result<(Sender, Receiver), ApiError> {
        assert_eq!(start, self.sent.load(Ordering::Acquire));
        self.starts.lock().unwrap().push(start);
        let (events, receiver) = mpsc::channel(8);
        Ok((
            Sender {
                service: self.clone(),
                position: Arc::new(Mutex::new((start, start))),
                events,
            },
            Receiver(receiver),
        ))
    }
}

fn publish(root: &Path, name: &str, payload: serde_json::Value) {
    atomic_write(
        &root.join(name),
        &serde_json::to_vec(&payload).unwrap(),
        WriteMode::CreateNew,
    )
    .unwrap();
}

async fn control(root: &Path, name: &str, nonce: &str) {
    tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let path = root.join(name);
            guard_recovery_path(&path).unwrap();
            if path.try_exists().unwrap() {
                assert_eq!(std::fs::read_to_string(path).unwrap(), nonce);
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Explicit synthetic measurement controller did not respond.");
}

async fn capture(store: &NativeRecoveryStore, seconds: u64, realtime: bool) -> serde_json::Value {
    let service = Arc::new(Service {
        sent: AtomicU64::new(0),
        starts: Mutex::new(Vec::new()),
    });
    let journal = store
        .create(
            "auto".into(),
            "synthetic-only, no user auth or network".into(),
            true,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    let id = journal.id().to_owned();
    let session = StreamingSession::start(journal, Arc::new(service.clone()), false).unwrap();
    let progress = session.progress();
    let mut timer = tokio::time::interval(Duration::from_millis(20));
    let began = tokio::time::Instant::now();
    let mut frame = [0; 640];
    let mut maximum_lateness = Duration::ZERO;
    for index in 0..seconds * 50 {
        if realtime {
            timer.tick().await;
            let due = began + Duration::from_millis(index * 20);
            maximum_lateness =
                maximum_lateness.max(tokio::time::Instant::now().saturating_duration_since(due));
        }
        for (offset, bytes) in frame.chunks_exact_mut(2).enumerate() {
            bytes.copy_from_slice(&sample(index * 320 + offset as u64).to_le_bytes());
        }
        session.append(&frame).unwrap();
        assert!(!progress.borrow().storage_failed);
    }
    frame.fill(0);
    let elapsed = began.elapsed().as_secs_f64();
    if realtime {
        assert!(
            elapsed >= seconds as f64 - 0.1,
            "The wall clock was not exercised."
        );
    }
    session.finish_capture().unwrap();
    assert!(
        session
            .wait_for_completion(Duration::from_secs(5), false)
            .await
            .unwrap()
            .is_some()
    );
    let state = session.snapshot().unwrap();
    let expected = seconds * 32000;
    assert!(state.capture_complete);
    assert_eq!(state.captured_bytes, expected);
    assert_eq!(state.confirmed_bytes, expected);
    assert_eq!(service.sent.load(Ordering::Acquire), expected);
    assert!(state.chunks.iter().all(|chunk| chunk.text.is_some()));
    let starts = service.starts.lock().unwrap().clone();
    if realtime && seconds > 50 * 60 {
        assert!(
            starts.len() >= 2,
            "Real session-rotation timer did not run."
        );
    }
    let result = json!({
        "audio_seconds": seconds, "actual_capture_seconds": elapsed,
        "captured_bytes": expected, "verified_provider_bytes": expected,
        "confirmed_bytes": state.confirmed_bytes, "stream_starts": starts,
        "checkpoint_count": state.chunks.len(),
        "maximum_producer_lateness_ms": maximum_lateness.as_secs_f64() * 1000.0,
    });
    drop(state);
    drop(progress);
    session.shutdown().await.unwrap();
    store.delete_idle(&id).unwrap();
    assert!(store.list().unwrap().is_empty());
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Explicit owned real-time synthetic hour collector; never opens physical ports."]
async fn realtime_synthetic_hour() {
    let root = std::env::var_os("VOICEPROMPT_SYNTHETIC_HOUR_DIRECTORY")
        .map(std::path::PathBuf::from)
        .expect("An explicit marked synthetic control directory is required.");
    crate::fixtures::validate(&root, None).unwrap();
    let expected = ["capture-options.json", "migration-fixture.json"];
    let mut entries: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries, expected,
        "Only fresh explicit synthetic inputs are allowed."
    );
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let installed = std::path::PathBuf::from(local).join("VoicePrompt");
        let selected = std::fs::canonicalize(&root)
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        let installed = installed.to_string_lossy().to_lowercase();
        assert!(selected != installed && !selected.starts_with(&format!("{installed}\\")));
    }
    let options_path = root.join("capture-options.json");
    guard_recovery_path(&options_path).unwrap();
    assert!(std::fs::metadata(&options_path).unwrap().len() <= 1024);
    let options: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&options_path).unwrap()).unwrap();
    assert_eq!(options.as_object().unwrap().len(), 3);
    assert_eq!(options["scope"], SCOPE);
    let seconds = options["audio_seconds"].as_u64().unwrap();
    assert!((2..=3600).contains(&seconds));
    let nonce = options["nonce"].as_str().unwrap();
    assert_eq!(nonce.len(), 32);
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("migration-fixture.json")).unwrap())
            .unwrap();
    assert_eq!(marker["instance_key"], nonce);
    let store = NativeRecoveryStore::new(root.join("recovery"), DEFAULT_RECOVERY_QUOTA).unwrap();
    capture(&store, 2, false).await;
    publish(
        &root,
        "ready.json",
        json!({
            "scope": SCOPE, "pid": std::process::id(), "physical_ports": false,
            "network_or_auth": false, "full_client_profile": false,
        }),
    );
    control(&root, "begin", nonce).await;
    let result = capture(&store, seconds, true).await;
    publish(&root, "idle-ready.json", result);
    control(&root, "finish", nonce).await;
}

use super::*;
use crate::recovery_store::tests::TemporaryRoot;
use crate::{DEFAULT_RECOVERY_QUOTA, NativeRecoveryStore};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use voiceprompt_core::wav::to_wav;

struct Backend {
    calls: AtomicUsize,
    active: AtomicUsize,
    maximum: AtomicUsize,
    started: Notify,
    gates: [Notify; 4],
    blocked: bool,
    forbidden: AtomicBool,
}

impl Backend {
    fn new(blocked: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            started: Notify::new(),
            gates: std::array::from_fn(|_| Notify::new()),
            blocked,
            forbidden: AtomicBool::new(false),
        })
    }

    async fn calls(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.calls.load(Ordering::Acquire) < count {
                self.started.notified().await;
            }
        })
        .await
        .unwrap();
    }
}

struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl BatchBackend for Backend {
    async fn transcribe(&self, wav: &[u8], _: &str, _: &str) -> Result<String, ApiError> {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        let _active = Active(&self.active);
        self.maximum.fetch_max(active, Ordering::AcqRel);
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.started.notify_one();
        if self.forbidden.load(Ordering::Acquire) {
            return Err(ApiError {
                kind: ApiErrorKind::Forbidden,
                status: Some(403),
                problem: None,
                response_invalid: false,
            });
        }
        let number = wav[44];
        if self.blocked {
            self.gates[usize::from(number - 1)].notified().await;
        }
        Ok(format!("word{number}"))
    }
}

fn saved(directory: &TemporaryRoot, count: u8) -> (NativeRecoveryStore, NativeRecoveryJournal) {
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create(
            "auto".into(),
            "synthetic context".into(),
            false,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    let wavs: Vec<_> = (1..=count)
        .map(|value| to_wav(&[value; 640]).unwrap())
        .collect();
    let audio: Vec<_> = wavs
        .iter()
        .enumerate()
        .map(|(index, wav)| AudioCheckpoint {
            wav,
            overlaps_previous: false,
            end_byte: (index as u64 + 1) * 640,
        })
        .collect();
    journal
        .checkpoint(
            &[],
            u64::from(count) * 640,
            &audio,
            true,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    (store, journal)
}

async fn wait_progress(
    progress: &mut watch::Receiver<DictationProgress>,
    ready: impl Fn(&DictationProgress) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if ready(&progress.borrow_and_update()) {
                return;
            }
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn out_of_order_results_remain_contiguous_and_never_exceed_two_requests() {
    let directory = TemporaryRoot::new();
    let (_store, journal) = saved(&directory, 4);
    let backend = Backend::new(true);
    let session = LegacySession::start(journal, backend.clone()).unwrap();
    backend.calls(2).await;
    assert_eq!(backend.maximum.load(Ordering::Acquire), 2);
    backend.gates[1].notify_one();
    let mut status = session.progress();
    wait_progress(&mut status, |progress| progress.pending_chunks == 3).await;
    assert_eq!(status.borrow().transcript, "");
    assert!(
        session
            .wait_for_completion(Duration::from_millis(20), false)
            .await
            .unwrap()
            .is_none()
    );
    backend.gates[0].notify_one();
    backend.gates[2].notify_one();
    backend.gates[3].notify_one();
    assert_eq!(
        session
            .wait_for_completion(Duration::from_secs(5), false)
            .await
            .unwrap()
            .as_deref(),
        Some("word1 word2 word3 word4")
    );
    assert_eq!(backend.maximum.load(Ordering::Acquire), 2);
    session.shutdown().await.unwrap();
    assert_eq!(backend.active.load(Ordering::Acquire), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_joins_an_already_started_journal_write_before_releasing_ownership() {
    let directory = TemporaryRoot::new();
    let (store, journal) = saved(&directory, 1);
    let id = journal.id().to_owned();
    let backend = Backend::new(true);
    let session = LegacySession::start(journal, backend.clone()).unwrap();
    backend.calls(1).await;
    let (release, blocked) = store.pause_next_fixture_write();
    backend.gates[0].notify_one();
    tokio::time::timeout(Duration::from_secs(5), blocked)
        .await
        .unwrap()
        .unwrap();
    let shutdown = tokio::spawn(session.shutdown());
    tokio::time::sleep(Duration::from_millis(10)).await;
    let prematurely_finished = shutdown.is_finished();
    drop(release);
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        !prematurely_finished,
        "Shutdown detached an active durable write."
    );
    let reopened = store.open(&id).unwrap();
    let manifest = reopened.snapshot().unwrap();
    assert_eq!(manifest.context, "synthetic context");
    assert_eq!(manifest.captured_bytes, 640);
    assert_eq!(manifest.chunks[0].text.as_deref(), Some("word1"));
}

#[tokio::test]
async fn stop_timeout_and_shutdown_keep_pending_audio_for_restart_recovery() {
    let directory = TemporaryRoot::new();
    let (store, journal) = saved(&directory, 2);
    let id = journal.id().to_owned();
    let backend = Backend::new(true);
    let session = LegacySession::start(journal, backend.clone()).unwrap();
    backend.calls(2).await;
    assert!(
        session
            .wait_for_completion(Duration::from_millis(20), false)
            .await
            .unwrap()
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(1), session.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(backend.active.load(Ordering::Acquire), 0);
    let journal = store.open(&id).unwrap();
    assert!(
        journal
            .snapshot()
            .unwrap()
            .chunks
            .iter()
            .all(|chunk| chunk.text.is_none())
    );
    assert_eq!(journal.read_audio(0).unwrap(), to_wav(&[1; 640]).unwrap());
    assert_eq!(journal.read_audio(1).unwrap(), to_wav(&[2; 640]).unwrap());
}

#[tokio::test]
async fn live_fragmented_capture_checkpoints_and_finishes_without_partial_samples() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create(
            "auto".into(),
            "synthetic context".into(),
            false,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    let backend = Backend::new(false);
    let session = LegacySession::start(journal, backend).unwrap();
    let pcm: Vec<_> = (0..16000).flat_map(|_| 1000i16.to_le_bytes()).collect();
    session.append(&pcm[..17]).unwrap();
    session.append(&pcm[17..]).unwrap();
    session.finish_capture().unwrap();
    assert!(
        session
            .wait_for_completion(Duration::from_secs(5), false)
            .await
            .unwrap()
            .is_some()
    );
    let state = session.snapshot().unwrap();
    assert_eq!(state.captured_bytes, pcm.len() as u64);
    assert!(state.capture_complete);
    assert_eq!(state.chunks.len(), 1);
    assert_eq!(state.chunks[0].end_byte, pcm.len() as u64);
    assert!(state.tail.is_empty());
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminal_auth_failure_retains_backlog_and_is_visible_in_progress() {
    let directory = TemporaryRoot::new();
    let (_store, journal) = saved(&directory, 2);
    let backend = Backend::new(false);
    backend.forbidden.store(true, Ordering::Release);
    let session = LegacySession::start(journal, backend.clone()).unwrap();
    let mut status = session.progress();
    wait_progress(&mut status, |progress| {
        progress.service_error == Some(ApiErrorKind::Forbidden)
    })
    .await;
    assert!(
        session
            .wait_for_completion(Duration::from_millis(20), false)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        session
            .snapshot()
            .unwrap()
            .chunks
            .iter()
            .all(|chunk| chunk.text.is_none())
    );
    assert!(backend.calls.load(Ordering::Acquire) <= 2);
    session.shutdown().await.unwrap();
}

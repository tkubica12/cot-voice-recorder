use super::*;
use crate::recovery_store::tests::TemporaryRoot;
use crate::{DEFAULT_RECOVERY_QUOTA, NativeRecoveryStore};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use voiceprompt_core::wav::to_wav;

struct Backend {
    calls: AtomicUsize,
    active: AtomicUsize,
    maximum: AtomicUsize,
    waiting: AtomicUsize,
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
            waiting: AtomicUsize::new(0),
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
            self.waiting.fetch_add(1, Ordering::AcqRel);
            let _waiting = Active(&self.waiting);
            self.started.notify_one();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn own_legacy_checkpoint_keeps_one_capture_worker_moving_until_the_exact_window_and_original_queue_fill()
 {
    use crate::microphone::{BUFFER_BYTES, CaptureBuffer, OUTPUT_BUFFER_COUNT};
    use std::sync::atomic::AtomicU64;
    use tokio::sync::mpsc;
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
    backend.forbidden.store(true, Ordering::Release);
    let session = Arc::new(LegacySession::start(journal, backend).unwrap());
    let samples: Vec<_> = (0..96_000)
        .flat_map(|index| (1_200i16 + (index % 97) as i16).to_le_bytes())
        .collect();
    let copied = Arc::new(AtomicU64::new(0));
    let completed = copied.clone();
    let writer = session.clone();
    let (queue, receiver) = mpsc::channel(OUTPUT_BUFFER_COUNT);
    let (stopped, _) = watch::channel(false);
    let capture = tokio::task::spawn_blocking(move || {
        let discard = AtomicBool::new(false);
        crate::dictation::persist_capture_with_room(
            receiver,
            &discard,
            &stopped,
            || writer.reserve_capture(&discard),
            |packet, slot| {
                slot.unwrap().append(packet)?;
                completed.fetch_add(packet.len() as u64, Ordering::Release);
                Ok(())
            },
        )
    });
    let (release, entered) = store.pause_next_fixture_write();
    for packet in samples[..64_000].chunks_exact(BUFFER_BYTES) {
        assert!(queue.send(CaptureBuffer(packet.to_vec())).await.is_ok());
    }
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    for packet in samples[64_000..128_000].chunks_exact(BUFFER_BYTES) {
        assert!(queue.send(CaptureBuffer(packet.to_vec())).await.is_ok());
    }
    let progressed = tokio::time::timeout(Duration::from_secs(1), async {
        while copied.load(Ordering::Acquire) != 128_000 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .is_ok();
    let copied_while_paused = copied.load(Ordering::Acquire);
    let saved_while_paused = session.progress().borrow().saved_bytes;
    let charged = {
        let pcm = session.inner.capture.lock().unwrap();
        (
            pcm.sealing_bytes + pcm.buffer.len() + pcm.reserved_bytes,
            pcm.reserved_bytes,
        )
    };
    if progressed {
        for packet in samples[128_000..].chunks_exact(BUFFER_BYTES) {
            assert!(queue.try_send(CaptureBuffer(packet.to_vec())).is_ok());
        }
    }
    let full = matches!(
        queue.try_send(CaptureBuffer(vec![0; BUFFER_BYTES])),
        Err(mpsc::error::TrySendError::Full(_))
    );
    drop(release);
    drop(queue);
    capture.await.unwrap().unwrap();
    let session = Arc::try_unwrap(session).ok().unwrap();
    session.finish_capture().unwrap();
    let state = session.snapshot().unwrap();
    for chunk in &state.chunks {
        let wav = session.inner.journal.read_audio(chunk.index).unwrap();
        let end = chunk.end_byte as usize;
        assert_eq!(&wav[44..], &samples[end - (wav.len() - 44)..end]);
    }
    session.shutdown().await.unwrap();
    assert!(
        progressed,
        "Single capture consumer waited for its OWN legacy checkpoint; completed={copied_while_paused}"
    );
    assert!(full);
    assert_eq!(copied_while_paused, 128_000);
    assert_eq!(charged, (128_000, 0));
    assert_eq!(saved_while_paused, 0);
    assert_eq!(copied.load(Ordering::Acquire), 192_000);
    assert_eq!(state.captured_bytes, 192_000);
    assert!(state.capture_complete);
    assert_eq!(state.context, "synthetic context");
}

fn live(directory: &TemporaryRoot) -> (NativeRecoveryStore, Arc<LegacySession<Backend>>) {
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
    backend.forbidden.store(true, Ordering::Release);
    let session = Arc::new(LegacySession::start(journal, backend).unwrap());
    (store, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn voiced_to_silence_emission_below_raw_checkpoint_threshold_is_durable_and_asr_eligible_without_more_input()
 {
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
    let mut reference = PcmChunker::new(120.0).unwrap();
    let voiced: Vec<_> = (0..25_600).flat_map(|_| 1_200i16.to_le_bytes()).collect();
    assert!(reference.append(&voiced).unwrap().is_empty());
    let tail = legacy_checkpoint::encode(&reference.snapshot()).unwrap();
    journal
        .checkpoint(&tail, 51_200, &[], false, time::OffsetDateTime::now_utc())
        .unwrap();
    let backend = Backend::new(false);
    let session = LegacySession::start(journal, backend.clone()).unwrap();
    session.append(&[0; 12_800]).unwrap();
    let eligible = tokio::time::timeout(Duration::from_secs(5), async {
        while backend.calls.load(Ordering::Acquire) == 0 {
            backend.started.notified().await;
        }
    })
    .await
    .is_ok();
    let saved = session.snapshot().unwrap();
    session.shutdown().await.unwrap();
    assert!(
        eligible,
        "An actual silence-cut emission waited for64000newrawbytes."
    );
    assert_eq!(saved.captured_bytes, 64_000);
    assert_eq!(saved.chunks.len(), 1);
    assert_eq!(saved.chunks[0].end_byte, 64_000);
    assert!(!saved.capture_complete);
    assert!(saved.captured_bytes - 51_200 < crate::capture_window::TRIGGER_BYTES as u64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn odd_legacy_fragment_is_durable_without_more_input_and_pairs_or_fails_eof_explicitly() {
    for pair in [false, true] {
        let directory = TemporaryRoot::new();
        let (store, session) = live(&directory);
        let id = session.id().to_owned();
        let mut bytes: Vec<_> = (0..32_000).flat_map(|_| 1_200i16.to_le_bytes()).collect();
        bytes.push(0x42);
        session.append(&bytes).unwrap();
        let mut progress = session.progress();
        let saved_without_more_input = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if progress.borrow_and_update().saved_bytes == 64_001 {
                    break;
                }
                progress.changed().await.unwrap();
            }
        })
        .await
        .is_ok();
        if !saved_without_more_input {
            Arc::try_unwrap(session)
                .ok()
                .unwrap()
                .shutdown()
                .await
                .unwrap();
            panic!(
                "Legacy64001-byte append waited for another input instead of publishing its valid tail."
            );
        }
        let published = session.snapshot().unwrap();
        assert_eq!(published.captured_bytes, 64_001);
        assert!(!published.capture_complete);
        let tail = legacy_checkpoint::parse(&published.tail).unwrap();
        assert_eq!(
            (tail.processed_bytes, tail.frame.as_slice()),
            (64_000, &[0x42][..])
        );
        if pair {
            bytes.push(0);
            session.append(&[0]).unwrap();
            session.finish_capture().unwrap();
            let complete = session.snapshot().unwrap();
            assert!(complete.capture_complete && complete.tail.is_empty());
            assert_eq!(complete.captured_bytes, 64_002);
            assert_eq!(&session.inner.journal.read_audio(0).unwrap()[44..], bytes);
        } else {
            assert!(matches!(
                session.finish_capture(),
                Err(SessionError::Audio(ChunkerError::IncompleteSample))
            ));
            assert!(session.progress().borrow().storage_failed);
            assert_eq!(session.snapshot().unwrap(), published);
        }
        Arc::try_unwrap(session)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        let reopened = store.open(&id).unwrap().snapshot().unwrap();
        if pair {
            assert_eq!(reopened.captured_bytes, 64_002);
            assert!(reopened.capture_complete && reopened.tail.is_empty());
        } else {
            assert_eq!(reopened, published);
            assert_eq!(
                legacy_checkpoint::parse(&reopened.tail).unwrap().frame,
                [0x42]
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restored_maximum_four_chunks_publish_with_one_raw_batch_and_with_incremental_drains() {
    for fragmented in [false, true] {
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
        let id = journal.id().to_owned();
        let prefix: Vec<_> = (0..95_680).flat_map(|_| 1_200i16.to_le_bytes()).collect();
        let raw: Vec<_> = (1..=200)
            .flat_map(|frame| {
                let sample = if (52..=71).contains(&frame) || (152..=171).contains(&frame) {
                    0i16
                } else {
                    1_200
                };
                std::iter::repeat_n(sample, 320).flat_map(i16::to_le_bytes)
            })
            .collect();
        assert_eq!((prefix.len(), raw.len()), (191_360, 128_000));
        let mut reference = PcmChunker::new(120.0).unwrap();
        assert!(reference.append(&prefix).unwrap().is_empty());
        let checkpoint = legacy_checkpoint::encode(&reference.snapshot()).unwrap();
        assert_eq!(
            legacy_checkpoint::parse(&checkpoint).unwrap(),
            reference.snapshot()
        );
        journal
            .checkpoint(
                &checkpoint,
                191_360,
                &[],
                false,
                time::OffsetDateTime::now_utc(),
            )
            .unwrap();
        let mut expected = reference.append(&raw).unwrap();
        expected.extend(reference.flush().unwrap());
        assert_eq!(
            expected
                .iter()
                .map(|chunk| chunk.end_byte)
                .collect::<Vec<_>>(),
            [192_000, 236_800, 300_800, 319_360]
        );
        let backend = Backend::new(false);
        backend.forbidden.store(true, Ordering::Release);
        let session = Arc::new(LegacySession::start(journal, backend).unwrap());
        if fragmented {
            for (index, frame) in raw.chunks_exact(640).enumerate() {
                session.append(frame).unwrap();
                let expected_end = 191_360 + (index + 1) as u64 * 640;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while session.inner.capture.lock().unwrap().buffer_start != expected_end {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            assert_eq!(session.snapshot().unwrap().captured_bytes, 300_800);
            assert_eq!(session.inner.capture.lock().unwrap().sealing_bytes, 18_560);
            session.finish_capture().unwrap();
        } else {
            let (release, entered) = store.pause_next_fixture_write();
            let durable = session.inner.lock().unwrap();
            session.append(&raw).unwrap();
            session.inner.capture.finish_requested();
            drop(durable);
            let finalizing = session.clone();
            let finish = tokio::task::spawn_blocking(move || finalizing.finish_capture());
            tokio::time::timeout(Duration::from_secs(5), entered)
                .await
                .unwrap()
                .unwrap();
            let charge = {
                let window = session.inner.capture.lock().unwrap();
                (
                    window.sealing_bytes,
                    window.buffer.len(),
                    window.reserved_bytes,
                )
            };
            let saved_before_release = session.progress().borrow().saved_bytes;
            drop(release);
            finish.await.unwrap().unwrap();
            assert_eq!(charge, (128_000, 0, 0));
            assert_eq!(saved_before_release, 191_360);
        }
        let saved = session.snapshot().unwrap();
        assert!(saved.capture_complete && saved.tail.is_empty());
        assert_eq!(saved.captured_bytes, 319_360);
        assert_eq!(saved.context, "synthetic context");
        assert_eq!(saved.chunks.len(), 4);
        for (chunk, expected) in saved.chunks.iter().zip(&expected) {
            assert_eq!(chunk.end_byte, expected.end_byte);
            assert_eq!(chunk.overlaps_previous, expected.overlaps_previous);
            assert_eq!(
                session.inner.journal.read_audio(chunk.index).unwrap(),
                expected.wav
            );
        }
        assert_eq!(session.inner.capture.lock().unwrap().sealing_bytes, 0);
        Arc::try_unwrap(session)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        let reopened = store.open(&id).unwrap();
        assert_eq!(reopened.snapshot().unwrap(), saved);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn voiced_quiet_overlap_and_partial_frame_restore_match_the_unchanged_chunker() {
    for kind in 0..3 {
        let directory = TemporaryRoot::new();
        let (store, session) = live(&directory);
        let id = session.id().to_owned();
        let bytes: Vec<_> = (0..130_003)
            .flat_map(|index| {
                let quiet = kind == 1 || kind == 2 && index / 16_000 % 5 == 0;
                (if quiet {
                    0i16
                } else {
                    1_200 + (index % 97) as i16
                })
                .to_le_bytes()
            })
            .collect();
        let prefix = 64_002;
        let mut reference = PcmChunker::new(120.0).unwrap();
        let mut expected = reference.append(&bytes[..prefix]).unwrap();
        session.append(&bytes[..17]).unwrap();
        session.append(&bytes[17..prefix]).unwrap();
        let mut progress = session.progress();
        wait_progress(&mut progress, |status| status.saved_bytes == prefix as u64).await;
        let published = session.snapshot().unwrap();
        assert_eq!(
            legacy_checkpoint::parse(&published.tail).unwrap(),
            reference.snapshot()
        );
        assert_eq!(reference.snapshot().frame.len(), 2);
        Arc::try_unwrap(session)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();

        let backend = Backend::new(false);
        backend.forbidden.store(true, Ordering::Release);
        let resumed = Arc::new(LegacySession::start(store.open(&id).unwrap(), backend).unwrap());
        expected.extend(reference.append(&bytes[prefix..]).unwrap());
        expected.extend(reference.flush().unwrap());
        let writer = resumed.clone();
        let tail = bytes[prefix..].to_vec();
        tokio::task::spawn_blocking(move || writer.append(&tail))
            .await
            .unwrap()
            .unwrap();
        resumed.finish_capture().unwrap();
        let saved = resumed.snapshot().unwrap();
        assert_eq!(saved.captured_bytes, bytes.len() as u64);
        assert!(saved.capture_complete && saved.tail.is_empty());
        assert_eq!(saved.context, "synthetic context");
        assert_eq!(saved.chunks.len(), expected.len());
        for (chunk, expected) in saved.chunks.iter().zip(&expected) {
            assert_eq!(chunk.end_byte, expected.end_byte);
            assert_eq!(chunk.overlaps_previous, expected.overlaps_previous);
            assert_eq!(
                resumed.inner.journal.read_audio(chunk.index).unwrap(),
                expected.wav
            );
        }
        assert!(saved.chunks.iter().all(|chunk| chunk.audio_bytes as usize
            <= voiceprompt_client_protocol::recovery::MAX_AUDIO_BYTES));
        Arc::try_unwrap(resumed)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_owned_legacy_write_retains_raw_tail_and_old_cipher_and_wakes_room_waiters() {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = TemporaryRoot::new();
    let (store, session) = live(&directory);
    let id = session.id().to_owned();
    let path = directory.0.join(&id).join("manifest.enc");
    let before = std::fs::read(&path).unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
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
    drop(release);
    let result = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(blocked && matches!(result, Err(SessionError::Storage(_))));
    {
        let state = session.inner.lock().unwrap();
        assert_eq!(state.manifest.captured_bytes, 0);
        assert_eq!(state.captured, 64_000);
        assert_eq!(state.sealing.as_ref().unwrap().pcm.0, vec![1; 64_000]);
        assert_eq!(
            legacy_checkpoint::parse(&state.writing_tail.0)
                .unwrap()
                .audio,
            vec![1; 64_000]
        );
        assert!(state.storage_failed);
    }
    {
        let raw = session.inner.capture.lock().unwrap();
        assert_eq!((raw.sealing_bytes, raw.reserved_bytes), (64_000, 0));
        assert_eq!(raw.buffer, vec![2; 64_000]);
    }
    let progress = session.progress().borrow().clone();
    assert!(progress.storage_failed && !progress.capture_complete);
    assert_eq!(
        (progress.captured_bytes, progress.saved_bytes),
        (128_000, 0)
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(matches!(
        session.append(&[3; 1_280]),
        Err(SessionError::Storage(_))
    ));
    assert!(matches!(
        session.finish_capture(),
        Err(SessionError::Storage(_))
    ));
    let failure = Arc::try_unwrap(session)
        .ok()
        .unwrap()
        .shutdown()
        .await
        .unwrap_err();
    assert!(failure.to_string().contains("MoveFileExW"));
    drop(held);
    assert_eq!(store.fixture_snapshot(&id).unwrap().captured_bytes, 0);
    store.delete_idle(&id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_wakes_legacy_room_but_final_drain_and_all_started_io_join_before_idle_delete() {
    let directory = TemporaryRoot::new();
    let (store, session) = live(&directory);
    let id = session.id().to_owned();
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
    session.inner.capture.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap();
    assert!(blocked && matches!(result, Err(SessionError::Closed)));
    let session = Arc::try_unwrap(session).ok().unwrap();
    let finish = tokio::task::spawn_blocking(move || {
        session.finish_capture().unwrap();
        session
    });
    let mut shutdown = tokio::spawn(async move { finish.await.unwrap().shutdown().await });
    let early = tokio::time::timeout(Duration::from_millis(100), &mut shutdown).await;
    let detached = early.is_ok();
    drop(release);
    match early {
        Ok(result) => result.unwrap().unwrap(),
        Err(_) => shutdown.await.unwrap().unwrap(),
    }
    assert!(!detached);
    let saved = store.fixture_snapshot(&id).unwrap();
    assert!(saved.capture_complete);
    assert_eq!(saved.captured_bytes, 128_000);
    let reader = store.open(&id).unwrap();
    assert_eq!(
        &reader.read_audio(0).unwrap()[44..],
        [vec![1; 64_000], vec![2; 64_000]].concat()
    );
    drop(reader);
    store.delete_idle(&id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn started_asr_result_write_does_not_block_legacy_packet_admission_or_lose_the_new_final_chunk()
 {
    use crate::microphone::{BUFFER_BYTES, CaptureBuffer, OUTPUT_BUFFER_COUNT};
    use tokio::sync::mpsc;
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
    let id = journal.id().to_owned();
    let prefix = vec![1; 192_000];
    let mut reference = PcmChunker::new(120.0).unwrap();
    let chunks = reference.append(&prefix).unwrap();
    let tail = legacy_checkpoint::encode(&reference.snapshot()).unwrap();
    let views: Vec<_> = chunks
        .iter()
        .map(|chunk| AudioCheckpoint {
            wav: &chunk.wav,
            end_byte: chunk.end_byte,
            overlaps_previous: chunk.overlaps_previous,
        })
        .collect();
    journal
        .checkpoint(
            &tail,
            192_000,
            &views,
            false,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    let backend = Backend::new(true);
    let session = Arc::new(LegacySession::start(journal, backend.clone()).unwrap());
    backend.calls(1).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while backend.waiting.load(Ordering::Acquire) != 1 {
            backend.started.notified().await;
        }
    })
    .await
    .unwrap();
    let (release, entered) = store.pause_next_fixture_write();
    backend.forbidden.store(true, Ordering::Release);
    backend.gates[0].notify_one();
    tokio::time::timeout(Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    let (queue, receiver) = mpsc::channel(OUTPUT_BUFFER_COUNT);
    let (stopped, _) = watch::channel(false);
    let writer = session.clone();
    let capture = tokio::task::spawn_blocking(move || {
        let discard = AtomicBool::new(false);
        crate::dictation::persist_capture_with_room(
            receiver,
            &discard,
            &stopped,
            || writer.reserve_capture(&discard),
            |pcm, slot| slot.unwrap().append(pcm),
        )
    });
    for _ in 0..30 {
        assert!(
            queue
                .send(CaptureBuffer(vec![2; BUFFER_BYTES]))
                .await
                .is_ok()
        );
    }
    drop(queue);
    let mut capture = capture;
    let appended = tokio::time::timeout(Duration::from_secs(1), &mut capture).await;
    let moved_while_result_io_paused = appended.is_ok();
    if let Ok(result) = appended {
        result.unwrap().unwrap();
    }
    let finalizing = session.clone();
    let mut finish = tokio::task::spawn_blocking(move || finalizing.finish_capture());
    let premature = tokio::time::timeout(Duration::from_millis(100), &mut finish).await;
    let detached = premature.is_ok();
    drop(release);
    if !moved_while_result_io_paused {
        capture.await.unwrap().unwrap();
    }
    match premature {
        Ok(result) => result.unwrap().unwrap(),
        Err(_) => finish.await.unwrap().unwrap(),
    }
    let saved = session.snapshot().unwrap();
    assert!(moved_while_result_io_paused && !detached);
    assert!(saved.capture_complete);
    assert_eq!(saved.captured_bytes, 230_400);
    assert_eq!(saved.chunks[0].text.as_deref(), Some("word1"));
    let second = session.inner.journal.read_audio(1).unwrap();
    assert_eq!(&second[44..], [vec![1; 19_200], vec![2; 38_400]].concat());
    Arc::try_unwrap(session)
        .ok()
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    assert_eq!(backend.active.load(Ordering::Acquire), 0);
    assert!(store.open(&id).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asr_publication_preserves_below_threshold_working_raw_without_publishing_or_refeeding_it()
{
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
    let id = journal.id().to_owned();
    let prefix = vec![1; 192_000];
    let new_raw = vec![2; 38_400];
    let mut reference = PcmChunker::new(120.0).unwrap();
    let first = reference.append(&prefix).unwrap();
    let durable_tail = reference.snapshot();
    let tail = legacy_checkpoint::encode(&durable_tail).unwrap();
    let views: Vec<_> = first
        .iter()
        .map(|chunk| AudioCheckpoint {
            wav: &chunk.wav,
            end_byte: chunk.end_byte,
            overlaps_previous: chunk.overlaps_previous,
        })
        .collect();
    journal
        .checkpoint(
            &tail,
            192_000,
            &views,
            false,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    let backend = Backend::new(true);
    let session = LegacySession::start(journal, backend.clone()).unwrap();
    backend.calls(1).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while backend.waiting.load(Ordering::Acquire) != 1 {
            backend.started.notified().await;
        }
    })
    .await
    .unwrap();
    assert!(reference.append(&new_raw).unwrap().is_empty());
    session.append(&new_raw).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while session.inner.capture.lock().unwrap().buffer_start != 230_400 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let snapshot = session.snapshot().unwrap();
    assert_eq!(snapshot.captured_bytes, 192_000);
    assert_eq!(
        legacy_checkpoint::parse(&snapshot.tail).unwrap(),
        durable_tail
    );
    backend.forbidden.store(true, Ordering::Release);
    backend.gates[0].notify_one();
    let mut progress = session.progress();
    wait_progress(&mut progress, |status| status.transcript == "word1").await;
    {
        let state = session.inner.lock().unwrap();
        assert_eq!(state.captured, 230_400);
        assert_eq!(state.manifest.captured_bytes, 192_000);
        assert_eq!(state.chunker.snapshot(), reference.snapshot());
        assert_eq!(state.sealing.as_ref().unwrap().pcm.0, new_raw);
    }
    let snapshot = session.snapshot().unwrap();
    assert_eq!(snapshot.captured_bytes, 192_000);
    assert_eq!(snapshot.chunks[0].text.as_deref(), Some("word1"));
    assert_eq!(
        legacy_checkpoint::parse(&snapshot.tail).unwrap(),
        durable_tail
    );
    {
        let raw = session.inner.capture.lock().unwrap();
        assert_eq!((raw.buffer_start, raw.sealing_bytes), (230_400, 38_400));
    }
    let silence = vec![0; 12_800];
    let emitted = reference.append(&silence).unwrap();
    assert_eq!(emitted.len(), 1);
    session.append(&silence).unwrap();
    wait_progress(&mut progress, |status| status.saved_bytes == 243_200).await;
    session.finish_capture().unwrap();
    let final_state = session.snapshot().unwrap();
    assert!(final_state.capture_complete && final_state.tail.is_empty());
    assert_eq!(final_state.captured_bytes, 243_200);
    assert_eq!(final_state.chunks.len(), 2);
    assert_eq!(session.inner.journal.read_audio(1).unwrap(), emitted[0].wav);
    assert_eq!(final_state.context, "synthetic context");
    session.shutdown().await.unwrap();
    assert_eq!(store.open(&id).unwrap().snapshot().unwrap(), final_state);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_joined_worker_panic_poison_keeps_raw_ownership_and_never_admits_more_audio() {
    let directory = TemporaryRoot::new();
    let (store, session) = live(&directory);
    let id = session.id().to_owned();
    session.append(&[1; 1_280]).unwrap();
    let owner = session.inner.clone();
    let job = tokio::task::spawn_blocking(move || {
        let mut state = owner.lock().unwrap();
        state.sealing = Some(owner.capture.detach(state.captured, false).unwrap());
        panic!("Controlled joined legacy seal worker panic.");
    });
    assert!(matches!(
        join_seal(&session.inner, job).await,
        Err(SessionError::Worker)
    ));
    assert!(session.progress().borrow().storage_failed);
    assert_eq!(session.progress().borrow().saved_bytes, 0);
    {
        let state = match session.inner.state.lock() {
            Ok(_) => panic!("The controlled writer did not poison its durable mutex."),
            Err(poisoned) => poisoned.into_inner(),
        };
        assert_eq!(state.sealing.as_ref().unwrap().pcm.0, vec![1; 1_280]);
        assert_eq!(state.manifest.captured_bytes, 0);
    }
    assert!(matches!(
        session.reserve_capture(&AtomicBool::new(false)),
        Err(SessionError::Storage(_))
    ));
    assert!(matches!(
        session.finish_capture(),
        Err(SessionError::Storage(_))
    ));
    let result = Arc::try_unwrap(session).ok().unwrap().shutdown().await;
    assert!(matches!(result, Ok(()) | Err(SessionError::Storage(_))));
    assert_eq!(store.fixture_snapshot(&id).unwrap().captured_bytes, 0);
    store.delete_idle(&id).unwrap();
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
async fn async_readers_do_not_bypass_a_pending_synchronous_capture_writer() {
    let directory = TemporaryRoot::new();
    let (_store, journal) = saved(&directory, 0);
    let session = LegacySession::start(journal, Backend::new(false)).unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pinned_legacy_reader_does_not_hold_capture_state_or_root_during_file_io() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create(
            "cs".into(),
            "synthetic context".into(),
            false,
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
    let id = journal.id().to_owned();
    let (release, blocked) = store.pause_next_pinned_read();
    let session = LegacySession::start(journal, Backend::new(false)).unwrap();
    session.append(&[1; 192000]).unwrap();
    tokio::time::timeout(Duration::from_secs(5), blocked)
        .await
        .unwrap()
        .unwrap();
    let mut writer = tokio::task::spawn_blocking(move || {
        let result = session.append(&[2; 64000]);
        (session, result)
    });
    let progressed = tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
    let (session, result) = match progressed {
        Ok(result) => result.unwrap(),
        Err(_) => {
            drop(release);
            let _ = writer.await;
            panic!("Pinned read blocked actual legacy capture append.");
        }
    };
    assert!(result.is_ok());
    let mut progress = session.progress();
    wait_progress(&mut progress, |status| status.saved_bytes == 256000).await;
    let saved = store.fixture_snapshot(&id).unwrap();
    assert_eq!(saved.captured_bytes, 256000);
    let shutdown = tokio::spawn(session.shutdown());
    tokio::time::sleep(Duration::from_millis(10)).await;
    let detached = shutdown.is_finished();
    drop(release);
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!detached, "Shutdown must join an active pinned file read.");
    assert!(store.open(&id).is_ok());
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

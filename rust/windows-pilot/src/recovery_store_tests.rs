use super::*;
use voiceprompt_core::wav::to_wav;

pub(crate) struct TemporaryRoot(pub(crate) PathBuf);

impl TemporaryRoot {
    pub(crate) fn new() -> Self {
        let seed = RecoveryManifest::create(
            "auto".into(),
            String::new(),
            false,
            utc_ticks(now()).unwrap(),
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!("VoicePrompt-native-recovery-{}", seed.id));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TemporaryRoot {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("Owned recovery fixture cleanup failed: {error}");
        }
    }
}

fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap()
}

fn audio(wav: &[u8], end_byte: u64) -> AudioCheckpoint<'_> {
    AudioCheckpoint {
        wav,
        end_byte,
        overlaps_previous: false,
    }
}

#[test]
fn root_writer_is_os_locked_and_shared_stores_cannot_change_quota_or_steal_a_journal() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let second = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    assert!(Arc::ptr_eq(&store.0, &second.0));
    assert!(NativeRecoveryStore::new(directory.0.clone(), 1024).is_err());
    assert!(
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(directory.0.join("writer.lock"))
            .is_err()
    );
    let journal = store
        .create("auto".into(), String::new(), true, now())
        .unwrap();
    let id = journal.id().to_owned();
    assert!(second.open(&id).is_err());
    assert!(second.delete_idle(&id).is_err());
    drop(journal);
    assert!(second.open(&id).is_ok());
    drop(second);
    drop(store);
    assert!(
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(directory.0.join("writer.lock"))
            .is_ok()
    );
}

#[test]
fn persisted_checkpoint_reopens_exact_audio_and_confirmed_bytes_survive_restart() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("auto".into(), "synthetic context".into(), true, now())
        .unwrap();
    let first = to_wav(&[1; 6400]).unwrap();
    let second = to_wav(&[2; 6400]).unwrap();
    journal
        .checkpoint(
            &[],
            12800,
            &[audio(&first, 6400), audio(&second, 12800)],
            true,
            now(),
        )
        .unwrap();
    assert_eq!(journal.read_audio(0).unwrap(), first);
    journal
        .confirm_streaming(
            6400,
            "synthetic first".into(),
            vec![1; 100],
            false,
            None,
            now(),
        )
        .unwrap();
    let path = store.0.session_path(journal.id()).unwrap();
    assert!(!path.join("00000000.wav.enc").exists());
    assert!(path.join("00000001.wav.enc").exists());
    let id = journal.id().to_owned();
    drop(journal);
    drop(store);
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store.open(&id).unwrap();
    let state = journal.snapshot().unwrap();
    assert_eq!(state.confirmed_bytes, 6400);
    assert_eq!(state.confirmed_text, "synthetic first");
    assert_eq!(state.tail, [1; 100]);
    assert_eq!(journal.read_audio(1).unwrap(), second);
    assert!(journal.read_audio(0).is_err());
    journal
        .confirm_streaming(
            12800,
            "synthetic first final".into(),
            Vec::new(),
            true,
            None,
            now(),
        )
        .unwrap();
    journal
        .prepare_audit("synthetic polished".into(), now(), true, now())
        .unwrap();
    let state = journal.snapshot().unwrap();
    assert!(state.audit.is_some());
    assert!(state.used_streaming && state.used_fallback);
    assert!(!path.join("00000001.wav.enc").exists());
}

#[test]
fn failed_manifest_publication_retains_old_state_and_charges_orphan_audio() {
    let directory = TemporaryRoot::new();
    let quota = 30000u64;
    let store = NativeRecoveryStore::new(directory.0.clone(), quota).unwrap();
    let journal = store
        .create("auto".into(), String::new(), true, now())
        .unwrap();
    let wav = to_wav(&[1; 6400]).unwrap();
    let path = store
        .0
        .session_path(journal.id())
        .unwrap()
        .join("manifest.enc");
    let before = std::fs::read(&path).unwrap();
    let plan = journal
        .snapshot()
        .unwrap()
        .checkpoint(
            &[],
            6400,
            &[audio(&wav, 6400)],
            false,
            utc_ticks(now()).unwrap(),
        )
        .unwrap();
    let new_manifest_bytes = protect(&plan.encode().unwrap()).unwrap().len();
    let new_audio_bytes = protect(&wav).unwrap().len();
    let filler = quota as usize - before.len() - new_audio_bytes - new_manifest_bytes + 1;
    let orphan = directory.0.join("unpublished.tmp");
    std::fs::write(&orphan, vec![0; filler]).unwrap();
    assert!(
        journal
            .checkpoint(&[], 6400, &[audio(&wav, 6400)], false, now())
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let audio_path = store.0.audio_path(journal.id(), 0).unwrap();
    assert!(audio_path.exists());
    assert!(journal.snapshot().unwrap().chunks.is_empty());
    std::fs::remove_file(orphan).unwrap();
    journal
        .checkpoint(&[], 6400, &[audio(&wav, 6400)], false, now())
        .unwrap();
    assert_eq!(journal.read_audio(0).unwrap(), wav);
}

#[test]
fn failed_confirmation_never_deletes_pending_audio_and_idempotent_retry_is_safe() {
    let directory = TemporaryRoot::new();
    let quota = 30000u64;
    let store = NativeRecoveryStore::new(directory.0.clone(), quota).unwrap();
    let journal = store
        .create("auto".into(), String::new(), false, now())
        .unwrap();
    let wav = to_wav(&[1; 6400]).unwrap();
    journal
        .checkpoint(&[], 6400, &[audio(&wav, 6400)], true, now())
        .unwrap();
    let directory_size: u64 = std::fs::read_dir(store.0.session_path(journal.id()).unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    let filler = directory.0.join("orphan.tmp");
    std::fs::write(&filler, vec![0; (quota - directory_size) as usize]).unwrap();
    assert!(journal.save_result(0, "synthetic".into(), now()).is_err());
    assert_eq!(journal.snapshot().unwrap().chunks[0].text, None);
    assert_eq!(journal.read_audio(0).unwrap(), wav);
    std::fs::remove_file(&filler).unwrap();
    journal.save_result(0, "synthetic".into(), now()).unwrap();
    let after = journal.snapshot().unwrap().encode().unwrap();
    journal.save_result(0, "synthetic".into(), now()).unwrap();
    assert_eq!(journal.snapshot().unwrap().encode().unwrap(), after);
    assert!(journal.save_result(0, "conflict".into(), now()).is_err());
}

#[test]
fn cleanup_retains_active_and_corrupt_journals_and_uses_strict_48_hour_boundary() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let active = store
        .create("auto".into(), String::new(), true, now())
        .unwrap();
    let expired = store
        .create("auto".into(), String::new(), true, now())
        .unwrap();
    let corrupt = store
        .create("auto".into(), String::new(), true, now())
        .unwrap();
    let expired_id = expired.id().to_owned();
    let corrupt_id = corrupt.id().to_owned();
    drop(expired);
    drop(corrupt);
    let corrupt_path = store
        .0
        .session_path(&corrupt_id)
        .unwrap()
        .join("manifest.enc");
    std::fs::write(&corrupt_path, b"corrupt synthetic fixture").unwrap();
    assert_eq!(store.cleanup(now() + time::Duration::hours(48)).unwrap(), 0);
    assert_eq!(
        store
            .cleanup(now() + time::Duration::hours(48) + time::Duration::nanoseconds(100))
            .unwrap(),
        1
    );
    assert!(!store.0.session_path(&expired_id).unwrap().exists());
    assert!(store.0.session_path(active.id()).unwrap().exists());
    assert_eq!(
        std::fs::read(corrupt_path).unwrap(),
        b"corrupt synthetic fixture"
    );
    assert!(
        store
            .list()
            .unwrap()
            .iter()
            .any(|entry| entry.id == corrupt_id && entry.error.is_some())
    );
}

#[test]
fn unexpected_nested_layout_is_rejected_before_any_file_is_deleted() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("auto".into(), String::new(), true, now())
        .unwrap();
    let id = journal.id().to_owned();
    let path = store.0.session_path(&id).unwrap();
    drop(journal);
    let before = std::fs::read(path.join("manifest.enc")).unwrap();
    std::fs::create_dir(path.join("unexpected")).unwrap();
    assert!(store.delete_idle(&id).is_err());
    assert_eq!(std::fs::read(path.join("manifest.enc")).unwrap(), before);
    std::fs::remove_dir(path.join("unexpected")).unwrap();
    store.delete_idle(&id).unwrap();
    assert!(!path.exists());
}

#[test]
fn damaged_audio_is_visible_and_recovery_open_never_silently_discards_it() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("auto".into(), String::new(), true, now())
        .unwrap();
    let wav = to_wav(&[1; 6400]).unwrap();
    journal
        .checkpoint(&[], 6400, &[audio(&wav, 6400)], true, now())
        .unwrap();
    let id = journal.id().to_owned();
    let path = store.0.audio_path(&id, 0).unwrap();
    std::fs::write(&path, b"corrupt synthetic audio").unwrap();
    assert!(journal.read_audio(0).is_err());
    drop(journal);
    assert!(store.open(&id).is_err());
    assert!(store.list().unwrap()[0].error.is_some());
    assert_eq!(std::fs::read(path).unwrap(), b"corrupt synthetic audio");
}

#[test]
fn producer_and_transcription_completion_cannot_overwrite_each_others_manifest_updates() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = Arc::new(
        store
            .create("auto".into(), String::new(), false, now())
            .unwrap(),
    );
    let wav = to_wav(&[1; 6400]).unwrap();
    journal
        .checkpoint(&[], 6400, &[audio(&wav, 6400)], false, now())
        .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let producer = journal.clone();
    let release = barrier.clone();
    let writer = std::thread::spawn(move || {
        release.wait();
        producer
            .checkpoint(&[], 12800, &[audio(&wav, 12800)], true, now())
            .unwrap();
    });
    let completion = journal.clone();
    let result = std::thread::spawn(move || {
        barrier.wait();
        completion
            .save_result(0, "synthetic first".into(), now())
            .unwrap();
    });
    writer.join().unwrap();
    result.join().unwrap();
    let state = journal.snapshot().unwrap();
    assert_eq!(state.chunks.len(), 2);
    assert_eq!(state.chunks[0].text.as_deref(), Some("synthetic first"));
    assert_eq!(state.chunks[1].text, None);
    assert_eq!(state.captured_bytes, 12800);
    assert!(state.capture_complete);
}

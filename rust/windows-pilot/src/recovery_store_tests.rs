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

#[test]
fn storage_error_source_chain_preserves_io_and_hresult_without_private_message_text() {
    use crate::legacy_session::SessionError;
    use std::error::Error;
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    let path = store.0.path.join(id).join("00000003.wav.enc");
    let error = SessionError::Storage(store.0.path_error(
        &path,
        NativeError::Io(
            "read encrypted recovery file",
            io::Error::from_raw_os_error(2),
        ),
    ));
    let native = error
        .source()
        .unwrap()
        .downcast_ref::<NativeError>()
        .unwrap();
    let cause = native
        .source()
        .unwrap()
        .downcast_ref::<NativeError>()
        .unwrap();
    let io = cause.source().unwrap().downcast_ref::<io::Error>().unwrap();
    assert_eq!(io.kind(), io::ErrorKind::NotFound);
    assert_eq!(io.raw_os_error(), Some(2));
    let display = error.to_string();
    assert!(display.contains(&format!("{id}\\00000003.wav.enc")));
    assert!(display.contains("read encrypted recovery file"));
    assert!(display.contains("kind=NotFound, os_code=Some(2)"));
    assert!(!display.contains(directory.0.to_str().unwrap()));
    let sensitive = "synthetic-secret=C:\\private-user\\private-file";
    let private = store.0.path.join("private-user").join("private-file");
    let error = SessionError::Storage(store.0.path_error(
        &private,
        NativeError::Io(
            "synthetic write",
            io::Error::new(io::ErrorKind::PermissionDenied, sensitive),
        ),
    ));
    for value in [error.to_string(), format!("{error:?}")] {
        assert!(value.contains("<other-entry>\\<other-entry>"));
        assert!(value.contains("kind=PermissionDenied"));
        assert!(!value.contains("private-user"));
        assert!(!value.contains("private-file"));
        assert!(!value.contains("synthetic-secret"));
    }
    let api = windows::core::Error::new(windows::core::HRESULT(0x80070005u32 as i32), sensitive);
    let error = SessionError::Storage(
        store
            .0
            .path_error(&path, NativeError::Api("replace atomic file", api)),
    );
    let message = error.to_string();
    assert!(message.contains("replace atomic file"));
    assert!(message.contains("HRESULT=0x80070005"));
    assert!(!message.contains(sensitive));
    let native = error
        .source()
        .unwrap()
        .source()
        .unwrap()
        .downcast_ref::<NativeError>()
        .unwrap();
    assert_eq!(
        native
            .source()
            .unwrap()
            .downcast_ref::<windows::core::Error>()
            .unwrap()
            .code()
            .0 as u32,
        0x80070005
    );
    println!("sanitized-storage-chain:{message}");
}

#[test]
fn actual_missing_pinned_file_preserves_its_owned_relative_path_and_io_source() {
    use std::error::Error;
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("cs".into(), "synthetic error binding".into(), true, now())
        .unwrap();
    let wav = to_wav(&[1, 0, 2, 0]).unwrap();
    journal
        .checkpoint(&[], 4, &[audio(&wav, 4)], false, now())
        .unwrap();
    let descriptor = journal.snapshot().unwrap().chunks[0].clone();
    let pin = journal
        .pin_audio(&descriptor, "synthetic error binding")
        .unwrap();
    let path = store.0.audio_path(journal.id(), 0).unwrap();
    std::fs::remove_file(&path).unwrap();
    let error = pin.read().unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("{}\\00000000.wav.enc", journal.id()))
    );
    assert!(error.to_string().contains("open pinned recovery audio"));
    let io = error
        .source()
        .unwrap()
        .source()
        .unwrap()
        .downcast_ref::<io::Error>()
        .unwrap();
    assert_eq!(io.kind(), io::ErrorKind::NotFound);
    assert_eq!(io.raw_os_error(), Some(2));
    println!("actual-pinned-file-error:{error}");
}

fn controlled_pause() -> (
    Arc<FixtureWritePause>,
    FixtureWriteRelease,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (entered, observed) = tokio::sync::oneshot::channel();
    let pause = Arc::new(FixtureWritePause {
        entered: Mutex::new(Some(entered)),
        released: Mutex::new(false),
        changed: std::sync::Condvar::new(),
    });
    (pause.clone(), FixtureWriteRelease(pause), observed)
}

#[test]
fn actual_atomic_replace_manifest_reader_flags_and_alias_characterization() {
    let directory = TemporaryRoot::new();
    for (name, flags, alias, share) in [
        ("shared-read", 0, false, 7),
        (
            "reparse-open",
            windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT.0,
            false,
            7,
        ),
        ("alias-read", 0, true, 7),
        ("alias-no-write", 0, true, 5),
        ("alias-read-only", 0, true, 1),
        (
            "alias-reparse-read-only",
            windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT.0,
            true,
            1,
        ),
        (
            "alias-reparse",
            windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT.0,
            true,
            7,
        ),
    ] {
        let target = directory.0.join(format!("{name}.enc"));
        atomic_write(&target, b"old immutable publication", WriteMode::Replace).unwrap();
        let opened = if alias {
            let link = directory.0.join(format!("{name}.reader.enc"));
            std::fs::hard_link(&target, &link).unwrap();
            link
        } else {
            target.clone()
        };
        let mut handle = OpenOptions::new()
            .read(true)
            .share_mode(share)
            .custom_flags(flags)
            .open(opened)
            .unwrap();
        if alias && share == 1 {
            assert!(std::fs::write(&target, b"denied mutable payload").is_err());
        }
        let replaced = atomic_write(&target, b"new durable publication", WriteMode::Replace);
        println!(
            "actual-atomic-reader-matrix: name={name}; share={share}; customflags={flags}; result={replaced:?}"
        );
        if alias {
            assert!(
                replaced.is_ok(),
                "The immutable read name must not block live destination publication."
            );
        }
        let expected = if replaced.is_ok() {
            b"new durable publication".as_slice()
        } else {
            b"old immutable publication".as_slice()
        };
        assert_eq!(std::fs::read(&target).unwrap(), expected);
        let mut immutable = Vec::new();
        handle.read_to_end(&mut immutable).unwrap();
        assert_eq!(immutable, b"old immutable publication");
        drop(handle);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_manifest_alias_cleanup_failure_is_visible_not_a_cached_success_or_detached_drop() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let mut journal = store
        .create("cs".into(), "alias cleanup".into(), true, now())
        .unwrap();
    let pin = store.fixture_pin_manifest(journal.id()).unwrap();
    let (release, entered) = store.pause_next_manifest_read(ManifestReadPhase::Unprotect);
    let reader = tokio::task::spawn_blocking(move || pin.read());
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    let alias = std::fs::read_dir(store.0.session_path(journal.id()).unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("manifest.reader.")
        })
        .unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&alias)
        .unwrap();
    drop(release);
    let error = reader.await.unwrap().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("release immutable manifest read name")
    );
    assert!(journal.published_snapshot().is_err());
    assert!(
        store.fixture_pin_manifest(journal.id()).is_err(),
        "Failed cleanup cannot accumulate more read-name orphans."
    );
    assert!(
        alias.exists(),
        "Failed explicit cleanup stays quota-visible, not detached."
    );
    drop(held);
    journal.delete().unwrap();
}

#[test]
fn crashed_manifest_read_name_is_ignored_by_codec_but_charged_and_deleted_on_recovery() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("cs".into(), "reader crash binding".into(), true, now())
        .unwrap();
    let id = journal.id().to_owned();
    let wav = to_wav(&[1, 0, 2, 0]).unwrap();
    journal
        .checkpoint_snapshot(&[], 4, &[audio(&wav, 4)], false, now())
        .unwrap();
    let folder = store.0.session_path(&id).unwrap();
    let alias = folder.join("manifest.reader.7.11.enc");
    std::fs::hard_link(folder.join("manifest.enc"), &alias).unwrap();
    journal
        .checkpoint_snapshot(&[], 8, &[audio(&wav, 8)], false, now())
        .unwrap();
    drop(journal);
    let mut reopened = store.open(&id).unwrap();
    assert_eq!(reopened.published_snapshot().unwrap().captured_bytes, 8);
    assert_eq!(reopened.read_audio(0).unwrap(), wav);
    assert_eq!(reopened.read_audio(1).unwrap(), wav);
    let current_bytes: u64 = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    let alias_bytes = std::fs::metadata(&alias).unwrap().len();
    let if_alias_were_ignored = (DEFAULT_RECOVERY_QUOTA - current_bytes + alias_bytes) as usize;
    assert!(store.0.ensure_quota(if_alias_were_ignored).is_err());
    reopened.delete().unwrap();
    assert!(!folder.exists());
}

#[test]
fn owned_publication_view_fails_when_root_ownership_is_poisoned() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("cs".into(), "root failure".into(), true, now())
        .unwrap();
    let root = store.0.clone();
    assert!(
        std::thread::spawn(move || {
            let _owned = root.active.lock().unwrap();
            panic!("Controlled synthetic ownership failure.");
        })
        .join()
        .is_err()
    );
    assert!(journal.published_snapshot().is_err());
    assert!(journal.snapshot().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn journal_drop_revokes_only_metadata_while_another_owned_mutation_is_blocked() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let released = store
        .create("cs".into(), "released lease".into(), true, now())
        .unwrap();
    let id = released.id().to_owned();
    let writer = store
        .create("cs".into(), "blocked writer".into(), true, now())
        .unwrap();
    let (release, entered) = store.pause_next_fixture_write();
    let writing = tokio::task::spawn_blocking(move || {
        let wav = to_wav(&[1, 0, 2, 0]).unwrap();
        writer.checkpoint_snapshot(&[], 4, &[audio(&wav, 4)], false, now())
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    let mut dropping = tokio::spawn(async move { drop(released) });
    let responsive =
        tokio::time::timeout(std::time::Duration::from_millis(200), &mut dropping).await;
    let dropped_ok = matches!(&responsive, Ok(Ok(())));
    let writer_still_blocked = !writing.is_finished();
    drop(release);
    if responsive.is_err() {
        dropping.await.unwrap();
    }
    writing.await.unwrap().unwrap();
    assert!(
        dropped_ok && writer_still_blocked,
        "Drop waited on filesystem-owning root mutation."
    );
    assert_eq!(
        store
            .open(&id)
            .unwrap()
            .published_snapshot()
            .unwrap()
            .context,
        "released lease"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_fresh_manifest_phases_leave_owned_checkpoint_confirmation_and_timers_free() {
    for phase in [
        ManifestReadPhase::Open,
        ManifestReadPhase::Read,
        ManifestReadPhase::Unprotect,
    ] {
        let directory = TemporaryRoot::new();
        let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
        let journal = Arc::new(
            store
                .create("cs".into(), "manifest phase binding".into(), true, now())
                .unwrap(),
        );
        let wav = to_wav(&[1, 0, 2, 0]).unwrap();
        journal
            .checkpoint_snapshot(&[], 4, &[audio(&wav, 4)], false, now())
            .unwrap();
        let pin = store.fixture_pin_manifest(journal.id()).unwrap();
        assert!(store.fixture_pin_manifest(journal.id()).is_err());
        let (release, entered) = store.pause_next_manifest_read(phase);
        let reader = tokio::task::spawn_blocking(move || pin.read());
        tokio::time::timeout(std::time::Duration::from_secs(5), entered)
            .await
            .unwrap()
            .unwrap();
        if phase == ManifestReadPhase::Read {
            let live = store
                .0
                .session_path(journal.id())
                .unwrap()
                .join("manifest.enc");
            assert!(std::fs::write(live, b"blocked foreign mutation").is_err());
        }
        assert!(store.delete_idle(journal.id()).is_err());
        let writer = journal.clone();
        let wave = wav.clone();
        let began = std::time::Instant::now();
        let mut writing = tokio::task::spawn_blocking(move || {
            writer.checkpoint_snapshot(&[], 8, &[audio(&wave, 8)], false, now())?;
            writer.confirm_streaming_snapshot(4, "confirmed".into(), vec![], false, None, now())?;
            writer.published_snapshot()
        });
        let progress = tokio::time::timeout(std::time::Duration::from_secs(2), &mut writing).await;
        let writer_elapsed_ms = began.elapsed().as_secs_f64() * 1000.0;
        let still_blocked = !reader.is_finished();
        let timer = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            tokio::time::sleep(std::time::Duration::from_millis(5)),
        )
        .await;
        drop(release);
        let written = match progress {
            Ok(result) => result.unwrap().unwrap(),
            Err(error) => {
                let _ = writing.await;
                let _ = reader.await;
                panic!("Owned writer blocked on an unrelated fresh manifest read: {error}");
            }
        };
        let inspected = reader.await.unwrap();
        assert!(still_blocked && timer.is_ok());
        assert_eq!((written.captured_bytes, written.confirmed_bytes), (8, 4));
        assert!(
            inspected
                .unwrap_err()
                .to_string()
                .contains("changed during fresh inspection")
        );
        let inspected = journal.snapshot().unwrap();
        assert_eq!(
            (inspected.captured_bytes, inspected.confirmed_bytes),
            (8, 4)
        );
        assert_eq!(journal.read_audio(1).unwrap(), wav);
        println!(
            "blocked-fresh-manifest: writer-and-confirm-progress-ms={}; latest-durable=8/4; reader-was-blocked=true",
            writer_elapsed_ms
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_read_handles_remain_quota_charged_until_the_joined_pin_is_released() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("cs".into(), "manifest quota".into(), true, now())
        .unwrap();
    let pin = store.fixture_pin_manifest(journal.id()).unwrap();
    let (release, entered) = store.pause_next_manifest_read(ManifestReadPhase::Read);
    let reader = tokio::task::spawn_blocking(move || pin.read());
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    let wav = to_wav(&[1, 0, 2, 0]).unwrap();
    journal
        .checkpoint_snapshot(&[], 4, &[audio(&wav, 4)], false, now())
        .unwrap();
    let bytes: u64 = std::fs::read_dir(&store.0.path)
        .unwrap()
        .flat_map(|entry| {
            let entry = entry.unwrap();
            if entry.metadata().unwrap().is_dir() {
                std::fs::read_dir(entry.path())
                    .unwrap()
                    .map(|child| child.unwrap().metadata().unwrap().len())
                    .collect::<Vec<_>>()
            } else {
                vec![entry.metadata().unwrap().len()]
            }
        })
        .sum();
    let additional = (DEFAULT_RECOVERY_QUOTA - bytes) as usize;
    assert!(
        store.0.ensure_quota(additional).is_err(),
        "Retired/open manifest capacity cannot disappear from quota."
    );
    drop(release);
    assert!(reader.await.unwrap().is_err());
    store.0.ensure_quota(additional).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_pin_survives_owner_release_but_never_returns_a_new_owners_cached_state() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("cs".into(), "original manifest lease".into(), true, now())
        .unwrap();
    let id = journal.id().to_owned();
    let pin = store.fixture_pin_manifest(&id).unwrap();
    let (release, entered) = store.pause_next_manifest_read(ManifestReadPhase::Unprotect);
    let reader = tokio::task::spawn_blocking(move || pin.read());
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    drop(journal);
    assert!(store.open(&id).is_err());
    assert!(store.delete_idle(&id).is_err());
    drop(release);
    assert!(
        reader
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("owner has been released")
    );
    let mut reopened = store.open(&id).unwrap();
    assert_eq!(
        reopened.published_snapshot().unwrap().context,
        "original manifest lease"
    );
    reopened.delete().unwrap();
    assert!(reopened.published_snapshot().is_err());
    assert!(reopened.snapshot().is_err());
}

#[test]
fn failed_actual_publication_never_exposes_inflight_or_cached_success() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = Arc::new(
        store
            .create(
                "cs".into(),
                "publication failure binding".into(),
                true,
                now(),
            )
            .unwrap(),
    );
    let path = store
        .0
        .session_path(journal.id())
        .unwrap()
        .join("manifest.enc");
    let held = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    let cipher_before = std::fs::read(&path).unwrap();
    let wav = to_wav(&[1, 0, 2, 0]).unwrap();
    assert!(
        journal
            .checkpoint_snapshot(&[], 4, &[audio(&wav, 4)], false, now())
            .is_err()
    );
    assert!(
        journal.published_snapshot().is_err(),
        "An I/O error cannot fall back to a cache-success result."
    );
    assert_eq!(journal.snapshot().unwrap().captured_bytes, 0);
    assert!(
        journal.published_snapshot().is_err(),
        "Fresh prefix inspection is not implicit pipeline recovery."
    );
    assert_eq!(std::fs::read(&path).unwrap(), cipher_before);
    drop(held);
    journal
        .checkpoint(&[], 4, &[audio(&wav, 4)], false, now())
        .unwrap();
    assert_eq!(journal.published_snapshot().unwrap().captured_bytes, 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inflight_checkpoint_view_stays_durable_and_fresh_io_error_is_not_a_cache_fallback() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = Arc::new(
        store
            .create("cs".into(), "publication view".into(), true, now())
            .unwrap(),
    );
    let (release, entered) = store.pause_next_fixture_write();
    let writer = journal.clone();
    let mut writing = tokio::task::spawn_blocking(move || {
        let wav = to_wav(&[1, 0, 2, 0]).unwrap();
        writer.checkpoint_snapshot(&[], 4, &[audio(&wav, 4)], false, now())
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(journal.published_snapshot().unwrap().captured_bytes, 0);
    drop(release);
    (&mut writing).await.unwrap().unwrap();
    assert_eq!(journal.published_snapshot().unwrap().captured_bytes, 4);
    let pin = store.fixture_pin_manifest(journal.id()).unwrap();
    let (release, entered) = store.pause_next_manifest_read(ManifestReadPhase::Open);
    let reading = tokio::task::spawn_blocking(move || pin.read());
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    let path = store
        .0
        .session_path(journal.id())
        .unwrap()
        .join("manifest.enc");
    std::fs::write(&path, b"corrupt controlled manifest").unwrap();
    drop(release);
    assert!(reading.await.unwrap().is_err());
    assert!(journal.published_snapshot().is_err());
    assert!(
        journal
            .checkpoint_snapshot(&[], 4, &[], true, now())
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), b"corrupt controlled manifest");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_manifest_read_failure_is_not_replaced_by_a_newer_cached_publication() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("cs".into(), "failed stale inspection".into(), true, now())
        .unwrap();
    let wav = to_wav(&[1, 0, 2, 0]).unwrap();
    journal
        .checkpoint_snapshot(&[], 4, &[audio(&wav, 4)], false, now())
        .unwrap();
    let pin = store.fixture_pin_manifest(journal.id()).unwrap();
    let (release, entered) = store.pause_next_manifest_read(ManifestReadPhase::Open);
    let reader = tokio::task::spawn_blocking(move || pin.read());
    tokio::time::timeout(std::time::Duration::from_secs(5), entered)
        .await
        .unwrap()
        .unwrap();
    let alias = std::fs::read_dir(store.0.session_path(journal.id()).unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("manifest.reader.")
        })
        .unwrap();
    std::fs::write(alias, b"controlled old-generation corruption").unwrap();
    journal
        .checkpoint_snapshot(&[], 8, &[audio(&wav, 8)], false, now())
        .unwrap();
    assert_eq!(journal.published_snapshot().unwrap().captured_bytes, 8);
    drop(release);
    assert!(
        reader.await.unwrap().is_err(),
        "Newer cache cannot mask failed actual IO/decryption."
    );
    assert!(journal.published_snapshot().is_err());
    assert_eq!(journal.snapshot().unwrap().captured_bytes, 8);
    assert!(journal.published_snapshot().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn last_pin_delete_quota_metadata_hypothesis_has_an_explicit_actual_result() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("cs".into(), "synthetic race binding".into(), true, now())
        .unwrap();
    let wav = to_wav(&[1, 0, 2, 0]).unwrap();
    journal
        .checkpoint(&[], 4, &[audio(&wav, 4)], false, now())
        .unwrap();
    let descriptor = journal.snapshot().unwrap().chunks[0].clone();
    let pin = journal
        .pin_audio(&descriptor, "synthetic race binding")
        .unwrap();
    journal
        .confirm_streaming(4, "confirmed".into(), vec![], false, None, now())
        .unwrap();
    let path = store.0.audio_path(journal.id(), 0).unwrap();
    let (pause, release_delete, delete_entered) = controlled_pause();
    *store.0.next_deferred_delete_pause.lock().unwrap() = Some(pause);
    let deleting = tokio::task::spawn_blocking(move || drop(pin));
    tokio::time::timeout(std::time::Duration::from_secs(5), delete_entered)
        .await
        .unwrap()
        .unwrap();
    let (pause, release_quota, quota_entered) = controlled_pause();
    *store.0.quota_entry_pause.lock().unwrap() = Some((path.clone(), pause));
    let scanning = store.clone();
    let quota = tokio::task::spawn_blocking(move || {
        let _owned = scanning.0.lock()?;
        scanning.0.ensure_quota(1)
    });
    let enumerated_before_delete =
        tokio::time::timeout(std::time::Duration::from_secs(5), quota_entered).await;
    drop(release_delete);
    deleting.await.unwrap();
    drop(release_quota);
    let result = quota.await.unwrap();
    enumerated_before_delete
        .expect("The actual enumerated entry never reached the metadata barrier.")
        .unwrap();
    println!(
        "actual-last-pin-quota-overlap: enumerated=true; deleted-before-metadata=true; result={result:?}; \
         NotFound hypothesis is not established by overlap alone"
    );
    assert!(!path.exists());
    assert!(
        result.is_ok(),
        "Actual quota result changed; preserve its precise error before any proposed repair: {result:?}"
    );
}

#[test]
fn immutable_audio_pin_defers_confirmed_delete_and_blocks_session_reopen_until_release() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let mut journal = store
        .create("cs".into(), "synthetic pin binding".into(), true, now())
        .unwrap();
    let id = journal.id().to_owned();
    let wav = to_wav(&[1, 0, 2, 0]).unwrap();
    journal
        .checkpoint(&[], 4, &[audio(&wav, 4)], false, now())
        .unwrap();
    let descriptor = journal.snapshot().unwrap().chunks[0].clone();
    assert!(journal.pin_audio(&descriptor, "different binding").is_err());
    let mut wrong = descriptor.clone();
    wrong.digest[0] ^= 1;
    assert!(journal.pin_audio(&wrong, "synthetic pin binding").is_err());
    let pin = journal
        .pin_audio(&descriptor, "synthetic pin binding")
        .unwrap();
    let second_pin = journal
        .pin_audio(&descriptor, "synthetic pin binding")
        .unwrap();
    journal
        .confirm_streaming(4, "synthetic confirmed".into(), vec![], false, None, now())
        .unwrap();
    let path = directory.0.join(&id).join("00000000.wav.enc");
    assert!(
        path.exists(),
        "Confirmed audio is still owned by the pinned reader."
    );
    assert!(
        journal
            .pin_audio(&descriptor, "synthetic pin binding")
            .is_err()
    );
    assert!(
        journal.delete().is_err(),
        "Explicit deletion cannot invalidate a pinned reader."
    );
    drop(journal);
    assert!(store.open(&id).is_err());
    assert!(store.delete_idle(&id).is_err());
    assert_eq!(pin.read().unwrap(), wav);
    assert!(
        path.exists(),
        "One remaining reader still owns the confirmed file."
    );
    assert_eq!(second_pin.read().unwrap(), wav);
    assert!(
        !path.exists(),
        "The final reader releases deferred confirmed audio."
    );
    let restored = store.open(&id).unwrap();
    assert_eq!(restored.snapshot().unwrap().confirmed_bytes, 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_pinned_file_read_leaves_journal_writes_confirmation_and_timers_responsive() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = Arc::new(
        store
            .create("cs".into(), "synthetic pin binding".into(), true, now())
            .unwrap(),
    );
    let first = to_wav(&[1, 0, 2, 0]).unwrap();
    journal
        .checkpoint(&[], 4, &[audio(&first, 4)], false, now())
        .unwrap();
    let descriptor = journal.snapshot().unwrap().chunks[0].clone();
    let pin = journal
        .pin_audio(&descriptor, "synthetic pin binding")
        .unwrap();
    let (release, blocked) = store.pause_next_pinned_read();
    let reader = tokio::task::spawn_blocking(move || pin.read());
    tokio::time::timeout(std::time::Duration::from_secs(5), blocked)
        .await
        .unwrap()
        .unwrap();
    let path = directory.0.join(journal.id()).join("00000000.wav.enc");
    let cipher = std::fs::read(&path).unwrap();
    assert!(std::fs::write(&path, b"synthetic rejected mutation").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), cipher);
    let second = to_wav(&[3, 0, 4, 0]).unwrap();
    let writer = journal.clone();
    let second_write = second.clone();
    let began = std::time::Instant::now();
    let mut progress = tokio::task::spawn_blocking(move || {
        writer.checkpoint_snapshot(&[], 8, &[audio(&second_write, 8)], false, now())?;
        writer.confirm_streaming_snapshot(
            4,
            "synthetic confirmed".into(),
            vec![],
            false,
            None,
            now(),
        )
    });
    let completed = tokio::time::timeout(std::time::Duration::from_secs(1), &mut progress).await;
    let writer_elapsed_ms = began.elapsed().as_secs_f64() * 1000.0;
    let still_reading = !reader.is_finished();
    drop(release);
    let audio = reader.await;
    if completed.is_err() {
        let _ = progress.await;
    }
    completed
        .expect("Pinned slow I/O blocked journal progress.")
        .unwrap()
        .unwrap();
    assert!(still_reading);
    assert_eq!(audio.unwrap().unwrap(), first);
    let state = journal.snapshot().unwrap();
    assert_eq!(state.captured_bytes, 8);
    assert_eq!(state.confirmed_bytes, 4);
    assert_eq!(state.context, "synthetic pin binding");
    assert_eq!(journal.read_audio(1).unwrap(), second);
    println!(
        "{{\"scope\":\"blocked-pinned-read-root-progress\",\"checkpoint_and_confirm_ms\":{writer_elapsed_ms},\"reader_still_blocked_during_progress\":{still_reading},\"original_binding\":true,\"saved_bytes\":8,\"confirmed_bytes\":4}}"
    );
}

#[test]
fn active_journal_rejects_changed_binding_before_new_audio_or_read_admission() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create(
            "cs".into(),
            "original synthetic binding".into(),
            true,
            now(),
        )
        .unwrap();
    let id = journal.id().to_owned();
    let wav = to_wav(&[1, 0, 2, 0]).unwrap();
    journal
        .checkpoint(&[], 4, &[audio(&wav, 4)], false, now())
        .unwrap();
    let mut changed = journal.snapshot().unwrap();
    changed.context = "changed synthetic binding".into();
    let path = directory.0.join(&id).join("manifest.enc");
    crate::NativeRecoveryManifestStore::new(path.clone())
        .unwrap()
        .save_fixture(&changed.encode().unwrap())
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(journal.snapshot().is_err());
    assert!(
        journal
            .checkpoint(&[], 8, &[audio(&wav, 8)], false, now())
            .is_err()
    );
    assert!(!directory.0.join(&id).join("00000001.wav.enc").exists());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn corrupt_pinned_payload_rejects_data_and_releases_read_ownership() {
    let directory = TemporaryRoot::new();
    let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
    let journal = store
        .create("cs".into(), "synthetic pin binding".into(), true, now())
        .unwrap();
    let id = journal.id().to_owned();
    let wav = to_wav(&[1, 0, 2, 0]).unwrap();
    journal
        .checkpoint(&[], 4, &[audio(&wav, 4)], false, now())
        .unwrap();
    let descriptor = journal.snapshot().unwrap().chunks[0].clone();
    let pin = journal
        .pin_audio(&descriptor, "synthetic pin binding")
        .unwrap();
    let path = directory.0.join(&id).join("00000000.wav.enc");
    let mut cipher = std::fs::read(&path).unwrap();
    cipher[0] ^= 1;
    std::fs::write(&path, &cipher).unwrap();
    assert!(pin.read().is_err());
    drop(journal);
    assert!(
        store.delete_idle(&id).is_ok(),
        "Failed reads must still release their pin."
    );
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
fn reopen_retries_confirmed_orphan_deletion_without_touching_pending_audio() {
    for streaming in [false, true] {
        let directory = TemporaryRoot::new();
        let store = NativeRecoveryStore::new(directory.0.clone(), DEFAULT_RECOVERY_QUOTA).unwrap();
        let journal = store
            .create("auto".into(), "synthetic".into(), streaming, now())
            .unwrap();
        let id = journal.id().to_owned();
        let first = to_wav(&[1; 6400]).unwrap();
        let pending = to_wav(&[2; 6400]).unwrap();
        journal
            .checkpoint(
                &[],
                12800,
                &[audio(&first, 6400), audio(&pending, 12800)],
                true,
                now(),
            )
            .unwrap();
        if streaming {
            journal
                .confirm_streaming(
                    6400,
                    "synthetic".into(),
                    vec![1; 6400],
                    false,
                    Some(1),
                    now(),
                )
                .unwrap();
        } else {
            journal.save_result(0, "synthetic".into(), now()).unwrap();
        }
        let orphan = store.0.audio_path(&id, 0).unwrap();
        std::fs::write(&orphan, protect(&first).unwrap()).unwrap();
        if streaming {
            let locked = OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&orphan)
                .unwrap();
            assert!(
                journal
                    .confirm_streaming(
                        6400,
                        "synthetic".into(),
                        vec![1; 6400],
                        false,
                        Some(1),
                        now(),
                    )
                    .is_err()
            );
            drop(locked);
            journal
                .confirm_streaming(
                    6400,
                    "synthetic".into(),
                    vec![1; 6400],
                    false,
                    Some(1),
                    now(),
                )
                .unwrap();
            assert!(
                !orphan.exists(),
                "Same-cursor retry must finish the earlier deletion."
            );
            std::fs::write(&orphan, protect(&first).unwrap()).unwrap();
        }
        drop(journal);
        let locked = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&orphan)
            .unwrap();
        assert!(
            store.open(&id).is_err(),
            "Failed cleanup must remain visible and retryable."
        );
        assert!(store.0.audio_path(&id, 1).unwrap().exists());
        drop(locked);
        let restored = store.open(&id).unwrap();
        assert!(!orphan.exists());
        assert_eq!(restored.read_audio(1).unwrap(), pending);
        assert!(restored.snapshot().unwrap().chunks[0].text.is_some());
        assert!(restored.snapshot().unwrap().chunks[1].text.is_none());
    }
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

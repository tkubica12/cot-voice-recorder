#[cfg(test)]
use crate::lock_trace::Guard as MutexGuard;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::sync::MutexGuard;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use time::OffsetDateTime;
use voiceprompt_client_protocol::recovery::{
    AudioCheckpoint, MAX_AUDIO_BYTES, MAX_MANIFEST_BYTES, RecoveryChunk, RecoveryManifest,
    utc_ticks, valid_id,
};

use crate::platform::{
    NativeError, WriteMode, atomic_write, guard_recovery_path, protect, unprotect,
};

pub const DEFAULT_RECOVERY_QUOTA: u64 = 256 * 1024 * 1024;
const ENCRYPTION_ALLOWANCE: usize = 64 * 1024;
const RETENTION_TICKS: i64 = 48 * 60 * 60 * 10_000_000;

struct RecoveryRoot {
    path: PathBuf,
    maximum_bytes: u64,
    _writer: File,
    active: Mutex<HashSet<String>>,
    readers: Mutex<HashMap<String, Weak<Mutex<ReaderState>>>>,
    #[cfg(test)]
    next_write_pause: Mutex<Option<Arc<FixtureWritePause>>>,
    #[cfg(test)]
    next_pinned_read_pause: Mutex<Option<Arc<FixtureWritePause>>>,
    #[cfg(test)]
    next_deferred_delete_pause: Mutex<Option<Arc<FixtureWritePause>>>,
    #[cfg(test)]
    quota_entry_pause: Mutex<Option<(PathBuf, Arc<FixtureWritePause>)>>,
    #[cfg(test)]
    manifest_read_pause: Mutex<Option<(ManifestReadPhase, Arc<FixtureWritePause>)>>,
    #[cfg(test)]
    manifest_reads_started: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    io_probe: FixtureIoProbe,
    #[cfg(test)]
    trace: Arc<crate::lock_trace::Lock>,
}

#[cfg(test)]
struct FixtureIoProbe {
    clock: std::time::Instant,
    phase: std::sync::atomic::AtomicU8,
    phase_started_ns: std::sync::atomic::AtomicU64,
    last_phase: std::sync::atomic::AtomicU8,
}

#[cfg(test)]
impl FixtureIoProbe {
    fn enter(&self, phase: u8) {
        use std::sync::atomic::Ordering;
        self.phase_started_ns
            .store(self.clock.elapsed().as_nanos() as u64, Ordering::Release);
        self.phase.store(phase, Ordering::Release);
    }
}

#[cfg(test)]
pub(crate) struct FixtureIoEnd<'a>(&'a FixtureIoProbe);

#[cfg(test)]
impl Drop for FixtureIoEnd<'_> {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        let phase = self.0.phase.swap(0, Ordering::AcqRel);
        self.0.last_phase.store(phase, Ordering::Release);
    }
}

#[cfg(test)]
struct FixtureWritePause {
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    released: Mutex<bool>,
    changed: std::sync::Condvar,
}

#[cfg(test)]
impl FixtureWritePause {
    fn wait(&self) {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.changed.wait(released).unwrap();
        }
    }
}

#[cfg(test)]
pub(crate) struct FixtureWriteRelease(Arc<FixtureWritePause>);

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManifestReadPhase {
    Open,
    Read,
    Unprotect,
}

enum ReadPurpose {
    Manifest,
    InspectedManifest,
    PinnedAudio,
    Other,
}

#[derive(Clone, Copy)]
enum ManifestSource {
    Fresh,
    Published,
}

#[cfg(test)]
impl Drop for FixtureWriteRelease {
    fn drop(&mut self) {
        *self.0.released.lock().unwrap() = true;
        self.0.changed.notify_all();
    }
}

#[derive(Clone)]
pub struct NativeRecoveryStore(Arc<RecoveryRoot>);

pub struct NativeRecoveryJournal {
    root: Arc<RecoveryRoot>,
    id: String,
    deleted: bool,
    readers: Arc<Mutex<ReaderState>>,
}

struct ReaderState {
    context: String,
    published: Arc<RecoveryManifest>,
    manifest_cipher_bytes: u64,
    manifest_pin_bytes: u64,
    generation: u64,
    owner_live: bool,
    publication_valid: bool,
    manifest_pin: bool,
    manifest_cleanup_failed: bool,
    available: HashMap<u32, RecoveryChunk>,
    pins: HashMap<u32, usize>,
    deferred_delete: HashSet<u32>,
}

impl ReaderState {
    fn new(state: &RecoveryManifest, manifest_cipher_bytes: u64) -> Self {
        Self {
            context: state.context.clone(),
            published: Arc::new(state.clone()),
            manifest_cipher_bytes,
            manifest_pin_bytes: 0,
            generation: 1,
            owner_live: true,
            publication_valid: true,
            manifest_pin: false,
            manifest_cleanup_failed: false,
            available: state
                .chunks
                .iter()
                .filter(|chunk| chunk.text.is_none())
                .map(|chunk| (chunk.index, chunk.clone()))
                .collect(),
            pins: HashMap::new(),
            deferred_delete: HashSet::new(),
        }
    }

    fn publish(&mut self, state: &RecoveryManifest, cipher_bytes: u64) {
        self.published = Arc::new(state.clone());
        self.manifest_cipher_bytes = cipher_bytes;
        if self.manifest_pin {
            self.manifest_pin_bytes = self.manifest_pin_bytes.max(cipher_bytes);
        }
        self.generation += 1;
        self.publication_valid = true;
        self.available = state
            .chunks
            .iter()
            .filter(|chunk| chunk.text.is_none())
            .map(|chunk| (chunk.index, chunk.clone()))
            .collect();
    }
}

pub(crate) struct ManifestReadPin {
    root: Arc<RecoveryRoot>,
    id: String,
    readers: Arc<Mutex<ReaderState>>,
    expected: Arc<RecoveryManifest>,
    generation: u64,
    alias_pending: bool,
}

impl ManifestReadPin {
    fn new(
        root: Arc<RecoveryRoot>,
        id: String,
        readers: Arc<Mutex<ReaderState>>,
    ) -> Result<Self, NativeError> {
        let ownership = root.lock()?;
        if !ownership.contains(&id) {
            return Err(NativeError::Verification(
                "Fresh manifest inspection lost its journal ownership.",
            ));
        }
        let mut state = readers
            .lock()
            .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
        if state.manifest_cleanup_failed {
            return Err(NativeError::Verification(
                "The previous manifest read name could not be released; close this journal before retrying inspection.",
            ));
        }
        if !state.owner_live || state.manifest_pin {
            return Err(NativeError::Verification(
                "Recovery fresh inspection requires its live owner and one available read pin.",
            ));
        }
        state.manifest_pin = true;
        state.manifest_pin_bytes = state.manifest_cipher_bytes;
        let expected = state.published.clone();
        let generation = state.generation;
        drop(state);
        drop(ownership);
        Ok(Self {
            root,
            id,
            readers,
            expected,
            generation,
            alias_pending: false,
        })
    }

    pub(crate) fn read(mut self) -> Result<RecoveryManifest, NativeError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let directory = self.root.session_path(&self.id)?;
        let source = directory.join("manifest.enc");
        let alias = directory.join(format!(
            "manifest.reader.{}.{}.enc",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let admitted = (|| {
            #[cfg(test)]
            let _mutation = crate::mutation_trace::span("manifest-reader-name-admission");
            guard_recovery_path(&source)?;
            guard_recovery_path(&alias)?;
            std::fs::hard_link(&source, &alias)
                .map_err(|error| NativeError::Io("pin immutable manifest read name", error))
        })()
        .map_err(|error| self.root.path_error(&source, error));
        if let Err(error) = admitted {
            return self.finish(Err(error));
        }
        self.alias_pending = true;
        // MoveFileExW can reject an opened destination even with share-delete.
        // Read a separate name of that immutable file, never the live destination.
        let result = self.finish(self.root.read_manifest_path(&self.id, &alias, true));
        let cleanup = (|| {
            let _ownership = self.root.lock()?;
            #[cfg(test)]
            let _mutation = crate::mutation_trace::span("manifest-reader-name-release");
            #[cfg(test)]
            let _probe = {
                self.root.io_probe.enter(10);
                FixtureIoEnd(&self.root.io_probe)
            };
            guard_recovery_path(&alias)?;
            std::fs::remove_file(&alias)
                .map_err(|error| NativeError::Io("release immutable manifest read name", error))
        })()
        .map_err(|error| self.root.path_error(&alias, error));
        if cleanup.is_ok() {
            self.alias_pending = false;
        }
        if cleanup.is_err() {
            match self.readers.lock() {
                Ok(mut readers) => readers.publication_valid = false,
                Err(_) => {
                    eprintln!("Fresh manifest cleanup failure invalidation failed: poisoned lock.")
                }
            }
        }
        match (result, cleanup) {
            (Err(error), cleanup) => {
                if let Err(cleanup) = cleanup {
                    eprintln!("Fresh manifest read cleanup also failed: {cleanup}");
                }
                Err(error)
            }
            (Ok(state), Ok(())) => Ok(state),
            (Ok(_), Err(error)) => self.finish(Err(error)),
        }
    }

    fn finish(
        &self,
        result: Result<RecoveryManifest, NativeError>,
    ) -> Result<RecoveryManifest, NativeError> {
        let matches_publication = result
            .as_ref()
            .is_ok_and(|state| state == self.expected.as_ref());
        let mut readers = match self.readers.lock() {
            Ok(readers) => readers,
            Err(_) => {
                eprintln!("Fresh manifest result validation failed: poisoned lock.");
                return match result {
                    Err(error) => Err(error),
                    Ok(_) => Err(NativeError::Verification(
                        "Recovery read pin lock is poisoned.",
                    )),
                };
            }
        };
        let state = match result {
            Ok(state) => state,
            Err(error) => {
                readers.publication_valid = false;
                return Err(error);
            }
        };
        if state.context != self.expected.context {
            readers.publication_valid = false;
            return Err(NativeError::Verification(
                "Recovery journal no longer matches its original binding.",
            ));
        }
        if !readers.owner_live || self.root.active.is_poisoned() || self.root.readers.is_poisoned()
        {
            return Err(NativeError::Verification(
                "Recovery snapshot owner has been released.",
            ));
        }
        if readers.generation == self.generation {
            if !matches_publication {
                readers.publication_valid = false;
                return Err(NativeError::Verification(
                    "Recovery changed outside its journal writer.",
                ));
            }
            return Ok(state);
        }
        Err(NativeError::Verification(
            "Recovery publication changed during fresh inspection; retry the inspection.",
        ))
    }
}

impl Drop for ManifestReadPin {
    fn drop(&mut self) {
        match self.readers.lock() {
            Ok(mut readers) => {
                readers.manifest_pin = false;
                readers.manifest_pin_bytes = 0;
                if self.alias_pending {
                    readers.manifest_cleanup_failed = true;
                    readers.publication_valid = false;
                }
            }
            Err(_) => eprintln!("Recovery manifest read pin release failed: poisoned lock."),
        }
    }
}

pub(crate) struct PinnedRecoveryAudio {
    root: Arc<RecoveryRoot>,
    readers: Arc<Mutex<ReaderState>>,
    id: String,
    descriptor: RecoveryChunk,
}

impl PinnedRecoveryAudio {
    pub fn read(self) -> Result<Vec<u8>, NativeError> {
        let path = self.root.audio_path(&self.id, self.descriptor.index)?;
        let result = self.read_path(&path);
        if result.is_err() {
            match self.readers.lock() {
                Ok(mut readers) => readers.publication_valid = false,
                Err(_) => eprintln!("Recovery audio failure invalidation failed: poisoned lock."),
            }
        }
        result.map_err(|error| self.root.path_error(&path, error))
    }

    fn read_path(&self, path: &Path) -> Result<Vec<u8>, NativeError> {
        use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
        guard_recovery_path(path)?;
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)
            .map_err(|error| NativeError::Io("open pinned recovery audio", error))?;
        let metadata = file
            .metadata()
            .map_err(|error| NativeError::Io("inspect pinned recovery audio", error))?;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(NativeError::Verification(
                "Pinned recovery audio refuses reparse points.",
            ));
        }
        let mut audio =
            self.root
                .read_open_file(file, MAX_AUDIO_BYTES, ReadPurpose::PinnedAudio)?;
        if let Err(error) = self.descriptor.verify_audio(&audio) {
            audio.fill(0);
            return Err(error.into());
        }
        Ok(audio)
    }
}

impl Drop for PinnedRecoveryAudio {
    fn drop(&mut self) {
        #[cfg(test)]
        let _mutation = crate::mutation_trace::span("deferred-pin-cleanup");
        let release =
            (|| {
                let _ownership = self.root.lock()?;
                let mut readers = self.readers.lock().map_err(|_| {
                    NativeError::Verification("Recovery read pin lock is poisoned.")
                })?;
                let count = readers.pins.get_mut(&self.descriptor.index).ok_or(
                    NativeError::Verification("Recovery read pin ownership was lost."),
                )?;
                *count = count.checked_sub(1).ok_or(NativeError::Verification(
                    "Recovery read pin count was invalid.",
                ))?;
                let delete = if *count == 0 {
                    readers.pins.remove(&self.descriptor.index);
                    readers.deferred_delete.remove(&self.descriptor.index)
                } else {
                    false
                };
                Ok::<_, NativeError>(delete)
            })();
        match release {
            Ok(true) => {
                #[cfg(test)]
                if let Some(pause) = self.root.next_deferred_delete_pause.lock().unwrap().take() {
                    pause.wait();
                }
                if let Err(error) = self.root.remove_audio_file(&self.id, self.descriptor.index) {
                    eprintln!("Deferred confirmed audio cleanup failed: {error}");
                }
            }
            Ok(false) => {}
            Err(error) => eprintln!("Recovery read pin release failed: {error}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NativeRecoveryInfo {
    pub id: String,
    pub created_at_ticks: i64,
    pub updated_at_ticks: i64,
    pub language: String,
    pub captured_bytes: u64,
    pub pending_chunks: usize,
    pub capture_complete: bool,
    pub audit_pending: bool,
    pub error: Option<&'static str>,
}

impl NativeRecoveryInfo {
    fn readable(state: &RecoveryManifest) -> Self {
        Self {
            id: state.id.clone(),
            created_at_ticks: state.created_at_ticks,
            updated_at_ticks: state.updated_at_ticks,
            language: state.language.clone(),
            captured_bytes: state.captured_bytes,
            pending_chunks: state
                .chunks
                .iter()
                .filter(|chunk| chunk.text.is_none())
                .count(),
            capture_complete: state.capture_complete,
            audit_pending: state.audit.is_some(),
            error: None,
        }
    }

    fn unreadable(id: String) -> Self {
        Self {
            id,
            created_at_ticks: 0,
            updated_at_ticks: 0,
            language: String::new(),
            captured_bytes: 0,
            pending_chunks: 0,
            capture_complete: false,
            audit_pending: false,
            error: Some("Recovery data is unreadable or corrupt."),
        }
    }
}

impl NativeRecoveryStore {
    pub fn new(root: PathBuf, maximum_bytes: u64) -> Result<Self, NativeError> {
        if !root.is_absolute() || maximum_bytes == 0 {
            return Err(NativeError::InvalidArgument(
                "Recovery requires an absolute root and positive quota.",
            ));
        }
        guard_recovery_path(&root)?;
        std::fs::create_dir_all(&root)
            .map_err(|error| NativeError::Io("create recovery root", error))?;
        guard_recovery_path(&root)?;
        let root = std::fs::canonicalize(root)
            .map_err(|error| NativeError::Io("canonicalize recovery root", error))?;
        static ROOTS: OnceLock<Mutex<HashMap<PathBuf, Weak<RecoveryRoot>>>> = OnceLock::new();
        let mut roots = ROOTS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| NativeError::Verification("Recovery root registry lock is poisoned."))?;
        roots.retain(|_, root| root.strong_count() != 0);
        if let Some(shared) = roots.get(&root).and_then(Weak::upgrade) {
            if shared.maximum_bytes != maximum_bytes {
                return Err(NativeError::InvalidArgument(
                    "An active recovery root cannot change quota.",
                ));
            }
            return Ok(Self(shared));
        }
        let lock_path = root.join("writer.lock");
        guard_recovery_path(&lock_path)?;
        // Share mode zero is an OS-enforced lock across threads, processes and Windows sessions.
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&lock_path)
            .map_err(|error| NativeError::Io("acquire exclusive recovery writer", error))?;
        if writer
            .metadata()
            .map_err(|error| NativeError::Io("inspect recovery writer lock", error))?
            .len()
            != 0
        {
            return Err(NativeError::Verification(
                "Unexpected recovery writer lock contents; file is preserved.",
            ));
        }
        let shared = Arc::new(RecoveryRoot {
            path: root.clone(),
            maximum_bytes,
            _writer: writer,
            active: Mutex::new(HashSet::new()),
            readers: Mutex::new(HashMap::new()),
            #[cfg(test)]
            next_write_pause: Mutex::new(None),
            #[cfg(test)]
            next_pinned_read_pause: Mutex::new(None),
            #[cfg(test)]
            next_deferred_delete_pause: Mutex::new(None),
            #[cfg(test)]
            quota_entry_pause: Mutex::new(None),
            #[cfg(test)]
            manifest_read_pause: Mutex::new(None),
            #[cfg(test)]
            manifest_reads_started: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            io_probe: FixtureIoProbe {
                clock: std::time::Instant::now(),
                phase: std::sync::atomic::AtomicU8::new(0),
                phase_started_ns: std::sync::atomic::AtomicU64::new(0),
                last_phase: std::sync::atomic::AtomicU8::new(0),
            },
            #[cfg(test)]
            trace: crate::lock_trace::Lock::new("recovery-root"),
        });
        roots.insert(root, Arc::downgrade(&shared));
        Ok(Self(shared))
    }

    pub fn create(
        &self,
        language: String,
        context: String,
        streaming: bool,
        now: OffsetDateTime,
    ) -> Result<NativeRecoveryJournal, NativeError> {
        let mut active = self.0.lock()?;
        let manifest = RecoveryManifest::create(language, context, streaming, utc_ticks(now)?)?;
        let directory = self.0.session_path(&manifest.id)?;
        guard_recovery_path(&directory).map_err(|error| self.0.path_error(&directory, error))?;
        std::fs::create_dir(&directory).map_err(|error| {
            self.0.path_error(
                &directory,
                NativeError::Io("create recovery session", error),
            )
        })?;
        let cipher_bytes = self.0.write_manifest(&manifest)?;
        let readers = self.0.register_readers(&manifest, cipher_bytes as u64)?;
        active.insert(manifest.id.clone());
        Ok(NativeRecoveryJournal {
            root: self.0.clone(),
            id: manifest.id.clone(),
            deleted: false,
            readers,
        })
    }

    pub fn open(&self, id: &str) -> Result<NativeRecoveryJournal, NativeError> {
        let mut active = self.0.lock()?;
        if active.contains(id) {
            return Err(NativeError::Verification(
                "This recovery journal already has an active owner.",
            ));
        }
        if self.0.has_read_pins(id)? {
            return Err(NativeError::Verification(
                "Recovery audio is still owned by a pinned read.",
            ));
        }
        let state = self.0.load(id, true)?;
        // Retry publish-before-delete leftovers once on reopen, not on every new confirmation.
        self.0.retry_confirmed_audio(&state)?;
        let manifest_path = self.0.session_path(id)?.join("manifest.enc");
        guard_recovery_path(&manifest_path)
            .map_err(|error| self.0.path_error(&manifest_path, error))?;
        let metadata = std::fs::symlink_metadata(&manifest_path).map_err(|error| {
            self.0.path_error(
                &manifest_path,
                NativeError::Io("inspect admitted manifest size", error),
            )
        })?;
        if metadata.file_attributes() & 0x400 != 0
            || metadata.len() == 0
            || metadata.len() > (MAX_MANIFEST_BYTES + ENCRYPTION_ALLOWANCE) as u64
        {
            return Err(NativeError::Verification(
                "Admitted manifest has invalid bounds or reparse attributes.",
            ));
        }
        let readers = self.0.register_readers(&state, metadata.len())?;
        active.insert(id.into());
        Ok(NativeRecoveryJournal {
            root: self.0.clone(),
            id: id.into(),
            deleted: false,
            readers,
        })
    }

    pub fn list(&self) -> Result<Vec<NativeRecoveryInfo>, NativeError> {
        let _active = self.0.lock()?;
        self.0.list()
    }

    #[cfg(test)]
    pub(crate) fn fixture_snapshot(&self, id: &str) -> Result<RecoveryManifest, NativeError> {
        let _active = self.0.lock()?;
        self.0.load(id, true)
    }

    #[cfg(test)]
    pub(crate) fn pause_next_fixture_write(
        &self,
    ) -> (FixtureWriteRelease, tokio::sync::oneshot::Receiver<()>) {
        let (entered, observed) = tokio::sync::oneshot::channel();
        let pause = Arc::new(FixtureWritePause {
            entered: Mutex::new(Some(entered)),
            released: Mutex::new(false),
            changed: std::sync::Condvar::new(),
        });
        assert!(
            self.0
                .next_write_pause
                .lock()
                .unwrap()
                .replace(pause.clone())
                .is_none()
        );
        (FixtureWriteRelease(pause), observed)
    }

    #[cfg(test)]
    pub(crate) fn pause_next_pinned_read(
        &self,
    ) -> (FixtureWriteRelease, tokio::sync::oneshot::Receiver<()>) {
        let (entered, observed) = tokio::sync::oneshot::channel();
        let pause = Arc::new(FixtureWritePause {
            entered: Mutex::new(Some(entered)),
            released: Mutex::new(false),
            changed: std::sync::Condvar::new(),
        });
        assert!(
            self.0
                .next_pinned_read_pause
                .lock()
                .unwrap()
                .replace(pause.clone())
                .is_none()
        );
        (FixtureWriteRelease(pause), observed)
    }

    #[cfg(test)]
    pub(crate) fn pause_next_manifest_read(
        &self,
        phase: ManifestReadPhase,
    ) -> (FixtureWriteRelease, tokio::sync::oneshot::Receiver<()>) {
        let (entered, observed) = tokio::sync::oneshot::channel();
        let pause = Arc::new(FixtureWritePause {
            entered: Mutex::new(Some(entered)),
            released: Mutex::new(false),
            changed: std::sync::Condvar::new(),
        });
        assert!(
            self.0
                .manifest_read_pause
                .lock()
                .unwrap()
                .replace((phase, pause.clone()))
                .is_none()
        );
        (FixtureWriteRelease(pause), observed)
    }

    #[cfg(test)]
    pub(crate) fn fixture_pin_manifest(&self, id: &str) -> Result<ManifestReadPin, NativeError> {
        let readers = self.0.reader_state(id)?.ok_or(NativeError::Verification(
            "Fixture fresh inspection requires an active journal.",
        ))?;
        ManifestReadPin::new(self.0.clone(), id.into(), readers)
    }

    #[cfg(test)]
    pub(crate) fn fixture_manifest_read_count(&self) -> usize {
        self.0
            .manifest_reads_started
            .load(std::sync::atomic::Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn fixture_io_progress(&self) -> serde_json::Value {
        use std::sync::atomic::Ordering;
        let probe = &self.0.io_probe;
        let phase = probe.phase.load(Ordering::Acquire);
        let name = |phase| match phase {
            0 => "idle",
            1 => "protect",
            2 => "validate-target-path",
            3 => "atomic-write",
            4 => "fixture-pause",
            5 => "read-encrypted",
            6 => "unprotect",
            7 => "remove-confirmed-audio",
            8 => "publishing-engine-progress",
            9 => "quota-scan",
            10 => "release-manifest-reader",
            _ => "invalid-phase",
        };
        let elapsed = (probe.clock.elapsed().as_nanos() as u64)
            .saturating_sub(probe.phase_started_ns.load(Ordering::Acquire));
        let pinned_reads = self.0.readers.try_lock().ok().and_then(|registry| {
            let mut pins = 0usize;
            let mut manifests = 0usize;
            let mut retained = 0u64;
            for reader in registry.values().filter_map(Weak::upgrade) {
                let reader = reader.try_lock().ok()?;
                pins += reader.pins.values().sum::<usize>();
                manifests += usize::from(reader.manifest_pin);
                retained += reader.manifest_pin_bytes;
            }
            Some((pins, manifests, retained))
        });
        serde_json::json!({
            "phase":name(phase),"active_ms":if phase==0{None}else{Some(elapsed as f64/1_000_000.0)},
            "last_phase":name(probe.last_phase.load(Ordering::Acquire)),
            "root_lock":self.0.trace.snapshot(),"active_pinned_reads":pinned_reads.map(|counts|counts.0),
            "active_manifest_inspections":pinned_reads.map(|counts|counts.1),
            "retained_manifest_capacity_bytes":pinned_reads.map(|counts|counts.2),
            "phase_scope":"root-serialized IO; unlocked manifest inspections excluded",
            "pin_snapshot_unavailable":pinned_reads.is_none()
        })
    }

    pub fn delete_idle(&self, id: &str) -> Result<(), NativeError> {
        let active = self.0.lock()?;
        if active.contains(id) {
            return Err(NativeError::Verification(
                "Active recovery journals cannot be deleted by another caller.",
            ));
        }
        self.0.delete_flat(id)
    }

    pub fn cleanup(&self, now: OffsetDateTime) -> Result<usize, NativeError> {
        let active = self.0.lock()?;
        let now = utc_ticks(now)?;
        let mut removed = 0;
        for entry in self.0.list()? {
            if entry.error.is_none()
                && !active.contains(&entry.id)
                && now - entry.updated_at_ticks > RETENTION_TICKS
            {
                self.0.delete_flat(&entry.id)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

impl RecoveryRoot {
    fn path_error(&self, path: &Path, source: NativeError) -> NativeError {
        let relative_path = path
            .strip_prefix(&self.path)
            .ok()
            .map(|relative| {
                relative
                    .components()
                    .map(|component| {
                        let name = component.as_os_str().to_string_lossy();
                        let audio = name.strip_suffix(".wav.enc").is_some_and(|index| {
                            index.len() == 8 && index.bytes().all(|byte| byte.is_ascii_digit())
                        });
                        if valid_id(&name)
                            || matches!(name.as_ref(), "manifest.enc" | "writer.lock")
                            || audio
                        {
                            name.into_owned()
                        } else {
                            "<other-entry>".into()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\\")
            })
            .filter(|relative| !relative.is_empty())
            .unwrap_or_else(|| "<root>".into());
        NativeError::RecoveryPath {
            relative_path,
            source: Box::new(source),
        }
    }

    fn register_readers(
        &self,
        state: &RecoveryManifest,
        cipher_bytes: u64,
    ) -> Result<Arc<Mutex<ReaderState>>, NativeError> {
        let readers = Arc::new(Mutex::new(ReaderState::new(state, cipher_bytes)));
        let mut registry = self
            .readers
            .lock()
            .map_err(|_| NativeError::Verification("Recovery reader registry is poisoned."))?;
        registry.retain(|_, readers| readers.strong_count() != 0);
        registry.insert(state.id.clone(), Arc::downgrade(&readers));
        Ok(readers)
    }

    fn reader_state(&self, id: &str) -> Result<Option<Arc<Mutex<ReaderState>>>, NativeError> {
        let registry = self
            .readers
            .lock()
            .map_err(|_| NativeError::Verification("Recovery reader registry is poisoned."))?;
        Ok(registry.get(id).and_then(Weak::upgrade))
    }

    fn has_read_pins(&self, id: &str) -> Result<bool, NativeError> {
        let Some(readers) = self.reader_state(id)? else {
            return Ok(false);
        };
        let readers = readers
            .lock()
            .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
        Ok(!readers.pins.is_empty() || readers.manifest_pin)
    }

    #[track_caller]
    fn lock(&self) -> Result<MutexGuard<'_, HashSet<String>>, NativeError> {
        #[cfg(test)]
        let mutation_wait = crate::mutation_trace::span("root-lock-wait");
        #[cfg(test)]
        let wait = self
            .trace
            .wait(&format!("{}", std::panic::Location::caller()));
        let mut guard = self
            .active
            .lock()
            .map_err(|_| NativeError::Verification("Recovery journal lock is poisoned."))?;
        {
            let registry = self
                .readers
                .lock()
                .map_err(|_| NativeError::Verification("Recovery reader registry is poisoned."))?;
            let mut released = Vec::new();
            for id in guard.iter() {
                let live = match registry.get(id).and_then(Weak::upgrade) {
                    Some(reader) => {
                        let state = reader.lock().map_err(|_| {
                            NativeError::Verification("Recovery read pin lock is poisoned.")
                        })?;
                        state.owner_live
                    }
                    None => false,
                };
                if !live {
                    released.push(id.clone());
                }
            }
            for id in released {
                guard.remove(&id);
            }
        }
        #[cfg(test)]
        drop(mutation_wait);
        #[cfg(test)]
        return Ok(wait.acquired(guard));
        #[cfg(not(test))]
        Ok(guard)
    }

    fn session_path(&self, id: &str) -> Result<PathBuf, NativeError> {
        if !valid_id(id) {
            return Err(NativeError::InvalidArgument(
                "Recovery ID must be a lowercase GUID in N format.",
            ));
        }
        // I/O entry points validate the full path; constructing it must not duplicate that walk.
        Ok(self.path.join(id))
    }

    fn audio_path(&self, id: &str, index: u32) -> Result<PathBuf, NativeError> {
        Ok(self.session_path(id)?.join(format!("{index:08}.wav.enc")))
    }

    fn load(&self, id: &str, inspect_audio: bool) -> Result<RecoveryManifest, NativeError> {
        let state = self.read_manifest(id, false)?;
        if inspect_audio {
            for chunk in state.chunks.iter().filter(|chunk| chunk.text.is_none()) {
                let mut audio =
                    self.read_encrypted(&self.audio_path(id, chunk.index)?, MAX_AUDIO_BYTES)?;
                let valid = chunk.verify_audio(&audio);
                audio.fill(0);
                valid?;
            }
        }
        Ok(state)
    }

    fn read_manifest(&self, id: &str, inspection: bool) -> Result<RecoveryManifest, NativeError> {
        let path = self.session_path(id)?.join("manifest.enc");
        self.read_manifest_path(id, &path, inspection)
    }

    fn read_manifest_path(
        &self,
        id: &str,
        path: &Path,
        inspection: bool,
    ) -> Result<RecoveryManifest, NativeError> {
        #[cfg(test)]
        self.manifest_reads_started
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let mut plain = self
            .read_encrypted_inner(path, MAX_MANIFEST_BYTES, inspection)
            .map_err(|error| self.path_error(path, error))?;
        let parsed = RecoveryManifest::parse(&plain, id);
        plain.fill(0);
        parsed.map_err(NativeError::Protocol)
    }

    fn list(&self) -> Result<Vec<NativeRecoveryInfo>, NativeError> {
        guard_recovery_path(&self.path).map_err(|error| self.path_error(&self.path, error))?;
        let entries = std::fs::read_dir(&self.path).map_err(|error| {
            self.path_error(&self.path, NativeError::Io("list recovery root", error))
        })?;
        let mut result = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                self.path_error(
                    &self.path,
                    NativeError::Io("read recovery directory entry", error),
                )
            })?;
            let name = entry.file_name();
            let Some(id) = name.to_str().filter(|id| valid_id(id)) else {
                continue;
            };
            let metadata = std::fs::symlink_metadata(entry.path()).map_err(|error| {
                self.path_error(
                    &entry.path(),
                    NativeError::Io("inspect recovery entry", error),
                )
            })?;
            if metadata.file_attributes() & 0x400 != 0 {
                eprintln!("Recovery reparse entry retained for explicit inspection.");
                result.push(NativeRecoveryInfo::unreadable(id.into()));
                continue;
            }
            if !metadata.is_dir() {
                continue;
            }
            if let Some(readers) = self.reader_state(id)? {
                let readers = readers.lock().map_err(|_| {
                    NativeError::Verification("Recovery read pin lock is poisoned.")
                })?;
                if readers.owner_live {
                    result.push(if readers.publication_valid {
                        NativeRecoveryInfo::readable(&readers.published)
                    } else {
                        NativeRecoveryInfo::unreadable(id.into())
                    });
                    continue;
                }
            }
            match self.load(id, true) {
                Ok(state) => result.push(NativeRecoveryInfo::readable(&state)),
                Err(error @ (NativeError::InvalidArgument(_) | NativeError::AlreadyRunning)) => {
                    return Err(error);
                }
                Err(error) => {
                    eprintln!("Recovery entry could not be inspected: {error}");
                    result.push(NativeRecoveryInfo::unreadable(id.into()));
                }
            }
        }
        Ok(result)
    }

    fn ensure_quota(&self, additional: usize) -> Result<(), NativeError> {
        #[cfg(test)]
        let mut mutation = crate::mutation_trace::span("quota-scan");
        let retained = {
            let readers: Vec<_> = self
                .readers
                .lock()
                .map_err(|_| NativeError::Verification("Recovery reader registry is poisoned."))?
                .values()
                .filter_map(Weak::upgrade)
                .collect();
            let mut retained = 0u64;
            for reader in readers {
                let reader = reader.lock().map_err(|_| {
                    NativeError::Verification("Recovery read pin lock is poisoned.")
                })?;
                retained = retained.checked_add(reader.manifest_pin_bytes).ok_or(
                    NativeError::Verification("Recovery retained manifest charge overflowed."),
                )?;
            }
            retained
        };
        #[cfg(test)]
        let mut charged_bytes = retained;
        #[cfg(test)]
        let mut metadata_probes = 0;
        let mut remaining = self
            .maximum_bytes
            .checked_sub(additional as u64)
            .and_then(|remaining| remaining.checked_sub(retained))
            .ok_or(NativeError::Verification(
                "Recovery storage quota exceeded.",
            ))?;
        let mut directories = vec![self.path.clone()];
        let mut count = 0usize;
        while let Some(directory) = directories.pop() {
            guard_recovery_path(&directory).map_err(|error| self.path_error(&directory, error))?;
            for entry in std::fs::read_dir(&directory).map_err(|error| {
                self.path_error(
                    &directory,
                    NativeError::Io("enumerate recovery quota", error),
                )
            })? {
                let entry = entry.map_err(|error| {
                    self.path_error(
                        &directory,
                        NativeError::Io("read recovery quota entry", error),
                    )
                })?;
                count += 1;
                if count > 1_000_000 {
                    return Err(NativeError::Verification(
                        "Recovery storage has too many entries.",
                    ));
                }
                #[cfg(test)]
                {
                    let pause = {
                        let mut next = self.quota_entry_pause.lock().unwrap();
                        if next.as_ref().is_some_and(|(path, _)| *path == entry.path()) {
                            next.take().map(|(_, pause)| pause)
                        } else {
                            None
                        }
                    };
                    if let Some(pause) = pause {
                        pause.wait();
                    }
                }
                let metadata = entry.metadata().map_err(|error| {
                    self.path_error(
                        &entry.path(),
                        NativeError::Io("inspect recovery quota entry", error),
                    )
                })?;
                #[cfg(test)]
                {
                    metadata_probes += 1;
                    if !metadata.is_dir() {
                        charged_bytes += metadata.len();
                    }
                    if let Some(mutation) = &mut mutation {
                        mutation.counts(count as u64, metadata_probes, charged_bytes);
                    }
                }
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(NativeError::Verification(
                        "Recovery quota refuses reparse points.",
                    ));
                }
                if metadata.is_dir() {
                    directories.push(entry.path());
                } else {
                    remaining =
                        remaining
                            .checked_sub(metadata.len())
                            .ok_or(NativeError::Verification(
                                "Recovery storage quota exceeded.",
                            ))?;
                }
            }
        }
        Ok(())
    }

    fn write_encrypted(
        &self,
        path: &Path,
        plain: &[u8],
        limit: usize,
    ) -> Result<usize, NativeError> {
        self.write_encrypted_inner(path, plain, limit)
            .map_err(|error| self.path_error(path, error))
    }

    fn write_encrypted_inner(
        &self,
        path: &Path,
        plain: &[u8],
        limit: usize,
    ) -> Result<usize, NativeError> {
        #[cfg(test)]
        let _probe = {
            self.io_probe.enter(1);
            FixtureIoEnd(&self.io_probe)
        };
        #[cfg(test)]
        {
            let pause = self.next_write_pause.lock().unwrap().take();
            if let Some(pause) = pause {
                self.io_probe.enter(4);
                if let Some(entered) = pause.entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                let mut released = pause.released.lock().unwrap();
                while !*released {
                    released = pause.changed.wait(released).unwrap();
                }
            }
        }
        if plain.len() > limit {
            return Err(NativeError::Verification(
                "Recovery plaintext exceeds bounds.",
            ));
        }
        #[cfg(test)]
        self.io_probe.enter(1);
        let cipher = protect(plain)?;
        if cipher.is_empty() || cipher.len() > limit + ENCRYPTION_ALLOWANCE {
            return Err(NativeError::Verification(
                "Encrypted recovery payload exceeds bounds.",
            ));
        }
        #[cfg(test)]
        self.io_probe.enter(2);
        guard_recovery_path(path)?;
        if path.file_name().is_some_and(|name| name == "manifest.enc") {
            if let Some(id) = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
            {
                if let Some(readers) = self.reader_state(id)? {
                    let mut readers = readers.lock().map_err(|_| {
                        NativeError::Verification("Recovery read pin lock is poisoned.")
                    })?;
                    if readers.manifest_pin {
                        readers.manifest_pin_bytes =
                            readers.manifest_pin_bytes.max(cipher.len() as u64);
                    }
                }
            }
        }
        // The previous target and any orphan/staging files stay charged during replacement.
        #[cfg(test)]
        self.io_probe.enter(9);
        self.ensure_quota(cipher.len())?;
        #[cfg(test)]
        self.io_probe.enter(3);
        atomic_write(path, &cipher, WriteMode::Replace)?;
        Ok(cipher.len())
    }

    fn write_manifest(&self, state: &RecoveryManifest) -> Result<usize, NativeError> {
        let path = self.session_path(&state.id)?.join("manifest.enc");
        let mut plain = {
            #[cfg(test)]
            let _mutation = crate::mutation_trace::span("manifest-encode");
            state.encode()?
        };
        let result = self.write_encrypted(&path, &plain, MAX_MANIFEST_BYTES);
        plain.fill(0);
        result
    }

    fn read_encrypted(&self, path: &Path, limit: usize) -> Result<Vec<u8>, NativeError> {
        self.read_encrypted_inner(path, limit, false)
            .map_err(|error| self.path_error(path, error))
    }

    fn read_encrypted_inner(
        &self,
        path: &Path,
        limit: usize,
        inspection: bool,
    ) -> Result<Vec<u8>, NativeError> {
        #[cfg(test)]
        let _mutation = crate::mutation_trace::span("encrypted-read");
        #[cfg(test)]
        let _probe = if inspection {
            None
        } else {
            self.io_probe.enter(5);
            Some(FixtureIoEnd(&self.io_probe))
        };
        guard_recovery_path(path)?;
        #[cfg(test)]
        if inspection || path.file_name().is_some_and(|name| name == "manifest.enc") {
            self.fixture_manifest_pause(ManifestReadPhase::Open);
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT.0);
        if inspection {
            options.share_mode(windows::Win32::Storage::FileSystem::FILE_SHARE_READ.0);
        }
        let file = options
            .open(path)
            .map_err(|error| NativeError::Io("open encrypted recovery file", error))?;
        let purpose = if inspection {
            ReadPurpose::InspectedManifest
        } else if path.file_name().is_some_and(|name| name == "manifest.enc") {
            ReadPurpose::Manifest
        } else {
            ReadPurpose::Other
        };
        self.read_open_file(file, limit, purpose)
    }

    #[cfg(test)]
    fn fixture_manifest_pause(&self, phase: ManifestReadPhase) {
        let pause = {
            let mut next = self.manifest_read_pause.lock().unwrap();
            if next
                .as_ref()
                .is_some_and(|(selected, _)| *selected == phase)
            {
                next.take().map(|(_, pause)| pause)
            } else {
                None
            }
        };
        if let Some(pause) = pause {
            pause.wait();
        }
    }

    fn read_open_file(
        &self,
        file: File,
        limit: usize,
        purpose: ReadPurpose,
    ) -> Result<Vec<u8>, NativeError> {
        #[cfg(not(test))]
        let _ = purpose;
        let metadata = file
            .metadata()
            .map_err(|error| NativeError::Io("inspect encrypted recovery file", error))?;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(NativeError::Verification(
                "Encrypted recovery handles refuse reparse points.",
            ));
        }
        let length = metadata.len();
        if length == 0
            || length > (limit + ENCRYPTION_ALLOWANCE) as u64
            || length > self.maximum_bytes
        {
            return Err(NativeError::Verification(
                "Encrypted recovery file exceeds supported bounds.",
            ));
        }
        #[cfg(test)]
        if matches!(purpose, ReadPurpose::PinnedAudio) {
            let pause = self.next_pinned_read_pause.lock().unwrap().take();
            if let Some(pause) = pause {
                if let Some(entered) = pause.entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                let mut released = pause.released.lock().unwrap();
                while !*released {
                    released = pause.changed.wait(released).unwrap();
                }
            }
        }
        #[cfg(test)]
        if matches!(
            purpose,
            ReadPurpose::Manifest | ReadPurpose::InspectedManifest
        ) {
            self.fixture_manifest_pause(ManifestReadPhase::Read);
        }
        let mut cipher = Vec::new();
        {
            #[cfg(test)]
            let _mutation = crate::mutation_trace::span("read-cipher-bytes");
            file.take((limit + ENCRYPTION_ALLOWANCE + 1) as u64)
                .read_to_end(&mut cipher)
                .map_err(|error| NativeError::Io("read encrypted recovery file", error))?;
        }
        if cipher.len() > limit + ENCRYPTION_ALLOWANCE {
            return Err(NativeError::Verification(
                "Encrypted recovery file exceeds supported bounds.",
            ));
        }
        #[cfg(test)]
        if matches!(purpose, ReadPurpose::Manifest | ReadPurpose::Other) {
            self.io_probe.enter(6);
        }
        #[cfg(test)]
        if matches!(
            purpose,
            ReadPurpose::Manifest | ReadPurpose::InspectedManifest
        ) {
            self.fixture_manifest_pause(ManifestReadPhase::Unprotect);
        }
        let mut plain = unprotect(&cipher)?;
        if plain.len() > limit {
            plain.fill(0);
            return Err(NativeError::Verification(
                "Recovery plaintext exceeds supported bounds.",
            ));
        }
        Ok(plain)
    }

    fn remove_audio(&self, id: &str, index: u32) -> Result<(), NativeError> {
        #[cfg(test)]
        let _mutation = crate::mutation_trace::span("confirmed-or-orphan-cleanup");
        if let Some(readers) = self.reader_state(id)? {
            let mut readers = readers
                .lock()
                .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
            if readers.pins.contains_key(&index) {
                readers.deferred_delete.insert(index);
                return Ok(());
            }
        }
        self.remove_audio_file(id, index)
    }

    fn remove_audio_file(&self, id: &str, index: u32) -> Result<(), NativeError> {
        #[cfg(test)]
        let _mutation = crate::mutation_trace::span("delete-audio-file");
        #[cfg(test)]
        let _probe = {
            self.io_probe.enter(7);
            FixtureIoEnd(&self.io_probe)
        };
        let path = self.audio_path(id, index)?;
        guard_recovery_path(&path).map_err(|error| self.path_error(&path, error))?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(self.path_error(
                &path,
                NativeError::Io("delete confirmed recovery audio", error),
            )),
        }
    }

    fn retry_confirmed_audio(&self, state: &RecoveryManifest) -> Result<(), NativeError> {
        for chunk in state.chunks.iter().filter(|chunk| chunk.text.is_some()) {
            self.remove_audio(&state.id, chunk.index)?;
        }
        Ok(())
    }

    fn delete_flat(&self, id: &str) -> Result<(), NativeError> {
        if self.has_read_pins(id)? {
            return Err(NativeError::Verification(
                "Recovery deletion cannot invalidate a pinned audio read.",
            ));
        }
        let directory = self.session_path(id)?;
        guard_recovery_path(&directory).map_err(|error| self.path_error(&directory, error))?;
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(self.path_error(
                    &directory,
                    NativeError::Io("list recovery deletion scope", error),
                ));
            }
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| {
                    self.path_error(
                        &directory,
                        NativeError::Io("inspect recovery deletion scope", error),
                    )
                })?
                .path();
            guard_recovery_path(&path).map_err(|error| self.path_error(&path, error))?;
            if std::fs::symlink_metadata(&path)
                .map_err(|error| {
                    self.path_error(
                        &path,
                        NativeError::Io("inspect recovery deletion file", error),
                    )
                })?
                .is_dir()
            {
                return Err(NativeError::Verification(
                    "Unexpected nested recovery directory; no files deleted.",
                ));
            }
            files.push(path);
        }
        for file in files {
            guard_recovery_path(&file).map_err(|error| self.path_error(&file, error))?;
            std::fs::remove_file(&file).map_err(|error| {
                self.path_error(&file, NativeError::Io("delete scoped recovery file", error))
            })?;
        }
        guard_recovery_path(&directory).map_err(|error| self.path_error(&directory, error))?;
        std::fs::remove_dir(&directory).map_err(|error| {
            self.path_error(
                &directory,
                NativeError::Io("delete empty recovery session", error),
            )
        })
    }
}

impl NativeRecoveryJournal {
    fn load_current(&self) -> Result<RecoveryManifest, NativeError> {
        let state = self.root.load(&self.id, false)?;
        let readers = self
            .readers
            .lock()
            .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
        if state.context != readers.context {
            return Err(NativeError::Verification(
                "Recovery journal no longer matches its original binding.",
            ));
        }
        if !readers.owner_live || state != *readers.published {
            return Err(NativeError::Verification(
                "Recovery changed outside its journal writer.",
            ));
        }
        Ok(state)
    }

    pub(crate) fn published_snapshot(&self) -> Result<RecoveryManifest, NativeError> {
        #[cfg(test)]
        let _view = crate::mutation_trace::span("own-durable-publication-view");
        self.ensure_open()?;
        if self.root.active.is_poisoned() || self.root.readers.is_poisoned() {
            return Err(NativeError::Verification(
                "Recovery root writer ownership is unavailable.",
            ));
        }
        let published = {
            let readers = self
                .readers
                .lock()
                .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
            if !readers.owner_live || !readers.publication_valid {
                return Err(NativeError::Verification(
                    "Recovery publication is unavailable without its successful writer ownership.",
                ));
            }
            readers.published.clone()
        };
        Ok(published.as_ref().clone())
    }

    fn invalidate_publication(&self) {
        match self.readers.lock() {
            Ok(mut readers) => readers.publication_valid = false,
            Err(_) => eprintln!("Recovery publication invalidation failed: poisoned lock."),
        }
    }

    fn mutate<T>(
        &self,
        source: ManifestSource,
        mutation: impl FnOnce(RecoveryManifest) -> Result<T, NativeError>,
    ) -> Result<T, NativeError> {
        let active = match self.root.lock() {
            Ok(active) => active,
            Err(error) => {
                self.invalidate_publication();
                return Err(error);
            }
        };
        let result = (|| {
            self.ensure_open()?;
            if !active.contains(&self.id) {
                return Err(NativeError::Verification(
                    "Recovery journal writer ownership was lost.",
                ));
            }
            if self
                .readers
                .lock()
                .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?
                .generation
                == u64::MAX
            {
                return Err(NativeError::Verification(
                    "Recovery publication generation exhausted.",
                ));
            }
            let current = match source {
                ManifestSource::Fresh => self.load_current()?,
                ManifestSource::Published => self.published_snapshot()?,
            };
            mutation(current)
        })();
        if result.is_err() {
            self.invalidate_publication();
        }
        result
    }

    fn publish_readers(
        &self,
        state: &RecoveryManifest,
        cipher_bytes: usize,
    ) -> Result<(), NativeError> {
        #[cfg(test)]
        let _mutation = crate::mutation_trace::span("manifest-reader-publication");
        let mut readers = self
            .readers
            .lock()
            .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
        if !readers.owner_live || readers.context != state.context {
            return Err(NativeError::Verification(
                "Recovery read binding changed after publication.",
            ));
        }
        readers.publish(state, cipher_bytes as u64);
        Ok(())
    }

    pub(crate) fn pin_audio(
        &self,
        expected: &RecoveryChunk,
        context: &str,
    ) -> Result<PinnedRecoveryAudio, NativeError> {
        self.pin_audio_from(expected, context, ManifestSource::Published)
    }

    fn pin_audio_from(
        &self,
        expected: &RecoveryChunk,
        context: &str,
        source: ManifestSource,
    ) -> Result<PinnedRecoveryAudio, NativeError> {
        // The journal owns this immutable descriptor cache; pin admission never waits on disk I/O.
        self.ensure_open()?;
        let mut readers = self
            .readers
            .lock()
            .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
        if !readers.owner_live
            || matches!(source, ManifestSource::Published) && !readers.publication_valid
            || readers.context != context
        {
            return Err(NativeError::Verification(
                "Pinned recovery read requires the original binding.",
            ));
        }
        let published = readers
            .available
            .get(&expected.index)
            .ok_or(NativeError::Verification(
                "Recovery audio is no longer unconfirmed.",
            ))?;
        if expected.audio_bytes != published.audio_bytes
            || expected.end_byte != published.end_byte
            || expected.digest != published.digest
            || expected.overlaps_previous != published.overlaps_previous
            || expected.text.is_some()
        {
            return Err(NativeError::Verification(
                "Recovery read descriptor does not match immutable audio.",
            ));
        }
        let descriptor = published.clone();
        let pins = readers.pins.entry(expected.index).or_default();
        *pins = pins.checked_add(1).ok_or(NativeError::Verification(
            "Recovery read pin count overflowed.",
        ))?;
        Ok(PinnedRecoveryAudio {
            root: self.root.clone(),
            readers: self.readers.clone(),
            id: self.id.clone(),
            descriptor,
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture_publish_progress(&self) -> FixtureIoEnd<'_> {
        self.root.io_probe.enter(8);
        FixtureIoEnd(&self.root.io_probe)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn ensure_open(&self) -> Result<(), NativeError> {
        if self.deleted {
            return Err(NativeError::Verification(
                "This recovery journal is deleted.",
            ));
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Result<RecoveryManifest, NativeError> {
        self.ensure_open()?;
        ManifestReadPin::new(self.root.clone(), self.id.clone(), self.readers.clone())?.read()
    }

    pub fn checkpoint(
        &self,
        tail: &[u8],
        captured: u64,
        added: &[AudioCheckpoint<'_>],
        complete: bool,
        now: OffsetDateTime,
    ) -> Result<(), NativeError> {
        self.checkpoint_with(tail, captured, added, complete, now, ManifestSource::Fresh)
            .map(|_| ())
    }

    pub(crate) fn checkpoint_snapshot(
        &self,
        tail: &[u8],
        captured: u64,
        added: &[AudioCheckpoint<'_>],
        complete: bool,
        now: OffsetDateTime,
    ) -> Result<RecoveryManifest, NativeError> {
        self.checkpoint_with(
            tail,
            captured,
            added,
            complete,
            now,
            ManifestSource::Published,
        )
    }

    fn checkpoint_with(
        &self,
        tail: &[u8],
        captured: u64,
        added: &[AudioCheckpoint<'_>],
        complete: bool,
        now: OffsetDateTime,
        source: ManifestSource,
    ) -> Result<RecoveryManifest, NativeError> {
        #[cfg(test)]
        let mut mutation = crate::mutation_trace::span("journal-checkpoint");
        #[cfg(test)]
        if let Some(mutation) = &mut mutation {
            mutation.counts(added.len() as u64, 0, captured);
        }
        self.mutate(source, |current| {
            let next = {
                #[cfg(test)]
                let _mutation = crate::mutation_trace::span("checkpoint-clone-hash-validate");
                current.checkpoint(tail, captured, added, complete, utc_ticks(now)?)?
            };
            // Check total manifest bounds before touching any audio. Still publish audio first.
            let mut encoded = {
                #[cfg(test)]
                let _mutation = crate::mutation_trace::span("manifest-size-preflight-encode");
                next.encode()?
            };
            let result = (|| {
                for (offset, audio) in added.iter().enumerate() {
                    let index = (current.chunks.len() + offset) as u32;
                    self.root.remove_audio(&self.id, index)?;
                    self.root.write_encrypted(
                        &self.root.audio_path(&self.id, index)?,
                        audio.wav,
                        MAX_AUDIO_BYTES,
                    )?;
                }
                let cipher_bytes = self.root.write_encrypted(
                    &self.root.session_path(&self.id)?.join("manifest.enc"),
                    &encoded,
                    MAX_MANIFEST_BYTES,
                )?;
                self.publish_readers(&next, cipher_bytes)?;
                Ok(next)
            })();
            encoded.fill(0);
            result
        })
    }

    pub fn read_audio(&self, index: u32) -> Result<Vec<u8>, NativeError> {
        let pin = {
            let state = self.snapshot()?;
            let chunk = state
                .chunks
                .get(index as usize)
                .ok_or(NativeError::InvalidArgument("Unknown recovery chunk."))?;
            self.pin_audio_from(chunk, &state.context, ManifestSource::Fresh)?
        };
        pin.read()
    }

    pub fn save_result(
        &self,
        index: u32,
        text: String,
        now: OffsetDateTime,
    ) -> Result<(), NativeError> {
        self.save_result_with(index, text, now, ManifestSource::Fresh)
            .map(|_| ())
    }

    pub(crate) fn save_result_snapshot(
        &self,
        index: u32,
        text: String,
        now: OffsetDateTime,
    ) -> Result<RecoveryManifest, NativeError> {
        self.save_result_with(index, text, now, ManifestSource::Published)
    }

    fn save_result_with(
        &self,
        index: u32,
        text: String,
        now: OffsetDateTime,
        source: ManifestSource,
    ) -> Result<RecoveryManifest, NativeError> {
        self.mutate(source, |mut state| {
            if state.commit_legacy_result(index, text, utc_ticks(now)?)? {
                let cipher_bytes = self.root.write_manifest(&state)?;
                self.publish_readers(&state, cipher_bytes)?;
            }
            self.root.remove_audio(&self.id, index)?;
            Ok(state)
        })
    }

    pub fn confirm_streaming(
        &self,
        through: u64,
        text: String,
        context_audio: Vec<u8>,
        fallback: bool,
        segment_words: Option<u32>,
        now: OffsetDateTime,
    ) -> Result<(), NativeError> {
        self.confirm_streaming_with(
            through,
            text,
            context_audio,
            fallback,
            segment_words,
            now,
            ManifestSource::Fresh,
        )
        .map(|_| ())
    }

    pub(crate) fn confirm_streaming_snapshot(
        &self,
        through: u64,
        text: String,
        context_audio: Vec<u8>,
        fallback: bool,
        segment_words: Option<u32>,
        now: OffsetDateTime,
    ) -> Result<RecoveryManifest, NativeError> {
        self.confirm_streaming_with(
            through,
            text,
            context_audio,
            fallback,
            segment_words,
            now,
            ManifestSource::Published,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn confirm_streaming_with(
        &self,
        through: u64,
        text: String,
        context_audio: Vec<u8>,
        fallback: bool,
        segment_words: Option<u32>,
        now: OffsetDateTime,
        source: ManifestSource,
    ) -> Result<RecoveryManifest, NativeError> {
        self.mutate(source, |mut state| {
            let retry_deletions = through == state.confirmed_bytes;
            let released = state.confirm(
                through,
                text,
                context_audio,
                fallback,
                segment_words,
                utc_ticks(now)?,
            )?;
            let cipher_bytes = self.root.write_manifest(&state)?;
            self.publish_readers(&state, cipher_bytes)?;
            if retry_deletions {
                self.root.retry_confirmed_audio(&state)?;
            } else {
                for index in released {
                    self.root.remove_audio(&self.id, index)?;
                }
            }
            Ok(state)
        })
    }

    pub fn prepare_audit(
        &self,
        text: String,
        completed_at: OffsetDateTime,
        polished: bool,
        now: OffsetDateTime,
    ) -> Result<(), NativeError> {
        self.prepare_audit_with(text, completed_at, polished, now, ManifestSource::Fresh)
            .map(|_| ())
    }

    pub(crate) fn prepare_audit_snapshot(
        &self,
        text: String,
        completed_at: OffsetDateTime,
        polished: bool,
        now: OffsetDateTime,
    ) -> Result<RecoveryManifest, NativeError> {
        self.prepare_audit_with(text, completed_at, polished, now, ManifestSource::Published)
    }

    fn prepare_audit_with(
        &self,
        text: String,
        completed_at: OffsetDateTime,
        polished: bool,
        now: OffsetDateTime,
        source: ManifestSource,
    ) -> Result<RecoveryManifest, NativeError> {
        self.mutate(source, |mut state| {
            state.freeze_audit(text, utc_ticks(completed_at)?, polished, utc_ticks(now)?)?;
            let cipher_bytes = self.root.write_manifest(&state)?;
            self.publish_readers(&state, cipher_bytes)?;
            Ok(state)
        })
    }

    pub fn delete(&mut self) -> Result<(), NativeError> {
        let mut active = self.root.lock()?;
        self.ensure_open()?;
        {
            let mut readers = self
                .readers
                .lock()
                .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
            if readers.manifest_pin || !readers.pins.is_empty() {
                return Err(NativeError::Verification(
                    "Recovery deletion cannot invalidate a pinned read.",
                ));
            }
            readers.owner_live = false;
            readers.publication_valid = false;
        }
        if let Err(error) = self.root.delete_flat(&self.id) {
            match self.readers.lock() {
                Ok(mut readers) => readers.owner_live = true,
                Err(_) => {
                    eprintln!("Failed deletion could not restore journal ownership: poisoned lock.")
                }
            }
            return Err(error);
        }
        active.remove(&self.id);
        self.deleted = true;
        Ok(())
    }
}

impl Drop for NativeRecoveryJournal {
    fn drop(&mut self) {
        if !self.deleted {
            match self.readers.lock() {
                Ok(mut readers) => {
                    readers.owner_live = false;
                    readers.publication_valid = false;
                }
                Err(_) => eprintln!("Recovery publication owner release failed: poisoned lock."),
            }
            // Root admission prunes this revoked lease under its own mutex.
        }
    }
}

#[cfg(test)]
#[path = "recovery_store_tests.rs"]
pub(crate) mod tests;

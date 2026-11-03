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
pub(crate) struct FixtureWriteRelease(Arc<FixtureWritePause>);

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
    available: HashMap<u32, RecoveryChunk>,
    pins: HashMap<u32, usize>,
    deferred_delete: HashSet<u32>,
}

impl ReaderState {
    fn new(state: &RecoveryManifest) -> Self {
        Self {
            context: state.context.clone(),
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

    fn publish(&mut self, state: &RecoveryManifest) {
        self.available = state
            .chunks
            .iter()
            .filter(|chunk| chunk.text.is_none())
            .map(|chunk| (chunk.index, chunk.clone()))
            .collect();
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
        use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
        let path = self.root.audio_path(&self.id, self.descriptor.index)?;
        guard_recovery_path(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(&path)
            .map_err(|error| NativeError::Io("open pinned recovery audio", error))?;
        let metadata = file
            .metadata()
            .map_err(|error| NativeError::Io("inspect pinned recovery audio", error))?;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(NativeError::Verification(
                "Pinned recovery audio refuses reparse points.",
            ));
        }
        let mut audio = self.root.read_open_file(file, MAX_AUDIO_BYTES, true)?;
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
        guard_recovery_path(&directory)?;
        std::fs::create_dir(&directory)
            .map_err(|error| NativeError::Io("create recovery session", error))?;
        self.0.write_manifest(&manifest)?;
        let readers = self.0.register_readers(&manifest)?;
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
        let readers = self.0.register_readers(&state)?;
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
            _ => "invalid-phase",
        };
        let elapsed = (probe.clock.elapsed().as_nanos() as u64)
            .saturating_sub(probe.phase_started_ns.load(Ordering::Acquire));
        let pinned_reads = self.0.readers.try_lock().ok().and_then(|registry| {
            let mut pins = 0usize;
            for reader in registry.values().filter_map(Weak::upgrade) {
                let reader = reader.try_lock().ok()?;
                pins += reader.pins.values().sum::<usize>();
            }
            Some(pins)
        });
        serde_json::json!({
            "phase":name(phase),"active_ms":if phase==0{None}else{Some(elapsed as f64/1_000_000.0)},
            "last_phase":name(probe.last_phase.load(Ordering::Acquire)),
            "root_lock":self.0.trace.snapshot(),"active_pinned_reads":pinned_reads,
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
    fn register_readers(
        &self,
        state: &RecoveryManifest,
    ) -> Result<Arc<Mutex<ReaderState>>, NativeError> {
        let readers = Arc::new(Mutex::new(ReaderState::new(state)));
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
        Ok(!readers.pins.is_empty())
    }

    #[track_caller]
    fn lock(&self) -> Result<MutexGuard<'_, HashSet<String>>, NativeError> {
        #[cfg(test)]
        let mutation_wait = crate::mutation_trace::span("root-lock-wait");
        #[cfg(test)]
        let wait = self
            .trace
            .wait(&format!("{}", std::panic::Location::caller()));
        let guard = self
            .active
            .lock()
            .map_err(|_| NativeError::Verification("Recovery journal lock is poisoned."))?;
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
        let mut plain = self.read_encrypted(
            &self.session_path(id)?.join("manifest.enc"),
            MAX_MANIFEST_BYTES,
        )?;
        let parsed = RecoveryManifest::parse(&plain, id);
        plain.fill(0);
        let state = parsed?;
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

    fn list(&self) -> Result<Vec<NativeRecoveryInfo>, NativeError> {
        guard_recovery_path(&self.path)?;
        let entries = std::fs::read_dir(&self.path)
            .map_err(|error| NativeError::Io("list recovery root", error))?;
        let mut result = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|error| NativeError::Io("read recovery directory entry", error))?;
            let name = entry.file_name();
            let Some(id) = name.to_str().filter(|id| valid_id(id)) else {
                continue;
            };
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|error| NativeError::Io("inspect recovery entry", error))?;
            if metadata.file_attributes() & 0x400 != 0 {
                eprintln!("Recovery reparse entry retained for explicit inspection.");
                result.push(NativeRecoveryInfo::unreadable(id.into()));
                continue;
            }
            if !metadata.is_dir() {
                continue;
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
        #[cfg(test)]
        let mut charged_bytes = 0;
        #[cfg(test)]
        let mut metadata_probes = 0;
        let mut remaining =
            self.maximum_bytes
                .checked_sub(additional as u64)
                .ok_or(NativeError::Verification(
                    "Recovery storage quota exceeded.",
                ))?;
        let mut directories = vec![self.path.clone()];
        let mut count = 0usize;
        while let Some(directory) = directories.pop() {
            guard_recovery_path(&directory)?;
            for entry in std::fs::read_dir(directory)
                .map_err(|error| NativeError::Io("enumerate recovery quota", error))?
            {
                let entry =
                    entry.map_err(|error| NativeError::Io("read recovery quota entry", error))?;
                count += 1;
                if count > 1_000_000 {
                    return Err(NativeError::Verification(
                        "Recovery storage has too many entries.",
                    ));
                }
                let metadata = entry
                    .metadata()
                    .map_err(|error| NativeError::Io("inspect recovery quota entry", error))?;
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

    fn write_encrypted(&self, path: &Path, plain: &[u8], limit: usize) -> Result<(), NativeError> {
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
        // The previous target and any orphan/staging files stay charged during replacement.
        #[cfg(test)]
        self.io_probe.enter(9);
        self.ensure_quota(cipher.len())?;
        #[cfg(test)]
        self.io_probe.enter(3);
        atomic_write(path, &cipher, WriteMode::Replace)
    }

    fn write_manifest(&self, state: &RecoveryManifest) -> Result<(), NativeError> {
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
        #[cfg(test)]
        let _mutation = crate::mutation_trace::span("encrypted-read");
        #[cfg(test)]
        let _probe = {
            self.io_probe.enter(5);
            FixtureIoEnd(&self.io_probe)
        };
        guard_recovery_path(path)?;
        let file = File::open(path)
            .map_err(|error| NativeError::Io("open encrypted recovery file", error))?;
        self.read_open_file(file, limit, false)
    }

    fn read_open_file(
        &self,
        file: File,
        limit: usize,
        pinned: bool,
    ) -> Result<Vec<u8>, NativeError> {
        #[cfg(not(test))]
        let _ = pinned;
        let length = file
            .metadata()
            .map_err(|error| NativeError::Io("inspect encrypted recovery file", error))?
            .len();
        if length == 0
            || length > (limit + ENCRYPTION_ALLOWANCE) as u64
            || length > self.maximum_bytes
        {
            return Err(NativeError::Verification(
                "Encrypted recovery file exceeds supported bounds.",
            ));
        }
        #[cfg(test)]
        if pinned {
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
        if !pinned {
            self.io_probe.enter(6);
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
        guard_recovery_path(&path)?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(NativeError::Io("delete confirmed recovery audio", error)),
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
        guard_recovery_path(&directory)?;
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(NativeError::Io("list recovery deletion scope", error)),
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| NativeError::Io("inspect recovery deletion scope", error))?
                .path();
            guard_recovery_path(&path)?;
            if std::fs::symlink_metadata(&path)
                .map_err(|error| NativeError::Io("inspect recovery deletion file", error))?
                .is_dir()
            {
                return Err(NativeError::Verification(
                    "Unexpected nested recovery directory; no files deleted.",
                ));
            }
            files.push(path);
        }
        for file in files {
            guard_recovery_path(&file)?;
            std::fs::remove_file(file)
                .map_err(|error| NativeError::Io("delete scoped recovery file", error))?;
        }
        guard_recovery_path(&directory)?;
        std::fs::remove_dir(directory)
            .map_err(|error| NativeError::Io("delete empty recovery session", error))
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
        Ok(state)
    }

    fn publish_readers(&self, state: &RecoveryManifest) -> Result<(), NativeError> {
        #[cfg(test)]
        let _mutation = crate::mutation_trace::span("manifest-reader-publication");
        let mut readers = self
            .readers
            .lock()
            .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
        if readers.context != state.context {
            return Err(NativeError::Verification(
                "Recovery read binding changed after publication.",
            ));
        }
        readers.publish(state);
        Ok(())
    }

    pub(crate) fn pin_audio(
        &self,
        expected: &RecoveryChunk,
        context: &str,
    ) -> Result<PinnedRecoveryAudio, NativeError> {
        // The journal owns this immutable descriptor cache; pin admission never waits on disk I/O.
        self.ensure_open()?;
        let mut readers = self
            .readers
            .lock()
            .map_err(|_| NativeError::Verification("Recovery read pin lock is poisoned."))?;
        if readers.context != context {
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
        let _active = self.root.lock()?;
        self.ensure_open()?;
        self.load_current()
    }

    pub fn checkpoint(
        &self,
        tail: &[u8],
        captured: u64,
        added: &[AudioCheckpoint<'_>],
        complete: bool,
        now: OffsetDateTime,
    ) -> Result<(), NativeError> {
        self.checkpoint_snapshot(tail, captured, added, complete, now)
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
        #[cfg(test)]
        let mut mutation = crate::mutation_trace::span("journal-checkpoint");
        #[cfg(test)]
        if let Some(mutation) = &mut mutation {
            mutation.counts(added.len() as u64, 0, captured);
        }
        let _active = self.root.lock()?;
        self.ensure_open()?;
        let current = self.load_current()?;
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
            self.root.write_encrypted(
                &self.root.session_path(&self.id)?.join("manifest.enc"),
                &encoded,
                MAX_MANIFEST_BYTES,
            )?;
            self.publish_readers(&next)?;
            Ok(next)
        })();
        encoded.fill(0);
        result
    }

    pub fn read_audio(&self, index: u32) -> Result<Vec<u8>, NativeError> {
        let pin = {
            let _active = self.root.lock()?;
            self.ensure_open()?;
            let state = self.load_current()?;
            let chunk = state
                .chunks
                .get(index as usize)
                .ok_or(NativeError::InvalidArgument("Unknown recovery chunk."))?;
            self.pin_audio(chunk, &state.context)?
        };
        pin.read()
    }

    pub fn save_result(
        &self,
        index: u32,
        text: String,
        now: OffsetDateTime,
    ) -> Result<(), NativeError> {
        let _active = self.root.lock()?;
        self.ensure_open()?;
        let mut state = self.load_current()?;
        if state.commit_legacy_result(index, text, utc_ticks(now)?)? {
            self.root.write_manifest(&state)?;
            self.publish_readers(&state)?;
        }
        self.root.remove_audio(&self.id, index)
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
        let _active = self.root.lock()?;
        self.ensure_open()?;
        let mut state = self.load_current()?;
        let retry_deletions = through == state.confirmed_bytes;
        let released = state.confirm(
            through,
            text,
            context_audio,
            fallback,
            segment_words,
            utc_ticks(now)?,
        )?;
        self.root.write_manifest(&state)?;
        self.publish_readers(&state)?;
        if retry_deletions {
            return self.root.retry_confirmed_audio(&state);
        }
        for index in released {
            self.root.remove_audio(&self.id, index)?;
        }
        Ok(())
    }

    pub fn prepare_audit(
        &self,
        text: String,
        completed_at: OffsetDateTime,
        polished: bool,
        now: OffsetDateTime,
    ) -> Result<(), NativeError> {
        let _active = self.root.lock()?;
        self.ensure_open()?;
        let mut state = self.load_current()?;
        state.freeze_audit(text, utc_ticks(completed_at)?, polished, utc_ticks(now)?)?;
        self.root.write_manifest(&state)?;
        self.publish_readers(&state)
    }

    pub fn delete(&mut self) -> Result<(), NativeError> {
        let mut active = self.root.lock()?;
        self.ensure_open()?;
        self.root.delete_flat(&self.id)?;
        active.remove(&self.id);
        self.deleted = true;
        Ok(())
    }
}

impl Drop for NativeRecoveryJournal {
    fn drop(&mut self) {
        if !self.deleted {
            match self.root.lock() {
                Ok(mut active) => {
                    active.remove(&self.id);
                }
                Err(error) => eprintln!("Recovery cleanup lease release failed: {error}"),
            }
        }
    }
}

#[cfg(test)]
#[path = "recovery_store_tests.rs"]
pub(crate) mod tests;

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use time::OffsetDateTime;
use voiceprompt_client_protocol::recovery::{
    AudioCheckpoint, MAX_AUDIO_BYTES, MAX_MANIFEST_BYTES, RecoveryManifest, utc_ticks, valid_id,
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
}

#[derive(Clone)]
pub struct NativeRecoveryStore(Arc<RecoveryRoot>);

pub struct NativeRecoveryJournal {
    root: Arc<RecoveryRoot>,
    id: String,
    deleted: bool,
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
        std::fs::create_dir(&directory)
            .map_err(|error| NativeError::Io("create recovery session", error))?;
        self.0.write_manifest(&manifest)?;
        active.insert(manifest.id.clone());
        Ok(NativeRecoveryJournal {
            root: self.0.clone(),
            id: manifest.id.clone(),
            deleted: false,
        })
    }

    pub fn open(&self, id: &str) -> Result<NativeRecoveryJournal, NativeError> {
        let mut active = self.0.lock()?;
        if active.contains(id) {
            return Err(NativeError::Verification(
                "This recovery journal already has an active owner.",
            ));
        }
        let state = self.0.load(id, true)?;
        // Retry publish-before-delete leftovers once on reopen, not on every new confirmation.
        self.0.retry_confirmed_audio(&state)?;
        active.insert(id.into());
        Ok(NativeRecoveryJournal {
            root: self.0.clone(),
            id: id.into(),
            deleted: false,
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
    fn lock(&self) -> Result<MutexGuard<'_, HashSet<String>>, NativeError> {
        self.active
            .lock()
            .map_err(|_| NativeError::Verification("Recovery journal lock is poisoned."))
    }

    fn session_path(&self, id: &str) -> Result<PathBuf, NativeError> {
        if !valid_id(id) {
            return Err(NativeError::InvalidArgument(
                "Recovery ID must be a lowercase GUID in N format.",
            ));
        }
        let path = self.path.join(id);
        guard_recovery_path(&path)?;
        Ok(path)
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
                let metadata = std::fs::symlink_metadata(entry.path())
                    .map_err(|error| NativeError::Io("inspect recovery quota entry", error))?;
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
        if plain.len() > limit {
            return Err(NativeError::Verification(
                "Recovery plaintext exceeds bounds.",
            ));
        }
        let cipher = protect(plain)?;
        if cipher.is_empty() || cipher.len() > limit + ENCRYPTION_ALLOWANCE {
            return Err(NativeError::Verification(
                "Encrypted recovery payload exceeds bounds.",
            ));
        }
        guard_recovery_path(path)?;
        // The previous target and any orphan/staging files stay charged during replacement.
        self.ensure_quota(cipher.len())?;
        atomic_write(path, &cipher, WriteMode::Replace)
    }

    fn write_manifest(&self, state: &RecoveryManifest) -> Result<(), NativeError> {
        let path = self.session_path(&state.id)?.join("manifest.enc");
        let mut plain = state.encode()?;
        let result = self.write_encrypted(&path, &plain, MAX_MANIFEST_BYTES);
        plain.fill(0);
        result
    }

    fn read_encrypted(&self, path: &Path, limit: usize) -> Result<Vec<u8>, NativeError> {
        guard_recovery_path(path)?;
        let file = File::open(path)
            .map_err(|error| NativeError::Io("open encrypted recovery file", error))?;
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
        let mut cipher = Vec::new();
        file.take((limit + ENCRYPTION_ALLOWANCE + 1) as u64)
            .read_to_end(&mut cipher)
            .map_err(|error| NativeError::Io("read encrypted recovery file", error))?;
        if cipher.len() > limit + ENCRYPTION_ALLOWANCE {
            return Err(NativeError::Verification(
                "Encrypted recovery file exceeds supported bounds.",
            ));
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
        let directory = self.session_path(id)?;
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
        self.root.load(&self.id, false)
    }

    pub fn checkpoint(
        &self,
        tail: &[u8],
        captured: u64,
        added: &[AudioCheckpoint<'_>],
        complete: bool,
        now: OffsetDateTime,
    ) -> Result<(), NativeError> {
        let _active = self.root.lock()?;
        self.ensure_open()?;
        let current = self.root.load(&self.id, false)?;
        let next = current.checkpoint(tail, captured, added, complete, utc_ticks(now)?)?;
        // Check total manifest bounds before touching any audio. Still publish audio first.
        let mut encoded = next.encode()?;
        encoded.fill(0);
        for (offset, audio) in added.iter().enumerate() {
            let index = (current.chunks.len() + offset) as u32;
            self.root.remove_audio(&self.id, index)?;
            self.root.write_encrypted(
                &self.root.audio_path(&self.id, index)?,
                audio.wav,
                MAX_AUDIO_BYTES,
            )?;
        }
        self.root.write_manifest(&next)
    }

    pub fn read_audio(&self, index: u32) -> Result<Vec<u8>, NativeError> {
        let _active = self.root.lock()?;
        self.ensure_open()?;
        let state = self.root.load(&self.id, false)?;
        let chunk = state
            .chunks
            .get(index as usize)
            .ok_or(NativeError::InvalidArgument("Unknown recovery chunk."))?;
        if chunk.text.is_some() {
            return Err(NativeError::Verification(
                "This recovery chunk is already transcribed.",
            ));
        }
        let mut audio = self
            .root
            .read_encrypted(&self.root.audio_path(&self.id, index)?, MAX_AUDIO_BYTES)?;
        if let Err(error) = chunk.verify_audio(&audio) {
            audio.fill(0);
            return Err(error.into());
        }
        Ok(audio)
    }

    pub fn save_result(
        &self,
        index: u32,
        text: String,
        now: OffsetDateTime,
    ) -> Result<(), NativeError> {
        let _active = self.root.lock()?;
        self.ensure_open()?;
        let mut state = self.root.load(&self.id, false)?;
        if state.commit_legacy_result(index, text, utc_ticks(now)?)? {
            self.root.write_manifest(&state)?;
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
        let mut state = self.root.load(&self.id, false)?;
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
        let mut state = self.root.load(&self.id, false)?;
        state.freeze_audit(text, utc_ticks(completed_at)?, polished, utc_ticks(now)?)?;
        self.root.write_manifest(&state)
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

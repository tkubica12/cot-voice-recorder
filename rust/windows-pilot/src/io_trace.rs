use std::collections::{HashMap, VecDeque};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use serde_json::{Value, json};

const MAX_ACTIVE: usize = 64;
const MAX_EVENTS: usize = 128;
const MAX_PUBLICATIONS: usize = 32;
const MAX_WITNESS_BYTES: usize = 256 * 1024;
pub(crate) const READ_ACCESS: u32 = 0x8000_0000;
pub(crate) const WRITE_ACCESS: u32 = 0x4000_0000;
pub(crate) const DEFAULT_SHARE: u32 = 7;

struct Data {
    active: HashMap<u64, Value>,
    events: VecDeque<Value>,
    publications: HashMap<String, Value>,
    evicted_events: u64,
}

pub(crate) struct Trace {
    root: PathBuf,
    clock: Instant,
    next: AtomicU64,
    lost: AtomicU64,
    opening: AtomicUsize,
    first_failed: AtomicBool,
    persisted: AtomicBool,
    data: Mutex<Data>,
    first: OnceLock<Value>,
    sink: OnceLock<PathBuf>,
    admission: OnceLock<Box<dyn Fn() -> Value + Send + Sync>>,
}

impl Trace {
    pub(crate) fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            root,
            clock: Instant::now(),
            next: AtomicU64::new(1),
            lost: AtomicU64::new(0),
            opening: AtomicUsize::new(0),
            first_failed: AtomicBool::new(false),
            persisted: AtomicBool::new(false),
            data: Mutex::new(Data {
                active: HashMap::new(),
                events: VecDeque::with_capacity(MAX_EVENTS),
                publications: HashMap::new(),
                evicted_events: 0,
            }),
            first: OnceLock::new(),
            sink: OnceLock::new(),
            admission: OnceLock::new(),
        })
    }

    fn stamp(&self) -> Value {
        json!({
            "monotonic_ns":self.clock.elapsed().as_nanos() as u64,
            "utc_unix_ns":time::OffsetDateTime::now_utc().unix_timestamp_nanos().to_string()
        })
    }

    fn relative(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.root).ok()?;
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return None;
        }
        let name = relative.to_str()?;
        (name.len() <= 240).then(|| name.to_owned())
    }

    fn event(data: &mut Data, event: Value) {
        if data.events.len() == MAX_EVENTS {
            data.events.pop_front();
            data.evicted_events += 1;
        }
        data.events.push_back(event);
    }

    pub(crate) fn set_sink(&self, path: PathBuf) {
        assert!(
            self.sink.set(path).is_ok(),
            "One owned witness sink per root."
        );
    }

    pub(crate) fn first_record(&self) -> Option<Value> {
        self.first.get().cloned()
    }

    pub(crate) fn set_admission(&self, source: impl Fn() -> Value + Send + Sync + 'static) {
        assert!(
            self.admission.set(Box::new(source)).is_ok(),
            "One admission source per root."
        );
    }

    pub(crate) fn opening(
        self: &Arc<Self>,
        path: &Path,
        role: &'static str,
        access: u32,
        share: u32,
        flags: u32,
    ) -> Opening {
        self.opening.fetch_add(1, Ordering::AcqRel);
        let event = json!({
            "operation":"open-request","relative_path":self.relative(path),
            "role":role,"observed_request":self.stamp(),
            "rust_thread":format!("{:?}",std::thread::current().id()),
            "requested_access_flags":access,"requested_share_flags":share,
            "requested_custom_flags":flags
        });
        match self.data.try_lock() {
            Ok(mut data) => Self::event(&mut data, event),
            Err(_) => {
                self.lost.fetch_add(1, Ordering::AcqRel);
            }
        }
        Opening {
            trace: self.clone(),
        }
    }

    pub(crate) fn publication(&self, id: &str, generation: u64, saved: u64, confirmed: u64) {
        match self.data.try_lock() {
            Ok(mut data)
                if data.publications.contains_key(id)
                    || data.publications.len() < MAX_PUBLICATIONS =>
            {
                data.publications.insert(
                    id.to_owned(),
                    json!({"generation":generation,"saved_bytes":saved,"confirmed_bytes":confirmed}),
                );
            }
            _ => {
                self.lost.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    #[track_caller]
    pub(crate) fn opened(
        self: &Arc<Self>,
        file: &File,
        path: &Path,
        role: &'static str,
        access: u32,
        share: u32,
        custom_flags: u32,
    ) -> Handle {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let caller = std::panic::Location::caller();
        let relative = self.relative(path);
        let callsite = format!(
            "{}:{}",
            Path::new(caller.file())
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            caller.line()
        );
        let mut record = json!({
            "id":id,"native_handle":file.as_raw_handle() as usize,
            "relative_path":relative,"role":role,"operation":"open",
            "rust_thread":format!("{:?}",std::thread::current().id()),
            "observed_open":self.stamp(),"callsite":callsite,
            "requested_access_flags":access,"requested_share_flags":share,
            "requested_custom_flags":custom_flags,
            "requested_creation_disposition":match role {
                "atomic-staging-write"=>1,"root-writer-lease"=>4,_=>3
            },
            "file_identity":"NOT_COLLECTED","link_count":"NOT_COLLECTED",
            "metadata":"NOT_YET_AVAILABLE","publication":null
        });
        match self.data.try_lock() {
            Ok(mut data) if relative.is_some() && data.active.len() < MAX_ACTIVE => {
                let journal = relative.as_deref().and_then(|name| name.split('\\').next());
                if let Some(publication) = journal.and_then(|id| data.publications.get(id)) {
                    record["publication"] = publication.clone();
                }
                data.active.insert(id, record.clone());
                Self::event(&mut data, record);
            }
            _ => {
                self.lost.fetch_add(1, Ordering::AcqRel);
            }
        }
        Handle {
            trace: self.clone(),
            id,
        }
    }

    #[track_caller]
    pub(crate) fn track(
        self: &Arc<Self>,
        file: File,
        path: &Path,
        role: &'static str,
        access: u32,
        share: u32,
        flags: u32,
    ) -> TrackedFile {
        let handle = self.opened(&file, path, role, access, share, flags);
        TrackedFile {
            file: Some(file),
            handle,
        }
    }

    pub(crate) fn snapshot(&self) -> Value {
        let stamp = self.stamp();
        let captured = self.data.try_lock().ok().map(|data| {
            (
                data.active.values().cloned().collect::<Vec<_>>(),
                data.events.iter().cloned().collect::<Vec<_>>(),
                data.publications.clone(),
                data.evicted_events,
            )
        });
        let lost = self.lost.load(Ordering::Acquire);
        let opening = self.opening.load(Ordering::Acquire);
        match captured {
            Some((active, events, publications, evicted)) => json!({
                "observed_snapshot":stamp,"metadata_unavailable":false,
                "active_tracking_complete":lost==0 && opening==0,"lost_updates":lost,
                "open_requests_in_flight":opening,
                "active_handles":active,"recent_lifetime_events":events,
                "published_generations":publications,"evicted_lifetime_events":evicted,
                "publication_context":"last successful own publication; never an in-flight candidate",
                "first_rename_witness_persisted":self.persisted.load(Ordering::Acquire),
                "coverage":"own Recovery File opens: writer lease, live manifest, immutable alias, audio and atomic staging",
                "exclusions":"OS-internal metadata/link/rename handles, control/witness files and every other process",
                "timestamps_are_observed_user_mode_boundaries":true,
                "monitor_acquires_no_engine_or_root_lock":true
            }),
            None => json!({
                "observed_snapshot":stamp,"metadata_unavailable":true,
                "active_tracking_complete":false,"lost_updates":lost,"active_handles":null,
                "open_requests_in_flight":opening,
                "owner_classification":"UNKNOWN",
                "monitor_acquires_no_engine_or_root_lock":true
            }),
        }
    }

    fn metadata_after_error(&self, path: &Path) -> Value {
        let started = self.stamp();
        let guarded = crate::platform::guard_recovery_path(path).is_ok();
        let result = if !guarded {
            json!({"unknown":"post-error full-path/reparse guard rejected the probe"})
        } else {
            match std::fs::symlink_metadata(path) {
                Ok(metadata) => json!({
                    "exists":true,"bytes":metadata.len(),
                    "attributes":metadata.file_attributes(),
                    "readonly":metadata.permissions().readonly(),
                    "reparse":metadata.file_attributes()&0x400!=0
                }),
                Err(error) => json!({
                    "exists":if error.kind()==io::ErrorKind::NotFound {Some(false)} else {None},
                    "io_kind":format!("{:?}",error.kind()),"os_code":error.raw_os_error()
                }),
            }
        };
        json!({
            "probe_started_after_api_error":started,
            "observed_after_api_error":self.stamp(),"path_guard_passed":guarded,
            "not_first_error_atomic_state":true,"result":result
        })
    }

    pub(crate) fn failed_rename(
        &self,
        source: &Path,
        destination: &Path,
        flags: u32,
        error: &crate::platform::NativeError,
    ) {
        let observed = self.stamp();
        if self.first_failed.swap(true, Ordering::AcqRel) {
            return;
        }
        let handles = self.snapshot();
        let source_name = self.relative(source);
        let destination_name = self.relative(destination);
        let classification = if handles["active_tracking_complete"] != true {
            "UNKNOWN"
        } else if handles["active_handles"].as_array().is_some_and(|active| {
            active
                .iter()
                .any(|handle| handle["relative_path"] == json!(destination_name))
        }) {
            "OWN_LIVE_DESTINATION_HANDLE_OBSERVED"
        } else {
            "NO_INSTRUMENTED_OWNER_SEEN"
        };
        let (native_operation, hresult) = match error {
            crate::platform::NativeError::Api(operation, source) => (
                Some(*operation),
                Some(format!("0x{:08X}", source.code().0 as u32)),
            ),
            _ => (None, None),
        };
        let record = json!({
            "schema":1,"scope":"test-only-own-Recovery-root",
            "api_result_observed":observed,"operation":"atomic-publication",
            "source_relative_path":source_name,"destination_relative_path":destination_name,
            "rename_flags":flags,"first_native_error":error.to_string(),"HRESULT":hresult,
            "native_operation":native_operation,"source_kind":"NativeError::Api -> windows::core::Error",
            "rust_thread":format!("{:?}",std::thread::current().id()),
            "stage":"rename-failed-after-staging-flush-and-close-before-staging-cleanup",
            "own_handle_snapshot":handles,"owner_classification":classification,
            "classification_does_not_prove_external_actor_or_exclusive_causality":true,
            "admission":self.admission.get().map(|source|source()),
            "admission_fields_are_pointwise_atomics_not_one_global_transaction":true,
            "source_metadata_after_error":self.metadata_after_error(source),
            "destination_metadata_after_error":self.metadata_after_error(destination),
            "contents_environment_heap_and_other_processes_not_inspected":true
        });
        let _ = self.first.set(record.clone());
        if let Some(sink) = self.sink.get() {
            let result = (|| {
                crate::platform::guard_recovery_path(sink)
                    .map_err(|_| io::Error::other("Witness path validation failed."))?;
                let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
                if bytes.len() > MAX_WITNESS_BYTES {
                    return Err(io::Error::other("Witness exceeded diagnostic bounds."));
                }
                let mut file = OpenOptions::new().write(true).create_new(true).open(sink)?;
                file.write_all(&bytes)?;
                file.sync_all()
            })();
            match result {
                Ok(()) => self.persisted.store(true, Ordering::Release),
                Err(error) => eprintln!(
                    "Owned Recovery rename witness could not be persisted: kind={:?}; os={:?}; original error unchanged.",
                    error.kind(),
                    error.raw_os_error()
                ),
            }
        }
    }
}

pub(crate) struct Opening {
    trace: Arc<Trace>,
}

impl Drop for Opening {
    fn drop(&mut self) {
        self.trace.opening.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct Handle {
    trace: Arc<Trace>,
    id: u64,
}

impl Handle {
    fn metadata(&self, metadata: &Metadata) {
        match self.trace.data.try_lock() {
            Ok(mut data) => {
                if let Some(record) = data.active.get_mut(&self.id) {
                    record["metadata"] = json!({
                        "bytes":metadata.len(),"attributes":metadata.file_attributes(),
                        "source":"already-required handle metadata"
                    });
                }
            }
            Err(_) => {
                self.trace.lost.fetch_add(1, Ordering::AcqRel);
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        match self.trace.data.try_lock() {
            Ok(mut data) => {
                if let Some(mut record) = data.active.remove(&self.id) {
                    record["operation"] = json!("close");
                    record["observed_close"] = self.trace.stamp();
                    record["closed_by_rust_thread"] =
                        json!(format!("{:?}", std::thread::current().id()));
                    Trace::event(&mut data, record);
                }
            }
            Err(_) => {
                self.trace.lost.fetch_add(1, Ordering::AcqRel);
            }
        }
    }
}

pub(crate) struct TrackedFile {
    file: Option<File>,
    handle: Handle,
}

impl TrackedFile {
    pub(crate) fn metadata(&self) -> io::Result<Metadata> {
        let metadata = self.file.as_ref().expect("Live tracked file").metadata()?;
        self.handle.metadata(&metadata);
        Ok(metadata)
    }
}

impl Read for TrackedFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.as_mut().expect("Live tracked file").read(buffer)
    }
}

impl Drop for TrackedFile {
    fn drop(&mut self) {
        // Record close only after releasing the real OS handle.
        drop(self.file.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::WriteMode;
    use crate::recovery_store::tests::TemporaryRoot;
    use crate::{DEFAULT_RECOVERY_QUOTA, NativeRecoveryStore};
    use std::os::windows::fs::OpenOptionsExt;

    fn setup() -> (
        TemporaryRoot,
        NativeRecoveryStore,
        crate::NativeRecoveryJournal,
    ) {
        let directory = TemporaryRoot::new();
        let store =
            NativeRecoveryStore::new(directory.0.join("recovery"), DEFAULT_RECOVERY_QUOTA).unwrap();
        let journal = store
            .create(
                "cs".into(),
                "synthetic handle witness".into(),
                true,
                time::OffsetDateTime::now_utc(),
            )
            .unwrap();
        (directory, store, journal)
    }

    #[test]
    fn own_live_manifest_reader_failure_is_durable_before_cleanup_and_close_removes_cause() {
        let (directory, store, journal) = setup();
        let trace = store.fixture_own_io();
        let sink = directory.0.join("first-rename-failure.json");
        trace.set_sink(sink.clone());
        trace.set_admission(|| {
            json!({
                "accepted_bytes":1280,"received_bytes":1280,"completed_append_bytes":0,
                "scope":"controlled unit counters, not a continuous capture"
            })
        });
        let target = trace.root.join(journal.id()).join("manifest.enc");
        let original = std::fs::read(&target).unwrap();
        let opening = trace.opening(&target, "controlled-live-manifest-read", READ_ACCESS, 1, 0);
        let file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        let mut reader = trace.track(
            file,
            &target,
            "controlled-live-manifest-read",
            READ_ACCESS,
            1,
            0,
        );
        drop(opening);
        reader.metadata().unwrap();
        let error = journal
            .checkpoint_snapshot(&[], 0, &[], false, time::OffsetDateTime::now_utc())
            .unwrap_err();
        let witness: Value = serde_json::from_slice(&std::fs::read(&sink).unwrap()).unwrap();
        assert_eq!(trace.first_record().unwrap(), witness);
        assert!(trace.persisted.load(Ordering::Acquire));
        assert_eq!(
            witness["owner_classification"],
            "OWN_LIVE_DESTINATION_HANDLE_OBSERVED"
        );
        assert_eq!(witness["rename_flags"], 9);
        assert_eq!(witness["admission"]["accepted_bytes"], 1280);
        assert_eq!(
            witness["source_metadata_after_error"]["result"]["exists"],
            true
        );
        assert_eq!(
            witness["own_handle_snapshot"]["active_tracking_complete"],
            true
        );
        let active = witness["own_handle_snapshot"]["active_handles"]
            .as_array()
            .unwrap();
        let held = active
            .iter()
            .find(|row| row["role"] == "controlled-live-manifest-read")
            .unwrap();
        assert_eq!(held["requested_share_flags"], 1);
        assert_eq!(held["publication"]["generation"], 1);
        assert_eq!(
            held["metadata"]["source"],
            "already-required handle metadata"
        );
        assert!(
            !active
                .iter()
                .any(|row| row["role"] == "atomic-staging-write")
        );
        let staging = trace
            .root
            .join(witness["source_relative_path"].as_str().unwrap());
        assert!(
            !staging.exists(),
            "Staging cleanup must occur after witness persistence."
        );
        assert_eq!(std::fs::read(&target).unwrap(), original);
        let mut read = Vec::new();
        reader.read_to_end(&mut read).unwrap();
        assert_eq!(read, original);
        let api_time = witness["api_result_observed"]["monotonic_ns"]
            .as_u64()
            .unwrap();
        let stage_close = witness["own_handle_snapshot"]["recent_lifetime_events"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|row| row["role"] == "atomic-staging-write" && row["operation"] == "close")
            .unwrap();
        assert!(
            stage_close["observed_close"]["monotonic_ns"]
                .as_u64()
                .unwrap()
                <= api_time
        );
        println!("controlled-own-reader-first-error: {error}; witness={witness}");
        drop(reader);
        // Explicit fresh validation in this controlled test, never a production error retry.
        journal
            .checkpoint(&[], 0, &[], false, time::OffsetDateTime::now_utc())
            .unwrap();
        assert!(
            !trace.snapshot()["active_handles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["relative_path"] == held["relative_path"])
        );
        assert_eq!(
            std::fs::read(&sink).unwrap(),
            serde_json::to_vec(&witness).unwrap()
        );
        drop(journal);
        drop(store);
        assert!(
            trace.snapshot()["active_handles"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn no_reader_publish_and_alias_lifetime_are_visible_without_a_false_denial_witness() {
        let (directory, store, journal) = setup();
        let trace = store.fixture_own_io();
        let sink = directory.0.join("first-rename-failure.json");
        trace.set_sink(sink.clone());
        journal
            .checkpoint_snapshot(&[], 0, &[], false, time::OffsetDateTime::now_utc())
            .unwrap();
        journal.snapshot().unwrap();
        assert!(!sink.exists() && trace.first_record().is_none());
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["active_tracking_complete"], true);
        assert_eq!(snapshot["active_handles"].as_array().unwrap().len(), 1);
        let events = snapshot["recent_lifetime_events"].as_array().unwrap();
        let alias_open = events
            .iter()
            .find(|row| {
                row["operation"] == "open" && row["role"] == "immutable-manifest-alias-read"
            })
            .unwrap();
        assert_eq!(alias_open["requested_share_flags"], 1);
        assert!(
            events
                .iter()
                .any(|row| row["operation"] == "close" && row["id"] == alias_open["id"])
        );
        assert!(
            events
                .iter()
                .any(|row| row["operation"] == "close" && row["role"] == "atomic-staging-write")
        );
    }

    struct RestorePermissions(PathBuf, std::fs::Permissions);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            std::fs::set_permissions(&self.0, self.1.clone()).unwrap();
        }
    }

    #[test]
    fn owned_readonly_target_has_no_instrumented_reader_and_post_error_attributes_are_not_atomic() {
        let (directory, store, journal) = setup();
        let trace = store.fixture_own_io();
        let sink = directory.0.join("first-rename-failure.json");
        trace.set_sink(sink.clone());
        let target = trace.root.join(journal.id()).join("manifest.enc");
        let permissions = std::fs::metadata(&target).unwrap().permissions();
        let restore = RestorePermissions(target.clone(), permissions.clone());
        let mut readonly = permissions;
        readonly.set_readonly(true);
        std::fs::set_permissions(&target, readonly).unwrap();
        let error = journal
            .checkpoint_snapshot(&[], 0, &[], false, time::OffsetDateTime::now_utc())
            .unwrap_err();
        let witness: Value = serde_json::from_slice(&std::fs::read(&sink).unwrap()).unwrap();
        assert_eq!(
            witness["owner_classification"],
            "NO_INSTRUMENTED_OWNER_SEEN"
        );
        assert_eq!(
            witness["destination_metadata_after_error"]["result"]["readonly"],
            true
        );
        assert_eq!(
            witness["destination_metadata_after_error"]["not_first_error_atomic_state"],
            true
        );
        assert!(
            witness["destination_metadata_after_error"]["observed_after_api_error"]["monotonic_ns"]
                .as_u64()
                .unwrap()
                >= witness["api_result_observed"]["monotonic_ns"]
                    .as_u64()
                    .unwrap()
        );
        println!("controlled-own-readonly-first-error: {error}; witness={witness}");
        drop(restore);
        journal
            .checkpoint(&[], 0, &[], false, time::OffsetDateTime::now_utc())
            .unwrap();
    }

    #[test]
    fn busy_registry_open_in_flight_and_capacity_loss_are_unknown_not_empty_owner_proofs() {
        let directory = TemporaryRoot::new();
        let root = std::fs::canonicalize(&directory.0).unwrap();
        let trace = Trace::new(root.clone());
        let path = root.join("manifest.enc");
        crate::platform::atomic_write_observed(&path, b"synthetic", WriteMode::Replace, &trace)
            .unwrap();
        {
            let _held = trace.data.lock().unwrap();
            let snapshot = trace.snapshot();
            assert_eq!(snapshot["metadata_unavailable"], true);
            assert!(snapshot["active_handles"].is_null());
        }
        let opening = trace.opening(&path, "pending-open", READ_ACCESS, DEFAULT_SHARE, 0);
        assert_eq!(trace.snapshot()["active_tracking_complete"], false);
        drop(opening);
        assert_eq!(trace.snapshot()["active_tracking_complete"], true);
        let mut files = Vec::new();
        for _ in 0..MAX_ACTIVE + 1 {
            let file = File::open(&path).unwrap();
            files.push(trace.track(file, &path, "bounded-reader", READ_ACCESS, DEFAULT_SHARE, 0));
        }
        let snapshot = trace.snapshot();
        assert_eq!(
            snapshot["active_handles"].as_array().unwrap().len(),
            MAX_ACTIVE
        );
        assert_eq!(snapshot["active_tracking_complete"], false);
        assert!(snapshot["lost_updates"].as_u64().unwrap() > 0);
        drop(files);
        assert!(
            trace.snapshot()["active_handles"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(trace.snapshot()["active_tracking_complete"], false);
        assert_eq!(
            trace.snapshot()["recent_lifetime_events"]
                .as_array()
                .unwrap()
                .len(),
            MAX_EVENTS
        );
        assert!(
            trace.snapshot()["evicted_lifetime_events"]
                .as_u64()
                .unwrap()
                > 0
        );
    }

    #[test]
    fn witness_storage_failure_keeps_first_native_error_and_in_memory_context() {
        let (directory, store, journal) = setup();
        let trace = store.fixture_own_io();
        let sink_directory = directory.0.join("not-a-witness-file");
        std::fs::create_dir(&sink_directory).unwrap();
        trace.set_sink(sink_directory);
        let target = trace.root.join(journal.id()).join("manifest.enc");
        let file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        let reader = trace.track(
            file,
            &target,
            "controlled-live-manifest-read",
            READ_ACCESS,
            1,
            0,
        );
        let error = journal
            .checkpoint_snapshot(&[], 0, &[], false, time::OffsetDateTime::now_utc())
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("MoveFileExW (replace encrypted cache)")
        );
        let witness = trace.first_record().unwrap();
        assert_eq!(
            witness["owner_classification"],
            "OWN_LIVE_DESTINATION_HANDLE_OBSERVED"
        );
        assert_eq!(witness["HRESULT"], "0x80070005");
        assert!(!trace.persisted.load(Ordering::Acquire));
        drop(reader);
    }

    #[test]
    fn witness_metadata_contention_does_not_wait_or_turn_unknown_into_no_owner() {
        let directory = TemporaryRoot::new();
        let root = std::fs::canonicalize(&directory.0).unwrap();
        let trace = Trace::new(root.clone());
        let target = root.join("manifest.enc");
        crate::platform::atomic_write_observed(&target, b"old", WriteMode::Replace, &trace)
            .unwrap();
        let reader = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        let sink = root.join("first-rename-failure.json");
        trace.set_sink(sink.clone());
        let held = trace.data.lock().unwrap();
        let error =
            crate::platform::atomic_write_observed(&target, b"new", WriteMode::Replace, &trace)
                .unwrap_err();
        let witness: Value = serde_json::from_slice(&std::fs::read(sink).unwrap()).unwrap();
        assert_eq!(witness["owner_classification"], "UNKNOWN");
        assert!(witness["own_handle_snapshot"]["active_handles"].is_null());
        println!("controlled-busy-trace-first-error: {error}");
        drop(held);
        drop(reader);
        assert_eq!(std::fs::read(target).unwrap(), b"old");
    }
}

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

const MAX_CONTEXTS: usize = 64;
const MAX_STACK_BYTES: usize = 16 * 1024;
static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);
static CAPTURE_STACKS: AtomicBool = AtomicBool::new(true);
thread_local! {
    static THREAD: u64 = NEXT_THREAD.fetch_add(1, Ordering::Relaxed);
}

fn now() -> u64 {
    static CLOCK: OnceLock<Instant> = OnceLock::new();
    CLOCK.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

pub(crate) fn freeze_context_capture() {
    CAPTURE_STACKS.store(false, Ordering::Release);
}

#[inline(never)]
fn acquisition_backtrace() -> String {
    format!("{:?}", std::backtrace::Backtrace::force_capture())
}

fn bounded_backtrace() -> String {
    let mut stack = acquisition_backtrace();
    if stack.len() > MAX_STACK_BYTES {
        let mut end = MAX_STACK_BYTES;
        while !stack.is_char_boundary(end) {
            end -= 1;
        }
        stack.truncate(end);
    }
    stack
}

#[inline(never)]
fn known_symbol_fixture_callsite() -> String {
    let stack = acquisition_backtrace();
    std::hint::black_box(&stack);
    stack
}

pub(crate) fn verify_fixture_symbols() {
    let stack = known_symbol_fixture_callsite();
    assert!(
        stack.contains("known_symbol_fixture_callsite"),
        "Local diagnostic symbols did not resolve: {stack}"
    );
}

struct Context {
    operation: String,
    acquisition_stack: Option<String>,
    captured_rust_thread_key: u64,
    captured_ns: Option<u64>,
}

struct Contexts {
    keys: HashMap<String, usize>,
    values: Vec<Context>,
}

fn contexts() -> &'static Mutex<Contexts> {
    static CONTEXTS: OnceLock<Mutex<Contexts>> = OnceLock::new();
    CONTEXTS.get_or_init(|| {
        Mutex::new(Contexts {
            keys: HashMap::new(),
            values: Vec::new(),
        })
    })
}

fn context(kind: &str, operation: &str, capture_stack: bool) -> usize {
    let thread = THREAD.with(|id| *id);
    let key = format!("{kind}:{operation}:{thread}");
    let Ok(mut cache) = contexts().try_lock() else {
        return 0;
    };
    if let Some(id) = cache.keys.get(&key).copied() {
        let entry = &mut cache.values[id - 1];
        if entry.acquisition_stack.is_none()
            && capture_stack
            && CAPTURE_STACKS.load(Ordering::Acquire)
        {
            entry.acquisition_stack = Some(bounded_backtrace());
            entry.captured_ns = Some(now());
        }
        return id;
    }
    if cache.values.len() == MAX_CONTEXTS {
        return 0;
    }
    let stack = (capture_stack && CAPTURE_STACKS.load(Ordering::Acquire)).then(bounded_backtrace);
    let captured_ns = stack.as_ref().map(|_| now());
    cache.values.push(Context {
        operation: operation.into(),
        acquisition_stack: stack,
        captured_rust_thread_key: thread,
        captured_ns,
    });
    let id = cache.values.len();
    cache.keys.insert(key, id);
    id
}

pub(crate) struct Lock {
    kind: &'static str,
    enabled: bool,
    capture_stack: bool,
    owner_epoch: AtomicU64,
    owner_thread: AtomicU64,
    owner_context: AtomicUsize,
    acquired_ns: AtomicU64,
    released_ns: AtomicU64,
    wait_count: AtomicUsize,
    last_wait_thread: AtomicU64,
    last_wait_context: AtomicUsize,
    last_wait_ns: AtomicU64,
    maximum_hold_ns: AtomicU64,
    maximum_wait_ns: AtomicU64,
    captured_bytes: AtomicU64,
    saved_bytes: AtomicU64,
    confirmed_bytes: AtomicU64,
}

impl Lock {
    pub fn new(kind: &'static str) -> Arc<Self> {
        let enabled = std::env::var("VOICEPROMPT_LOCK_TRACE").as_deref() == Ok("owner-phases");
        Arc::new(Self::with_options(kind, enabled, enabled))
    }

    fn with_options(kind: &'static str, enabled: bool, capture_stack: bool) -> Self {
        Self {
            kind,
            enabled,
            capture_stack,
            owner_epoch: AtomicU64::new(0),
            owner_thread: AtomicU64::new(0),
            owner_context: AtomicUsize::new(0),
            acquired_ns: AtomicU64::new(0),
            released_ns: AtomicU64::new(0),
            wait_count: AtomicUsize::new(0),
            last_wait_thread: AtomicU64::new(0),
            last_wait_context: AtomicUsize::new(0),
            last_wait_ns: AtomicU64::new(0),
            maximum_hold_ns: AtomicU64::new(0),
            maximum_wait_ns: AtomicU64::new(0),
            captured_bytes: AtomicU64::new(0),
            saved_bytes: AtomicU64::new(0),
            confirmed_bytes: AtomicU64::new(0),
        }
    }

    pub fn wait(self: &Arc<Self>, operation: &str) -> Wait {
        if self.enabled {
            let id = context(self.kind, operation, false);
            self.last_wait_thread
                .store(THREAD.with(|id| *id), Ordering::Release);
            self.last_wait_context.store(id, Ordering::Release);
            self.last_wait_ns.store(now(), Ordering::Release);
            self.wait_count.fetch_add(1, Ordering::AcqRel);
        }
        Wait {
            lock: self.clone(),
            operation: operation.into(),
            active: self.enabled,
            started_ns: now(),
        }
    }

    pub fn snapshot(&self) -> serde_json::Value {
        let epoch = self.owner_epoch.load(Ordering::Acquire);
        let owner_thread = self.owner_thread.load(Ordering::Acquire);
        let acquired = self.acquired_ns.load(Ordering::Acquire);
        let owner_context = self.owner_context.load(Ordering::Acquire);
        let wait_context = self.last_wait_context.load(Ordering::Acquire);
        let lookup = |id: usize, cache: &Contexts| {
            cache.values.get(id.wrapping_sub(1)).map(|entry| {
                serde_json::json!({"operation":entry.operation,
                "acquisition_context_stack":entry.acquisition_stack,
                "context_captured_rust_thread_key":entry.captured_rust_thread_key,
                "context_captured_ns":entry.captured_ns,
                "not_current_suspended_stack":true})
            })
        };
        let (owner, waiter, context_busy) = match contexts().try_lock() {
            Ok(cache) => (
                lookup(owner_context, &cache),
                lookup(wait_context, &cache),
                false,
            ),
            Err(_) => (None, None, true),
        };
        serde_json::json!({
            "kind":self.kind,"enabled":self.enabled,"epoch":epoch,"owner_rust_thread_key":owner_thread,
            "acquired_ns":acquired,"released_ns":self.released_ns.load(Ordering::Acquire),
            "owner_age_ms":if epoch==0 {None}else{Some(now().saturating_sub(acquired) as f64/1_000_000.0)},
            "wait_count":self.wait_count.load(Ordering::Acquire),
            "last_wait_rust_thread_key":self.last_wait_thread.load(Ordering::Acquire),
            "last_wait_started_ns":self.last_wait_ns.load(Ordering::Acquire),
            "owner_context":owner,"last_wait_context":waiter,"context_cache_busy":context_busy,
            "stable_epoch":epoch==self.owner_epoch.load(Ordering::Acquire),
            "owner_active":epoch!=0,
            "timestamps_are_process_monotonic":true,"not_os_thread_ids":true
            ,"maximum_hold_ms":self.maximum_hold_ns.load(Ordering::Acquire) as f64/1_000_000.0,
            "maximum_wait_ms":self.maximum_wait_ns.load(Ordering::Acquire) as f64/1_000_000.0,
            "captured_bytes":self.captured_bytes.load(Ordering::Acquire),
            "saved_bytes":self.saved_bytes.load(Ordering::Acquire),
            "confirmed_bytes":self.confirmed_bytes.load(Ordering::Acquire)
        })
    }

    pub fn progress(&self, captured: u64, saved: u64, confirmed: u64) {
        self.captured_bytes.store(captured, Ordering::Release);
        self.saved_bytes.store(saved, Ordering::Release);
        self.confirmed_bytes.store(confirmed, Ordering::Release);
    }
}

pub(crate) struct Wait {
    lock: Arc<Lock>,
    operation: String,
    active: bool,
    started_ns: u64,
}

impl Wait {
    pub fn acquired<'a, T>(mut self, inner: std::sync::MutexGuard<'a, T>) -> Guard<'a, T> {
        let acquired_ns = now();
        let epoch = if self.active {
            self.lock.wait_count.fetch_sub(1, Ordering::AcqRel);
            self.active = false;
            let epoch = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
            self.lock.owner_epoch.store(0, Ordering::Release);
            self.lock
                .owner_thread
                .store(THREAD.with(|id| *id), Ordering::Release);
            self.lock.owner_context.store(0, Ordering::Release);
            self.lock.acquired_ns.store(acquired_ns, Ordering::Release);
            self.lock.maximum_wait_ns.fetch_max(
                acquired_ns.saturating_sub(self.started_ns),
                Ordering::AcqRel,
            );
            self.lock.owner_epoch.store(epoch, Ordering::Release);
            let id = context(self.lock.kind, &self.operation, self.lock.capture_stack);
            self.lock.owner_context.store(id, Ordering::Release);
            epoch
        } else {
            0
        };
        Guard {
            inner: Some(inner),
            lock: self.lock.clone(),
            epoch,
            acquired_ns,
            mutation: crate::mutation_trace::span(match self.lock.kind {
                "recovery-root" => "root-lock-held",
                "streaming-state" | "legacy-state" => "engine-lock-held",
                _ => "other-lock-held",
            }),
        }
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        if self.active {
            self.lock.wait_count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

pub(crate) struct Guard<'a, T> {
    inner: Option<std::sync::MutexGuard<'a, T>>,
    lock: Arc<Lock>,
    epoch: u64,
    acquired_ns: u64,
    mutation: Option<crate::mutation_trace::Span>,
}
impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.inner.as_ref().expect("Owned guard remains live")
    }
}
impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.inner.as_mut().expect("Owned guard remains live")
    }
}
impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        drop(self.inner.take());
        drop(self.mutation.take());
        if self.epoch != 0 {
            let released_ns = now();
            self.lock.released_ns.store(released_ns, Ordering::Release);
            self.lock.maximum_hold_ns.fetch_max(
                released_ns.saturating_sub(self.acquired_ns),
                Ordering::AcqRel,
            );
            let _ = self.lock.owner_epoch.compare_exchange(
                self.epoch,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_guard_tracks_owner_wait_and_release_without_monitoring_the_target_mutex() {
        let trace = Arc::new(Lock::with_options("fixture-lock", true, false));
        let mutex = Mutex::new(1);
        let waiting = trace.wait("known-fixture-owner");
        assert_eq!(trace.snapshot()["wait_count"], 1);
        let guard = waiting.acquired(mutex.lock().unwrap());
        let owned = trace.snapshot();
        assert!(owned["epoch"].as_u64().unwrap() > 0);
        assert_eq!(owned["owner_context"]["operation"], "known-fixture-owner");
        assert_eq!(owned["wait_count"], 0);
        drop(guard);
        assert_eq!(trace.snapshot()["epoch"], 0);
    }

    #[test]
    fn context_cache_contention_never_blocks_the_monitor() {
        let trace = Lock::with_options("fixture-monitor", true, false);
        let cache = contexts().lock().unwrap();
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["context_cache_busy"], true);
        drop(cache);
    }

    #[test]
    #[ignore = "Diagnostic release with local PDB and debuginfo; verify symbols before owner tracing."]
    fn diagnostic_backtrace_contains_known_fixture_symbol() {
        verify_fixture_symbols();
        println!("known-local-fixture-symbol-resolved; acquisition-context-only");
    }
}

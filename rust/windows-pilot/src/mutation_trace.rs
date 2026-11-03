use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAX_EVENTS: usize = 512;
const MAX_ACTIVE: usize = 32;

thread_local! {
    static CURRENT: RefCell<Option<Arc<Capture>>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct Event {
    id: u64,
    phase: &'static str,
    started_ns: u64,
    completed_ns: Option<u64>,
    entries: u64,
    metadata_probes: u64,
    bytes: u64,
    owner_rust_thread: String,
}

impl Event {
    fn value(&self) -> serde_json::Value {
        serde_json::json!({
            "id":self.id,"phase":self.phase,"started_ns":self.started_ns,
            "completed_ns":self.completed_ns,"entries":self.entries,
            "metadata_probes":self.metadata_probes,"bytes":self.bytes,
            "owner_rust_thread":self.owner_rust_thread
        })
    }
}

struct Data {
    active: Vec<Event>,
    completed: Vec<Event>,
}

pub(crate) struct Capture {
    clock: Instant,
    next: AtomicU64,
    dropped: AtomicU64,
    data: Mutex<Data>,
}

impl Capture {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            clock: Instant::now(),
            next: AtomicU64::new(1),
            dropped: AtomicU64::new(0),
            data: Mutex::new(Data {
                active: Vec::with_capacity(MAX_ACTIVE),
                completed: Vec::with_capacity(MAX_EVENTS),
            }),
        })
    }

    pub fn enter(self: &Arc<Self>) -> Scope {
        let previous = CURRENT.with(|current| current.replace(Some(self.clone())));
        Scope {
            previous,
            _thread: std::marker::PhantomData,
        }
    }

    pub fn snapshot(&self) -> serde_json::Value {
        let now = self.clock.elapsed().as_nanos() as u64;
        match self.data.try_lock() {
            Ok(data) => serde_json::json!({
                "snapshot_ns":now,
                "completed":data.completed.iter().map(Event::value).collect::<Vec<_>>(),
                "active":data.active.iter().map(|event|serde_json::json!({
                    "event":event.value(),"current_age_ns":now.saturating_sub(event.started_ns)
                })).collect::<Vec<_>>(),
                "dropped_events":self.dropped.load(Ordering::Acquire),
                "metadata_unavailable":false,
                "monitor_acquires_no_engine_or_journal_lock":true,
                "phases_are_inclusive_and_may_nest":true
                ,"thread_ids_are_rust_not_os":true
            }),
            Err(_) => serde_json::json!({
                "snapshot_ns":now,"metadata_unavailable":true,
                "dropped_events":self.dropped.load(Ordering::Acquire),
                "monitor_acquires_no_engine_or_journal_lock":true
            }),
        }
    }
}

pub(crate) struct Scope {
    previous: Option<Arc<Capture>>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|current| current.replace(self.previous.take()));
    }
}

pub(crate) struct Span {
    capture: Arc<Capture>,
    event: Event,
}

pub(crate) fn span(phase: &'static str) -> Option<Span> {
    let capture = CURRENT.with(|current| current.borrow().clone())?;
    let event = Event {
        id: capture.next.fetch_add(1, Ordering::Relaxed),
        phase,
        started_ns: capture.clock.elapsed().as_nanos() as u64,
        completed_ns: None,
        entries: 0,
        metadata_probes: 0,
        bytes: 0,
        owner_rust_thread: format!("{:?}", std::thread::current().id()),
    };
    match capture.data.try_lock() {
        Ok(mut data) if data.active.len() < MAX_ACTIVE => data.active.push(event.clone()),
        _ => {
            capture.dropped.fetch_add(1, Ordering::AcqRel);
        }
    }
    Some(Span { capture, event })
}

impl Span {
    pub fn counts(&mut self, entries: u64, probes: u64, bytes: u64) {
        self.event.entries = entries;
        self.event.metadata_probes = probes;
        self.event.bytes = bytes;
        match self.capture.data.try_lock() {
            Ok(mut data) => {
                if let Some(active) = data
                    .active
                    .iter_mut()
                    .find(|event| event.id == self.event.id)
                {
                    *active = self.event.clone();
                }
            }
            Err(_) => {
                self.capture.dropped.fetch_add(1, Ordering::AcqRel);
            }
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        self.event.completed_ns = Some(self.capture.clock.elapsed().as_nanos() as u64);
        match self.capture.data.try_lock() {
            Ok(mut data) => {
                data.active.retain(|event| event.id != self.event.id);
                if data.completed.len() < MAX_EVENTS {
                    data.completed.push(self.event.clone());
                } else {
                    self.capture.dropped.fetch_add(1, Ordering::AcqRel);
                }
            }
            Err(_) => {
                self.capture.dropped.fetch_add(1, Ordering::AcqRel);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_timeline_separates_completed_duration_from_active_age() {
        let capture = Capture::new();
        let scope = capture.enter();
        let parent = span("known-outer").unwrap();
        {
            let mut child = span("known-child").unwrap();
            child.counts(2, 3, 4);
        }
        let active = capture.snapshot();
        assert_eq!(active["completed"].as_array().unwrap().len(), 1);
        assert_eq!(active["active"].as_array().unwrap().len(), 1);
        assert_eq!(active["completed"][0]["bytes"], 4);
        drop(parent);
        drop(scope);
        let complete = capture.snapshot();
        assert!(complete["active"].as_array().unwrap().is_empty());
        assert_eq!(complete["dropped_events"], 0);
        assert!(span("disabled").is_none());
    }

    #[test]
    fn monitor_never_waits_on_its_metadata_lock_and_capacity_failure_is_explicit() {
        let capture = Capture::new();
        let scope = capture.enter();
        let held = capture.data.lock().unwrap();
        assert_eq!(capture.snapshot()["metadata_unavailable"], true);
        drop(span("metadata-busy"));
        drop(held);
        assert!(capture.snapshot()["dropped_events"].as_u64().unwrap() > 0);
        for _ in 0..MAX_EVENTS + 1 {
            drop(span("bounded"));
        }
        let result = capture.snapshot();
        assert_eq!(result["completed"].as_array().unwrap().len(), MAX_EVENTS);
        assert!(result["dropped_events"].as_u64().unwrap() > 0);
        drop(scope);
    }
}

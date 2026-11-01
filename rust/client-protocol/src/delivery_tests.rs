use super::*;
use crate::api::ApiErrorKind;
use crate::history::{add_entry, live_entries};
use std::io;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize};

#[derive(Default)]
struct History {
    entries: StdMutex<Vec<HistoryEntry>>,
    fail: AtomicBool,
}

impl HistoryPersistence for History {
    type Error = io::Error;
    fn all(&self, now: OffsetDateTime) -> Result<Vec<HistoryEntry>, Self::Error> {
        Ok(live_entries(self.entries.lock().unwrap().clone(), now))
    }
    fn add(&self, entry: HistoryEntry, now: OffsetDateTime) -> Result<(), Self::Error> {
        if self.fail.load(Ordering::Acquire) {
            return Err(io::Error::other("synthetic disk failure"));
        }
        let mut entries = self.entries.lock().unwrap();
        *entries = add_entry(std::mem::take(&mut entries), entry, now);
        Ok(())
    }
    fn cleanup(&self, now: OffsetDateTime) -> Result<usize, Self::Error> {
        let mut entries = self.entries.lock().unwrap();
        let before = entries.len();
        *entries = live_entries(std::mem::take(&mut entries), now);
        Ok(before - entries.len())
    }
}

#[derive(Clone, Default)]
struct Writer {
    copies: Arc<StdMutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
    busy: Arc<AtomicUsize>,
}

impl ClipboardWriter for Writer {
    async fn write(
        &self,
        text: &str,
        automatic: Option<Arc<DeliveryPolicy>>,
    ) -> Result<CopyResult, ClipboardError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if automatic
            .as_ref()
            .is_some_and(|policy| !policy.allows_auto_copy())
        {
            return Ok(CopyResult::Suppressed);
        }
        let mut busy = self.busy.load(Ordering::Acquire);
        while busy > 0 {
            match self.busy.compare_exchange_weak(
                busy,
                busy - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Err(ClipboardError::Busy),
                Err(current) => busy = current,
            }
        }
        self.copies.lock().unwrap().push(text.into());
        Ok(CopyResult::Copied)
    }
}

#[derive(Clone, Default)]
struct Notifications(Arc<StdMutex<Vec<Notice>>>);

impl Notifier for Notifications {
    fn notify(&self, notice: Notice) -> Result<(), NotificationError> {
        self.0.lock().unwrap().push(notice);
        Ok(())
    }
}

#[derive(Default)]
struct Source {
    calls: AtomicUsize,
    fail: AtomicBool,
    audit: bool,
    delay: Duration,
}

impl TranscriptSource for Source {
    async fn fetch(&self, id: &str) -> Result<Transcript, ApiError> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        tokio::time::sleep(self.delay).await;
        if self.fail.load(Ordering::Acquire) {
            return Err(ApiError {
                kind: ApiErrorKind::Retryable,
                status: Some(503),
                problem: None,
                response_invalid: false,
            });
        }
        let now = OffsetDateTime::now_utc();
        Ok(Transcript {
            transcript_id: id.into(),
            recording_id: "synthetic".into(),
            body: format!("final-{id}"),
            raw_body: Some(format!("original-{id}")),
            preview: "private synthetic preview".into(),
            character_count: 7,
            completed_at: now,
            expires_at: now + time::Duration::hours(48),
            source: if self.audit {
                "windows_dictation"
            } else {
                "recording"
            }
            .into(),
            language: None,
            refine_model: None,
            transcribe_model: None,
        })
    }
}

fn event(id: &str) -> TranscriptCompleted {
    TranscriptCompleted {
        event: "transcript.completed".into(),
        event_id: format!("event-{id}"),
        transcript_id: id.into(),
        recording_id: "synthetic".into(),
        completed_at: None,
        preview: String::new(),
    }
}

type Coordinator = TranscriptCoordinator<Source, History, Writer, Notifications>;

fn coordinator(source: Source) -> Coordinator {
    TranscriptCoordinator::new(
        source,
        Arc::new(History::default()),
        Writer::default(),
        Notifications::default(),
        Arc::new(DeliveryPolicy::default()),
    )
}

#[tokio::test]
async fn durable_history_precedes_copy_and_concurrent_duplicates_are_copied_once() {
    let value = coordinator(Source::default());
    let evt = event("a");
    let (first, second) = tokio::join!(value.handle(&evt), value.handle(&evt));
    assert_eq!(first.unwrap(), HandlingResult::CopiedAndNotified);
    assert_eq!(second.unwrap(), HandlingResult::Duplicate);
    assert_eq!(value.entries().await.unwrap()[0].body, "final-a");
    assert_eq!(*value.clipboard.writer.copies.lock().unwrap(), ["final-a"]);
    assert_eq!(
        *value.notifier.0.lock().unwrap(),
        [Notice::TranscriptCopied]
    );
    assert_eq!(value.source.calls.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn failed_fetch_and_failed_disk_are_retryable_and_never_copy() {
    let value = coordinator(Source::default());
    value.source.fail.store(true, Ordering::Release);
    assert!(matches!(
        value.handle(&event("a")).await,
        Err(DeliveryError::Fetch(_))
    ));
    value.source.fail.store(false, Ordering::Release);
    value.history.fail.store(true, Ordering::Release);
    assert!(matches!(
        value.handle(&event("a")).await,
        Err(DeliveryError::History(_))
    ));
    assert!(value.clipboard.writer.copies.lock().unwrap().is_empty());
    value.history.fail.store(false, Ordering::Release);
    assert_eq!(
        value.handle(&event("a")).await.unwrap(),
        HandlingResult::CopiedAndNotified
    );
}

#[tokio::test]
async fn pause_busy_and_audit_cache_without_autocopy_but_manual_original_still_works() {
    for case in 0..3 {
        let value = coordinator(Source {
            audit: case == 2,
            ..Default::default()
        });
        value.policy.set_paused(case == 0);
        value.policy.set_dictation_busy(case == 1);
        assert_eq!(
            value.handle(&event("a")).await.unwrap(),
            HandlingResult::CachedSuppressed
        );
        assert!(value.clipboard.writer.copies.lock().unwrap().is_empty());
        assert!(value.notifier.0.lock().unwrap().is_empty());
        assert_eq!(
            value.copy_cached(None, false).await.unwrap(),
            CopyResult::Copied
        );
        assert_eq!(
            value.copy_cached(Some("a"), true).await.unwrap(),
            CopyResult::Copied
        );
        assert_eq!(
            *value.clipboard.writer.copies.lock().unwrap(),
            ["final-a", "original-a"]
        );
        assert_eq!(
            value.copy_cached(Some("missing"), true).await.unwrap(),
            CopyResult::Empty
        );
    }
}

#[tokio::test]
async fn clipboard_exhaustion_preserves_history_and_dedupe_with_visible_fallback() {
    let value = coordinator(Source::default());
    value.clipboard.writer.busy.store(5, Ordering::Release);
    assert_eq!(
        value.handle(&event("a")).await.unwrap(),
        HandlingResult::ClipboardFailed
    );
    assert_eq!(value.clipboard.writer.calls.load(Ordering::Acquire), 5);
    assert_eq!(value.entries().await.unwrap().len(), 1);
    assert_eq!(*value.notifier.0.lock().unwrap(), [Notice::ClipboardFailed]);
    assert_eq!(
        value.handle(&event("a")).await.unwrap(),
        HandlingResult::Duplicate
    );
    assert_eq!(
        value.copy_cached(None, false).await.unwrap(),
        CopyResult::Copied
    );
}

#[tokio::test]
async fn policy_changes_during_fetch_and_clipboard_backoff_suppress_copy() {
    let value = coordinator(Source {
        delay: Duration::from_millis(20),
        ..Default::default()
    });
    let evt = event("a");
    let (handled, ()) = tokio::join!(value.handle(&evt), async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        value.policy.set_paused(true);
    });
    assert_eq!(handled.unwrap(), HandlingResult::CachedSuppressed);
    assert!(value.clipboard.writer.copies.lock().unwrap().is_empty());

    value.policy.set_paused(false);
    value.clipboard.writer.busy.store(1, Ordering::Release);
    let evt = event("b");
    let (handled, ()) = tokio::join!(value.handle(&evt), async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        value.policy.set_dictation_busy(true);
    });
    assert_eq!(handled.unwrap(), HandlingResult::CachedSuppressed);
    assert_eq!(value.clipboard.writer.calls.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn aborted_fetch_is_not_deduped_and_invalid_clipboard_text_is_not_written() {
    let value = coordinator(Source {
        delay: Duration::from_millis(50),
        ..Default::default()
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(5), value.handle(&event("a")))
            .await
            .is_err()
    );
    assert_eq!(
        value.handle(&event("a")).await.unwrap(),
        HandlingResult::CopiedAndNotified
    );
    assert_eq!(
        value.clipboard.copy("contains\0null", None).await,
        Err(ClipboardError::InvalidText)
    );
    assert_eq!(value.clipboard.copy("", None).await, Ok(CopyResult::Empty));
}

#[tokio::test]
async fn real_authenticated_http_fetch_flows_through_cache_to_clipboard() {
    let (base, server) = crate::api::tests::server(vec![(200, crate::api::tests::transcript())]);
    let (source, bearer) = crate::api::tests::make_client(base).await;
    let writer = Writer::default();
    let notifications = Notifications::default();
    let coordinator = TranscriptCoordinator::new(
        source,
        Arc::new(History::default()),
        writer.clone(),
        notifications.clone(),
        Arc::new(DeliveryPolicy::default()),
    );
    assert_eq!(
        coordinator.handle(&event("t")).await.unwrap(),
        HandlingResult::CopiedAndNotified
    );
    assert_eq!(
        coordinator.entries().await.unwrap()[0].body,
        "synthetic transcript"
    );
    assert_eq!(*writer.copies.lock().unwrap(), ["synthetic transcript"]);
    assert_eq!(*notifications.0.lock().unwrap(), [Notice::TranscriptCopied]);
    let requests = server.join().unwrap();
    assert!(requests[0].to_ascii_lowercase().contains(&format!(
        "authorization: bearer {}",
        bearer.to_ascii_lowercase()
    )));
}

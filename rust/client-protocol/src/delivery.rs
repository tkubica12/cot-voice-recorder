use std::error::Error;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use time::OffsetDateTime;
use tokio::sync::Mutex;

use crate::ProtocolError;
use crate::api::{ApiClient, ApiError, Transcript};
use crate::history::{HistoryEntry, should_auto_copy};
use crate::realtime::{EventDeduplicator, TranscriptCompleted};
use crate::runtime::TokenPersistence;

const PAUSED: u8 = 1;
const DICTATION_BUSY: u8 = 2;
const INACTIVE: u8 = 4;

#[derive(Default)]
pub struct DeliveryPolicy(AtomicU8);

impl DeliveryPolicy {
    pub fn set_paused(&self, paused: bool) {
        self.set_flag(PAUSED, paused);
    }

    pub fn set_dictation_busy(&self, busy: bool) {
        self.set_flag(DICTATION_BUSY, busy);
    }

    pub fn set_active(&self, active: bool) {
        self.set_flag(INACTIVE, !active);
    }

    fn set_flag(&self, flag: u8, enabled: bool) {
        if enabled {
            self.0.fetch_or(flag, Ordering::AcqRel);
        } else {
            self.0.fetch_and(!flag, Ordering::AcqRel);
        }
    }

    pub fn paused(&self) -> bool {
        self.0.load(Ordering::Acquire) & PAUSED != 0
    }

    pub fn allows_auto_copy(&self) -> bool {
        let flags = self.0.load(Ordering::Acquire);
        if flags & INACTIVE != 0 {
            return false;
        }
        should_auto_copy(
            flags & PAUSED != 0,
            flags & DICTATION_BUSY != 0,
            "recording",
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardError {
    Busy,
    InvalidText,
    Unavailable,
    Cancelled,
    Platform,
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Clipboard operation failed: {self:?}.")
    }
}

impl Error for ClipboardError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyResult {
    Copied,
    Empty,
    Suppressed,
}

pub trait ClipboardWriter: Send + Sync + 'static {
    // Native adapters must check the policy immediately before touching the clipboard.
    fn write(
        &self,
        text: &str,
        automatic: Option<Arc<DeliveryPolicy>>,
    ) -> impl Future<Output = Result<CopyResult, ClipboardError>> + Send;
}

pub struct ClipboardCopier<W> {
    writer: W,
}

impl<W: ClipboardWriter> ClipboardCopier<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub async fn copy(
        &self,
        text: &str,
        automatic: Option<Arc<DeliveryPolicy>>,
    ) -> Result<CopyResult, ClipboardError> {
        if text.is_empty() {
            return Ok(CopyResult::Empty);
        }
        if text.contains('\0') || text.len() > 16 * 1024 * 1024 {
            return Err(ClipboardError::InvalidText);
        }
        for attempt in 1..=5 {
            if automatic
                .as_ref()
                .is_some_and(|policy| !policy.allows_auto_copy())
            {
                return Ok(CopyResult::Suppressed);
            }
            match self.writer.write(text, automatic.clone()).await {
                Err(ClipboardError::Busy) if attempt < 5 => {
                    tokio::time::sleep(Duration::from_millis(50 * attempt)).await;
                }
                result => return result,
            }
        }
        unreachable!("the last attempt always returns")
    }
}

pub trait HistoryPersistence: Send + Sync + 'static {
    type Error: Error + Send + Sync + 'static;
    fn all(&self, now: OffsetDateTime) -> Result<Vec<HistoryEntry>, Self::Error>;
    fn add(&self, entry: HistoryEntry, now: OffsetDateTime) -> Result<(), Self::Error>;
    fn cleanup(&self, now: OffsetDateTime) -> Result<usize, Self::Error>;
}

pub trait TranscriptSource: Send + Sync + 'static {
    fn fetch(&self, id: &str) -> impl Future<Output = Result<Transcript, ApiError>> + Send;
}

impl<P: TokenPersistence> TranscriptSource for ApiClient<P> {
    async fn fetch(&self, id: &str) -> Result<Transcript, ApiError> {
        self.transcript(id).await
    }
}

impl<S: TranscriptSource> TranscriptSource for Arc<S> {
    async fn fetch(&self, id: &str) -> Result<Transcript, ApiError> {
        self.as_ref().fetch(id).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    TranscriptCopied,
    FetchFailed,
    HistoryFailed,
    ClipboardFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationError;

impl fmt::Display for NotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("The native notification could not be delivered.")
    }
}

impl Error for NotificationError {}

pub trait Notifier: Send + Sync + 'static {
    fn notify(&self, notice: Notice) -> Result<(), NotificationError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlingResult {
    CopiedAndNotified,
    CachedSuppressed,
    Duplicate,
    ClipboardFailed,
}

#[derive(Debug)]
pub enum DeliveryError<E> {
    Fetch(ApiError),
    History(E),
    Worker(tokio::task::JoinError),
    Protocol(ProtocolError),
    Notification(NotificationError),
}

impl<E: fmt::Display> fmt::Display for DeliveryError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(error) => error.fmt(f),
            Self::History(_) => f.write_str("Transcript history could not be persisted or read."),
            Self::Worker(_) => f.write_str("Transcript history worker failed."),
            Self::Protocol(error) => error.fmt(f),
            Self::Notification(error) => error.fmt(f),
        }
    }
}

impl<E: Error + 'static> Error for DeliveryError<E> {}

pub struct TranscriptCoordinator<S, H, W, N> {
    source: S,
    history: Arc<H>,
    clipboard: ClipboardCopier<W>,
    notifier: N,
    policy: Arc<DeliveryPolicy>,
    gate: Mutex<EventDeduplicator>,
}

impl<S: TranscriptSource, H: HistoryPersistence, W: ClipboardWriter, N: Notifier>
    TranscriptCoordinator<S, H, W, N>
{
    pub fn new(
        source: S,
        history: Arc<H>,
        writer: W,
        notifier: N,
        policy: Arc<DeliveryPolicy>,
    ) -> Self {
        Self {
            source,
            history,
            clipboard: ClipboardCopier::new(writer),
            notifier,
            policy,
            gate: Mutex::new(EventDeduplicator::new(2048).expect("fixed bounded capacity")),
        }
    }

    fn notice(&self, notice: Notice) {
        if let Err(error) = self.notifier.notify(notice) {
            eprintln!("Transcript notification: {error}");
        }
    }

    pub async fn handle(
        &self,
        event: &TranscriptCompleted,
    ) -> Result<HandlingResult, DeliveryError<H::Error>> {
        if event.event != "transcript.completed"
            || event.event_id.is_empty()
            || event.event_id.len() > 1024
            || event.transcript_id.is_empty()
            || event.transcript_id.len() > 128
        {
            return Err(DeliveryError::Protocol(ProtocolError::InvalidField(
                "transcript.completed identity",
            )));
        }
        // Serialize the entire delivery, so an older slow fetch cannot overwrite a newer copy.
        let mut dedupe = self.gate.lock().await;
        if dedupe.has_seen(&event.event_id) {
            return Ok(HandlingResult::Duplicate);
        }
        let transcript = match self.source.fetch(&event.transcript_id).await {
            Ok(value) => value,
            Err(error) => {
                self.notice(Notice::FetchFailed);
                return Err(DeliveryError::Fetch(error));
            }
        };
        let now = OffsetDateTime::now_utc();
        let entry = HistoryEntry {
            transcript_id: transcript.transcript_id,
            recording_id: transcript.recording_id,
            preview: transcript.preview,
            raw_body: transcript.raw_body.filter(|raw| raw != &transcript.body),
            body: transcript.body.clone(),
            completed_at: transcript.completed_at,
            cached_at: now,
            character_count: transcript.character_count,
            refinement_fallback_blocks: 0,
        };
        let history = self.history.clone();
        let saved = tokio::task::spawn_blocking(move || history.add(entry, now))
            .await
            .map_err(DeliveryError::Worker);
        match saved {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.notice(Notice::HistoryFailed);
                return Err(DeliveryError::History(error));
            }
            Err(error) => {
                self.notice(Notice::HistoryFailed);
                return Err(error);
            }
        }
        dedupe
            .mark(Some(&event.event_id))
            .map_err(DeliveryError::Protocol)?;
        if transcript.source == "windows_dictation" || !self.policy.allows_auto_copy() {
            return Ok(HandlingResult::CachedSuppressed);
        }
        match self
            .clipboard
            .copy(&transcript.body, Some(self.policy.clone()))
            .await
        {
            Ok(CopyResult::Copied) => {
                self.notifier
                    .notify(Notice::TranscriptCopied)
                    .map_err(DeliveryError::Notification)?;
                Ok(HandlingResult::CopiedAndNotified)
            }
            Ok(CopyResult::Suppressed | CopyResult::Empty) => Ok(HandlingResult::CachedSuppressed),
            Err(error) => {
                eprintln!("Transcript copy: {error}");
                self.notice(Notice::ClipboardFailed);
                Ok(HandlingResult::ClipboardFailed)
            }
        }
    }

    pub async fn entries(&self) -> Result<Vec<HistoryEntry>, DeliveryError<H::Error>> {
        let history = self.history.clone();
        tokio::task::spawn_blocking(move || history.all(OffsetDateTime::now_utc()))
            .await
            .map_err(DeliveryError::Worker)?
            .map_err(DeliveryError::History)
    }

    pub async fn cleanup(&self) -> Result<usize, DeliveryError<H::Error>> {
        let history = self.history.clone();
        tokio::task::spawn_blocking(move || history.cleanup(OffsetDateTime::now_utc()))
            .await
            .map_err(DeliveryError::Worker)?
            .map_err(DeliveryError::History)
    }

    pub async fn copy_cached(
        &self,
        id: Option<&str>,
        original: bool,
    ) -> Result<CopyResult, CachedCopyError<H::Error>> {
        let _guard = self.gate.lock().await;
        let entries = self.entries().await.map_err(CachedCopyError::History)?;
        let entry = match id {
            Some(id) => entries.iter().find(|entry| entry.transcript_id == id),
            None => entries.first(),
        };
        let text = entry.and_then(|entry| {
            if original {
                entry.raw_body.as_deref()
            } else {
                Some(entry.body.as_str())
            }
        });
        self.clipboard
            .copy(text.unwrap_or_default(), None)
            .await
            .map_err(CachedCopyError::Clipboard)
    }
}

#[derive(Debug)]
pub enum CachedCopyError<E> {
    History(DeliveryError<E>),
    Clipboard(ClipboardError),
}

impl<E: fmt::Display> fmt::Display for CachedCopyError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::History(error) => error.fmt(f),
            Self::Clipboard(error) => error.fmt(f),
        }
    }
}

impl<E: Error + 'static> Error for CachedCopyError<E> {}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;

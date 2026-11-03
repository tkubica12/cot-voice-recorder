#[cfg(test)]
use crate::lock_trace::Guard as MutexGuard;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::future::Future;
#[cfg(not(test))]
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, watch};
use voiceprompt_client_protocol::api::{ApiClient, ApiError, ApiErrorKind};
use voiceprompt_client_protocol::dictation_text::{preview_tail, stitch};
use voiceprompt_client_protocol::legacy_checkpoint;
use voiceprompt_client_protocol::recovery::{AudioCheckpoint, RecoveryManifest};
use voiceprompt_client_protocol::runtime::TokenPersistence;
use voiceprompt_core::chunker::{AudioChunk, ChunkerError, PcmChunker};

use crate::{NativeError, NativeRecoveryJournal};

#[cfg(test)]
#[path = "legacy_session_tests.rs"]
mod tests;

pub trait BatchBackend: Send + Sync + 'static {
    fn transcribe(
        &self,
        wav: &[u8],
        language: &str,
        context: &str,
    ) -> impl Future<Output = Result<String, ApiError>> + Send;
}

pub struct BoundBatchBackend<P: TokenPersistence> {
    pub(crate) api: Arc<ApiClient<P>>,
    pub(crate) backend_label: String,
}

impl<P: TokenPersistence> BoundBatchBackend<P> {
    pub fn new(api: Arc<ApiClient<P>>, backend_label: String) -> Result<Self, NativeError> {
        if !api.matches_backend_label(&backend_label) {
            return Err(NativeError::InvalidArgument(
                "Dictation binding does not match the authenticated backend.",
            ));
        }
        Ok(Self { api, backend_label })
    }
}

impl<P: TokenPersistence> BatchBackend for BoundBatchBackend<P> {
    async fn transcribe(
        &self,
        wav: &[u8],
        language: &str,
        context: &str,
    ) -> Result<String, ApiError> {
        let (backend, email) = context
            .split_once('\n')
            .filter(|(backend, email)| *backend == self.backend_label && !email.trim().is_empty())
            .ok_or(ApiError {
                kind: ApiErrorKind::Unauthorized,
                status: Some(401),
                problem: None,
                response_invalid: false,
            })?;
        let _ = backend;
        self.api
            .transcribe_bound_dictation(wav, language, email)
            .await
    }
}

#[derive(Clone)]
pub struct DictationProgress {
    pub captured_bytes: u64,
    pub saved_bytes: u64,
    pub transcribed_bytes: u64,
    pub pending_chunks: usize,
    pub transcript: String,
    pub preview: String,
    pub service_error: Option<ApiErrorKind>,
    pub storage_failed: bool,
    pub capture_complete: bool,
}

impl fmt::Debug for DictationProgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DictationProgress")
            .field("captured_bytes", &self.captured_bytes)
            .field("saved_bytes", &self.saved_bytes)
            .field("transcribed_bytes", &self.transcribed_bytes)
            .field("pending_chunks", &self.pending_chunks)
            .field("storage_failed", &self.storage_failed)
            .field("text", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub enum SessionError {
    Storage(NativeError),
    Audio(ChunkerError),
    Closed,
    Worker,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(
                f,
                "Dictation recovery storage failed; durable data is retained: {error}"
            ),
            Self::Audio(error) => error.fmt(f),
            Self::Closed => f.write_str("Dictation capture is already closed."),
            Self::Worker => f.write_str("Dictation background worker failed."),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(source) => Some(source),
            Self::Audio(source) => Some(source),
            _ => None,
        }
    }
}

struct Retry {
    kind: ApiErrorKind,
    next: Option<tokio::time::Instant>,
    attempts: u32,
}

struct State {
    manifest: RecoveryManifest,
    chunker: PcmChunker,
    captured: u64,
    errors: BTreeMap<u32, Retry>,
    storage_failed: bool,
}

struct Inner<S: BatchBackend> {
    journal: NativeRecoveryJournal,
    service: Arc<S>,
    state: Mutex<State>,
    synchronous_waiters: AtomicUsize,
    #[cfg(test)]
    trace: Arc<crate::lock_trace::Lock>,
    progress: watch::Sender<DictationProgress>,
    wake: Notify,
    cancel: watch::Sender<bool>,
}

pub struct LegacySession<S: BatchBackend> {
    inner: Arc<Inner<S>>,
    pump: Option<tokio::task::JoinHandle<()>>,
}

struct AudioBuffer(Vec<u8>);
impl Drop for AudioBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl<S: BatchBackend> LegacySession<S> {
    pub fn start(journal: NativeRecoveryJournal, service: Arc<S>) -> Result<Self, SessionError> {
        let manifest = journal.snapshot().map_err(SessionError::Storage)?;
        if manifest.streaming {
            return Err(SessionError::Closed);
        }
        let chunker = if manifest.tail.is_empty() {
            PcmChunker::new(120.0).map_err(SessionError::Audio)?
        } else {
            let checkpoint = legacy_checkpoint::parse(&manifest.tail)
                .map_err(|error| SessionError::Storage(NativeError::Protocol(error)))?;
            PcmChunker::restore(checkpoint).map_err(SessionError::Audio)?
        };
        let state = State {
            captured: manifest.captured_bytes,
            manifest,
            chunker,
            errors: BTreeMap::new(),
            storage_failed: false,
        };
        let initial = progress(&state);
        let (sender, _) = watch::channel(initial);
        let (cancel, _) = watch::channel(false);
        let inner = Arc::new(Inner {
            journal,
            service,
            state: Mutex::new(state),
            synchronous_waiters: AtomicUsize::new(0),
            #[cfg(test)]
            trace: crate::lock_trace::Lock::new("legacy-state"),
            progress: sender,
            wake: Notify::new(),
            cancel,
        });
        let worker = inner.clone();
        let pump = tokio::spawn(async move { run(worker).await });
        Ok(Self {
            inner,
            pump: Some(pump),
        })
    }

    pub fn id(&self) -> &str {
        self.inner.journal.id()
    }
    pub fn progress(&self) -> watch::Receiver<DictationProgress> {
        self.inner.progress.subscribe()
    }
    #[cfg(test)]
    pub(crate) fn fixture_lock_trace(&self) -> Arc<crate::lock_trace::Lock> {
        self.inner.trace.clone()
    }

    pub fn append(&self, pcm: &[u8]) -> Result<(), SessionError> {
        let mut state = self.inner.lock()?;
        self.inner.ensure_open(&state)?;
        state.captured = state
            .captured
            .checked_add(pcm.len() as u64)
            .ok_or(SessionError::Audio(ChunkerError::CursorOverflow))?;
        let chunks = state.chunker.append(pcm).map_err(SessionError::Audio)?;
        if !chunks.is_empty() || state.captured - state.manifest.captured_bytes >= 64_000 {
            self.inner.checkpoint(&mut state, chunks, false)?;
        }
        self.inner.publish(&state);
        Ok(())
    }

    pub fn finish_capture(&self) -> Result<(), SessionError> {
        let mut state = self.inner.lock()?;
        if state.manifest.capture_complete {
            return Ok(());
        }
        self.inner.ensure_open(&state)?;
        let chunks = state.chunker.flush().map_err(SessionError::Audio)?;
        self.inner.checkpoint(&mut state, chunks, true)
    }

    pub fn snapshot(&self) -> Result<RecoveryManifest, SessionError> {
        self.inner.journal.snapshot().map_err(SessionError::Storage)
    }

    pub async fn wait_for_completion(
        &self,
        timeout: Duration,
        reset_on_progress: bool,
    ) -> Result<Option<String>, SessionError> {
        let mut status = self.progress();
        let mut until = tokio::time::Instant::now() + timeout;
        let mut previous = status.borrow().transcribed_bytes;
        loop {
            let state = status.borrow_and_update().clone();
            if state.storage_failed {
                return Err(SessionError::Storage(NativeError::Verification(
                    "Recovery storage failed.",
                )));
            }
            if state.capture_complete && state.pending_chunks == 0 {
                return Ok(Some(state.transcript));
            }
            if reset_on_progress && state.transcribed_bytes > previous {
                until = tokio::time::Instant::now() + timeout;
            }
            previous = state.transcribed_bytes;
            tokio::select! {
                _ = tokio::time::sleep_until(until) => return Ok(None),
                result = status.changed() => result.map_err(|_| SessionError::Worker)?,
            }
        }
    }

    pub async fn shutdown(mut self) -> Result<(), SessionError> {
        self.inner.cancel.send_replace(true);
        self.inner.wake.notify_one();
        if let Some(pump) = self.pump.take() {
            pump.await.map_err(|_| SessionError::Worker)?;
        }
        Ok(())
    }
}

impl<S: BatchBackend> Drop for LegacySession<S> {
    fn drop(&mut self) {
        self.inner.cancel.send_replace(true);
        self.inner.wake.notify_one();
    }
}

impl<S: BatchBackend> Inner<S> {
    async fn async_lock(
        &self,
        operation: &'static str,
    ) -> Result<MutexGuard<'_, State>, SessionError> {
        #[cfg(not(test))]
        let _ = operation;
        #[cfg(test)]
        let wait = self.trace.wait(operation);
        loop {
            if self.synchronous_waiters.load(Ordering::Acquire) != 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
                continue;
            }
            match self.state.try_lock() {
                Ok(state) => {
                    #[cfg(test)]
                    return Ok(wait.acquired(state));
                    #[cfg(not(test))]
                    return Ok(state);
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(SessionError::Storage(NativeError::Verification(
                        "Dictation state lock is poisoned.",
                    )));
                }
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[track_caller]
    fn lock(&self) -> Result<MutexGuard<'_, State>, SessionError> {
        #[cfg(test)]
        let wait = self
            .trace
            .wait(&format!("{}", std::panic::Location::caller()));
        self.synchronous_waiters.fetch_add(1, Ordering::AcqRel);
        let result = self.state.lock();
        self.synchronous_waiters.fetch_sub(1, Ordering::AcqRel);
        let guard = result.map_err(|_| {
            SessionError::Storage(NativeError::Verification(
                "Dictation state lock is poisoned.",
            ))
        })?;
        #[cfg(test)]
        return Ok(wait.acquired(guard));
        #[cfg(not(test))]
        Ok(guard)
    }

    fn ensure_open(&self, state: &State) -> Result<(), SessionError> {
        if state.storage_failed {
            return Err(SessionError::Storage(NativeError::Verification(
                "Recovery storage failed.",
            )));
        }
        if state.manifest.capture_complete || *self.cancel.borrow() {
            return Err(SessionError::Closed);
        }
        Ok(())
    }

    fn publish(&self, state: &State) {
        #[cfg(test)]
        let _probe = self.journal.fixture_publish_progress();
        let value = progress(state);
        #[cfg(test)]
        self.trace.progress(
            value.captured_bytes,
            value.saved_bytes,
            value.transcribed_bytes,
        );
        self.progress.send_replace(value);
    }

    fn checkpoint(
        &self,
        state: &mut State,
        mut chunks: Vec<AudioChunk>,
        complete: bool,
    ) -> Result<(), SessionError> {
        let mut tail = if complete {
            Vec::new()
        } else {
            match legacy_checkpoint::encode(&state.chunker.snapshot()) {
                Ok(tail) => tail,
                Err(error) => {
                    for chunk in &mut chunks {
                        chunk.wav.fill(0);
                    }
                    state.storage_failed = true;
                    self.publish(state);
                    return Err(SessionError::Storage(NativeError::Protocol(error)));
                }
            }
        };
        let views: Vec<_> = chunks
            .iter()
            .map(|chunk| AudioCheckpoint {
                wav: &chunk.wav,
                overlaps_previous: chunk.overlaps_previous,
                end_byte: chunk.end_byte,
            })
            .collect();
        let result = self
            .journal
            .checkpoint(
                &tail,
                state.captured,
                &views,
                complete,
                time::OffsetDateTime::now_utc(),
            )
            .and_then(|()| self.journal.snapshot());
        tail.fill(0);
        for chunk in &mut chunks {
            chunk.wav.fill(0);
        }
        match result {
            Ok(manifest) => {
                state.manifest = manifest;
                self.publish(state);
                self.wake.notify_one();
                Ok(())
            }
            Err(error) => {
                eprintln!("Legacy dictation checkpoint: {error}");
                state.storage_failed = true;
                self.publish(state);
                Err(SessionError::Storage(error))
            }
        }
    }
}

fn progress(state: &State) -> DictationProgress {
    let prefix: Vec<_> = state
        .manifest
        .chunks
        .iter()
        .take_while(|chunk| chunk.text.is_some())
        .collect();
    let transcript = stitch(prefix.iter().map(|chunk| {
        (
            chunk.text.as_deref().unwrap_or_default(),
            chunk.overlaps_previous,
        )
    }));
    let pending = state
        .manifest
        .chunks
        .iter()
        .filter(|chunk| chunk.text.is_none())
        .count();
    let through = if state.manifest.capture_complete && pending == 0 {
        state.captured
    } else {
        prefix.last().map_or(0, |chunk| chunk.end_byte)
    };
    DictationProgress {
        captured_bytes: state.captured,
        saved_bytes: state.manifest.captured_bytes,
        transcribed_bytes: through,
        pending_chunks: pending,
        preview: preview_tail(&transcript),
        transcript,
        service_error: state.errors.values().next().map(|error| error.kind),
        storage_failed: state.storage_failed,
        capture_complete: state.manifest.capture_complete,
    }
}

enum JobError {
    Api(ApiErrorKind),
    Storage(NativeError),
    Worker,
    Cancelled,
}

async fn transcribe<S: BatchBackend>(inner: Arc<Inner<S>>, index: u32) -> Result<(), JobError> {
    let mut cancel = inner.cancel.subscribe();
    if *cancel.borrow() {
        return Err(JobError::Cancelled);
    }
    let (language, context, pin) = {
        let state = inner
            .async_lock("legacy-request-context")
            .await
            .map_err(|_| JobError::Worker)?;
        let chunk = state
            .manifest
            .chunks
            .get(index as usize)
            .ok_or(JobError::Worker)?;
        let pin = inner
            .journal
            .pin_audio(chunk, &state.manifest.context)
            .map_err(JobError::Storage)?;
        (
            state.manifest.language.clone(),
            state.manifest.context.clone(),
            pin,
        )
    };
    let audio = tokio::task::spawn_blocking(move || pin.read().map(AudioBuffer))
        .await
        .map_err(|_| JobError::Worker)?
        .map_err(JobError::Storage)?;
    if *cancel.borrow() {
        return Err(JobError::Cancelled);
    }
    let text = tokio::select! {
        biased;
        _ = cancel.changed() => return Err(JobError::Cancelled),
        result = inner.service.transcribe(&audio.0, &language, &context) => result.map_err(|error| {
            eprintln!("Legacy dictation ASR: {error}");
            JobError::Api(error.kind)
        })?,
    };
    drop(audio);
    let writer = inner.clone();
    tokio::task::spawn_blocking(move || {
        writer
            .journal
            .save_result(index, text, time::OffsetDateTime::now_utc())?;
        let mut state = writer
            .lock()
            .map_err(|_| NativeError::Verification("Dictation state lock is poisoned."))?;
        state.manifest = writer.journal.snapshot()?;
        state.errors.remove(&index);
        writer.publish(&state);
        Ok::<_, NativeError>(())
    })
    .await
    .map_err(|_| JobError::Worker)?
    .map_err(JobError::Storage)
}

async fn run<S: BatchBackend>(inner: Arc<Inner<S>>) {
    let mut cancel = inner.cancel.subscribe();
    let mut jobs = tokio::task::JoinSet::new();
    let mut running = HashSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        if *cancel.borrow() {
            break;
        }
        {
            let state = match inner.async_lock("legacy-job-discovery").await {
                Ok(state) => state,
                Err(error) => {
                    eprintln!("Legacy dictation pump: {error}");
                    inner
                        .progress
                        .send_modify(|progress| progress.storage_failed = true);
                    break;
                }
            };
            let ready = state.errors.values().all(|error| {
                error
                    .next
                    .is_some_and(|next| next <= tokio::time::Instant::now())
            });
            if !state.storage_failed && ready {
                for chunk in state
                    .manifest
                    .chunks
                    .iter()
                    .filter(|chunk| chunk.text.is_none())
                {
                    if running.len() == 2 {
                        break;
                    }
                    if running.insert(chunk.index) {
                        let index = chunk.index;
                        let worker = inner.clone();
                        jobs.spawn(async move { (index, transcribe(worker, index).await) });
                    }
                }
            }
        }
        tokio::select! {
            biased;
            _ = cancel.changed() => {},
            Some(result) = jobs.join_next() => {
                match result {
                    Ok((index, outcome)) => {
                        running.remove(&index);
                        if let Err(error) = outcome {
                            let mut state = match inner.async_lock("legacy-job-result").await { Ok(state) => state, Err(_) => break };
                            match error {
                                JobError::Api(kind) => {
                                    let attempts = state.errors.get(&index).map_or(1, |old| old.attempts.saturating_add(1));
                                    let next = (kind == ApiErrorKind::Retryable).then(|| tokio::time::Instant::now() + Duration::from_secs(u64::from(attempts.saturating_mul(2).min(30))));
                                    state.errors.insert(index, Retry { kind, next, attempts });
                                }
                                JobError::Storage(error) => { eprintln!("Legacy dictation storage: {error}"); state.storage_failed = true; }
                                JobError::Worker => { eprintln!("Legacy dictation worker failed."); state.storage_failed = true; }
                                JobError::Cancelled => {}
                            }
                            inner.publish(&state);
                        }
                    }
                    Err(error) => {
                        eprintln!("Legacy dictation task failed: {error}");
                        inner.progress.send_modify(|progress| progress.storage_failed = true);
                        break;
                    }
                }
            }
            _ = inner.wake.notified() => {},
            _ = tick.tick() => {},
        }
    }
    // Network waits cancel cooperatively; already-started journal I/O must join before ownership is released.
    inner.cancel.send_replace(true);
    while let Some(result) = jobs.join_next().await {
        if let Err(error) = result {
            if !error.is_cancelled() {
                eprintln!("Legacy dictation shutdown: {error}");
            }
        }
    }
}

#[cfg(test)]
use crate::lock_trace::Guard as MutexGuard;
use std::fmt;
use std::future::Future;
#[cfg(not(test))]
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, watch};
use voiceprompt_client_protocol::api::{ApiError, ApiErrorKind};
use voiceprompt_client_protocol::dictation_stream::{
    DictationMessage, DictationReceiver, DictationSender,
};
use voiceprompt_client_protocol::dictation_text::{append_preserving_prefix, preview_tail};
use voiceprompt_client_protocol::recovery::{AudioCheckpoint, RecoveryManifest};
use voiceprompt_client_protocol::runtime::TokenPersistence;
use voiceprompt_core::chunker::{OVERLAP_BYTES, PcmChunker};
use voiceprompt_core::wav::to_wav;

use crate::capture_window::{
    AudioBuffer, CaptureSlot, CaptureWindow, PcmState, SealingAudio,
    TRIGGER_BYTES as PCM_BUFFER_BYTES,
};
use crate::legacy_session::{BatchBackend, BoundBatchBackend, DictationProgress, SessionError};
use crate::{NativeError, NativeRecoveryJournal};

#[cfg(test)]
#[path = "streaming_session_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "streaming_longevity_tests.rs"]
mod longevity_tests;

pub trait AudioSender: Clone + Send + Sync + 'static {
    fn audio(&self, pcm: &[u8]) -> impl Future<Output = Result<(), ApiError>> + Send;
    fn commit(&self, finish: bool) -> impl Future<Output = Result<(), ApiError>> + Send;
}

pub trait EventReceiver: Send + 'static {
    fn receive(&mut self) -> impl Future<Output = Result<DictationMessage, ApiError>> + Send;
}

pub trait StreamingBackend: BatchBackend {
    type Sender: AudioSender;
    type Receiver: EventReceiver;
    fn connect(
        &self,
        language: &str,
        start: u64,
        context: &str,
    ) -> impl Future<Output = Result<(Self::Sender, Self::Receiver), ApiError>> + Send;
}

impl AudioSender for DictationSender {
    async fn audio(&self, pcm: &[u8]) -> Result<(), ApiError> {
        DictationSender::audio(self, pcm).await
    }
    async fn commit(&self, finish: bool) -> Result<(), ApiError> {
        DictationSender::commit(self, finish).await
    }
}
impl EventReceiver for DictationReceiver {
    async fn receive(&mut self) -> Result<DictationMessage, ApiError> {
        DictationReceiver::receive(self).await
    }
}
impl<P: TokenPersistence> StreamingBackend for BoundBatchBackend<P> {
    type Sender = DictationSender;
    type Receiver = DictationReceiver;
    async fn connect(
        &self,
        language: &str,
        start: u64,
        context: &str,
    ) -> Result<(Self::Sender, Self::Receiver), ApiError> {
        let (_, email) = context
            .split_once('\n')
            .filter(|(backend, email)| *backend == self.backend_label && !email.trim().is_empty())
            .ok_or(ApiError {
                kind: ApiErrorKind::Unauthorized,
                status: Some(401),
                problem: None,
                response_invalid: false,
            })?;
        Ok(self
            .api
            .connect_bound_dictation(language, start, email)
            .await?
            .split())
    }
}

enum AudioRead {
    Memory(AudioBuffer),
    Pinned {
        pin: crate::recovery_store::PinnedRecoveryAudio,
        start: u64,
    },
}

struct AudioBatch(Vec<voiceprompt_core::chunker::AudioChunk>);
impl Drop for AudioBatch {
    fn drop(&mut self) {
        for chunk in &mut self.0 {
            chunk.wav.fill(0);
        }
    }
}

struct Hypothesis {
    id: String,
    delta: String,
    partial: String,
}

struct State {
    manifest: RecoveryManifest,
    sealing: Option<SealingAudio>,
    cache: Option<(u64, AudioBuffer)>,
    hypotheses: Vec<Hypothesis>,
    service_error: Option<ApiErrorKind>,
}

struct Inner<S: StreamingBackend> {
    journal: NativeRecoveryJournal,
    service: Arc<S>,
    // Durable operations may acquire PCM briefly; PCM owners never wait for durable state.
    state: Mutex<State>,
    capture: Arc<CaptureWindow>,
    synchronous_waiters: AtomicUsize,
    #[cfg(test)]
    trace: Arc<crate::lock_trace::Lock>,
    progress: watch::Sender<DictationProgress>,
    cancel: watch::Sender<bool>,
    wake: Arc<Notify>,
    recovery: bool,
}

pub struct StreamingSession<S: StreamingBackend> {
    inner: Arc<Inner<S>>,
    pump: Option<tokio::task::JoinHandle<()>>,
    sealer: Option<tokio::task::JoinHandle<Result<(), SessionError>>>,
}

#[derive(Debug)]
enum Failure {
    Api(ApiErrorKind),
    Storage(NativeError),
    Worker,
}

async fn seal<S: StreamingBackend>(inner: Arc<Inner<S>>) -> Result<(), SessionError> {
    let mut cancel = inner.cancel.subscribe();
    loop {
        if *cancel.borrow() || inner.capture.storage_failed.load(Ordering::Acquire) {
            return Ok(());
        }
        let requested = {
            let mut pcm = inner.lock_pcm().map_err(session_failure)?;
            std::mem::take(&mut pcm.seal_requested)
        };
        if requested {
            let owner = inner.clone();
            let job = tokio::task::spawn_blocking(move || {
                let mut state = owner.lock_named("capture-seal")?;
                let needed = {
                    let mut pcm = owner.lock_pcm()?;
                    pcm.seal_requested = false;
                    !pcm.closed && pcm.buffer.len() >= PCM_BUFFER_BYTES
                };
                if needed
                    && !*owner.cancel.borrow()
                    && !owner.capture.storage_failed.load(Ordering::Acquire)
                    && !owner.capture.finishing.load(Ordering::Acquire)
                {
                    owner.checkpoint(&mut state, false)?;
                }
                Ok::<_, Failure>(())
            });
            join_seal(&inner, job).await?;
            continue;
        }
        tokio::select! {
            _ = cancel.changed() => {},
            _ = inner.capture.seal.notified() => {},
        }
    }
}

async fn join_seal<S: StreamingBackend>(
    inner: &Inner<S>,
    job: tokio::task::JoinHandle<Result<(), Failure>>,
) -> Result<(), SessionError> {
    let result = job.await.unwrap_or(Err(Failure::Worker));
    if let Err(failure) = result {
        inner.fail();
        eprintln!("Streaming capture sealer: {failure}");
        return Err(session_failure(failure));
    }
    Ok(())
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(kind) => write!(f, "Streaming service failure: {kind:?}; audio retained."),
            Self::Storage(error) => write!(
                f,
                "Streaming recovery storage failed; durable audio retained: {error}"
            ),
            Self::Worker => f.write_str("Streaming task failed; durable audio retained."),
        }
    }
}

impl<S: StreamingBackend> StreamingSession<S> {
    pub fn start(
        journal: NativeRecoveryJournal,
        service: Arc<S>,
        recovery: bool,
    ) -> Result<Self, SessionError> {
        let manifest = journal
            .published_snapshot()
            .map_err(SessionError::Storage)?;
        if !manifest.streaming {
            return Err(SessionError::Closed);
        }
        let captured = manifest.captured_bytes;
        let complete = manifest.capture_complete;
        let state = State {
            manifest,
            sealing: None,
            cache: None,
            hypotheses: Vec::new(),
            service_error: None,
        };
        let (progress, _) = watch::channel(make_progress(&state, captured, false));
        let (cancel, _) = watch::channel(false);
        let wake = Arc::new(Notify::new());
        #[cfg(test)]
        let trace = crate::lock_trace::Lock::new("streaming-state");
        let capture = CaptureWindow::new(
            captured,
            complete,
            progress.clone(),
            cancel.clone(),
            wake.clone(),
            #[cfg(test)]
            trace.clone(),
        );
        let inner = Arc::new(Inner {
            journal,
            service,
            state: Mutex::new(state),
            capture,
            synchronous_waiters: AtomicUsize::new(0),
            #[cfg(test)]
            trace,
            progress,
            cancel,
            wake,
            recovery,
        });
        let worker = inner.clone();
        let pump = tokio::spawn(async move { pump(worker).await });
        let worker = inner.clone();
        let sealer = tokio::spawn(async move { seal(worker).await });
        Ok(Self {
            inner,
            pump: Some(pump),
            sealer: Some(sealer),
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
    pub fn snapshot(&self) -> Result<RecoveryManifest, SessionError> {
        self.inner
            .journal
            .published_snapshot()
            .map_err(SessionError::Storage)
    }

    pub(crate) fn reserve_capture(
        &self,
        discard: &AtomicBool,
    ) -> Result<Option<CaptureSlot>, SessionError> {
        self.inner.capture.reserve(discard)
    }

    pub fn append(&self, pcm: &[u8]) -> Result<(), SessionError> {
        if pcm.len() % 2 != 0 {
            return Err(SessionError::Audio(
                voiceprompt_core::chunker::ChunkerError::IncompleteSample,
            ));
        }
        self.inner.capture.append(pcm)
    }

    pub fn finish_capture(&self) -> Result<(), SessionError> {
        self.inner.capture.finish_requested();
        let mut state = self.inner.lock().map_err(session_failure)?;
        if state.manifest.capture_complete {
            return Ok(());
        }
        self.inner
            .checkpoint(&mut state, true)
            .map_err(session_failure)?;
        self.inner.wake.notify_one();
        Ok(())
    }

    pub fn prepare_audit(
        &self,
        text: String,
        completed: time::OffsetDateTime,
        polished: bool,
    ) -> Result<(), SessionError> {
        self.inner
            .freeze_audit(text, completed, polished)
            .map_err(session_failure)
    }

    pub(crate) async fn prepare_audit_async(
        &self,
        text: String,
        completed: time::OffsetDateTime,
        polished: bool,
    ) -> Result<(), SessionError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.freeze_audit(text, completed, polished))
            .await
            .map_err(|_| SessionError::Worker)?
            .map_err(session_failure)
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
            if state.capture_complete && state.transcribed_bytes == state.captured_bytes {
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
        self.inner.cancel();
        let pump = match self.pump.take() {
            Some(pump) => pump.await.map_err(|_| SessionError::Worker),
            None => Ok(()),
        };
        let sealer = match self.sealer.take() {
            Some(sealer) => sealer
                .await
                .map_err(|_| SessionError::Worker)
                .and_then(|result| result),
            None => Ok(()),
        };
        pump.and(sealer)
    }
}

impl<S: StreamingBackend> Drop for StreamingSession<S> {
    fn drop(&mut self) {
        self.inner.cancel();
    }
}

fn session_failure(failure: Failure) -> SessionError {
    match failure {
        Failure::Storage(error) => SessionError::Storage(error),
        _ => SessionError::Worker,
    }
}

fn capture_failure(error: SessionError) -> Failure {
    match error {
        SessionError::Storage(error) => Failure::Storage(error),
        _ => Failure::Worker,
    }
}

fn make_progress(state: &State, captured: u64, storage_failed: bool) -> DictationProgress {
    let suffix = state
        .hypotheses
        .iter()
        .map(|item| format!("{}{}", item.delta, item.partial))
        .collect::<Vec<_>>()
        .join(" ");
    let preview = append_preserving_prefix(&state.manifest.confirmed_text, &suffix, false, None);
    DictationProgress {
        captured_bytes: captured,
        saved_bytes: state.manifest.captured_bytes,
        transcribed_bytes: state.manifest.confirmed_bytes,
        pending_chunks: state
            .manifest
            .chunks
            .iter()
            .filter(|chunk| chunk.text.is_none())
            .count(),
        transcript: state.manifest.confirmed_text.clone(),
        preview: preview_tail(&preview),
        service_error: state.service_error,
        storage_failed,
        capture_complete: state.manifest.capture_complete,
    }
}

impl<S: StreamingBackend> Inner<S> {
    fn cancel(&self) {
        self.capture.cancel();
    }

    fn fail(&self) {
        self.capture.fail();
    }

    fn lock_pcm(&self) -> Result<std::sync::MutexGuard<'_, PcmState>, Failure> {
        self.capture.lock().map_err(capture_failure)
    }

    async fn async_lock(&self, operation: &'static str) -> Result<MutexGuard<'_, State>, Failure> {
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
                    self.fail();
                    return Err(Failure::Storage(NativeError::Verification(
                        "Streaming state lock is poisoned.",
                    )));
                }
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[track_caller]
    fn lock(&self) -> Result<MutexGuard<'_, State>, Failure> {
        self.lock_named("")
    }

    #[track_caller]
    fn lock_named(&self, operation: &'static str) -> Result<MutexGuard<'_, State>, Failure> {
        #[cfg(not(test))]
        let _ = operation;
        #[cfg(test)]
        let mutation_wait = crate::mutation_trace::span("engine-lock-wait");
        #[cfg(test)]
        let wait = {
            let operation = if operation.is_empty() {
                format!("{}", std::panic::Location::caller())
            } else {
                operation.to_owned()
            };
            self.trace.wait(&operation)
        };
        self.synchronous_waiters.fetch_add(1, Ordering::AcqRel);
        let result = self.state.lock();
        self.synchronous_waiters.fetch_sub(1, Ordering::AcqRel);
        let guard = result.map_err(|_| {
            self.fail();
            Failure::Storage(NativeError::Verification(
                "Streaming state lock is poisoned.",
            ))
        })?;
        #[cfg(test)]
        drop(mutation_wait);
        #[cfg(test)]
        return Ok(wait.acquired(guard));
        #[cfg(not(test))]
        Ok(guard)
    }

    fn freeze_audit(
        &self,
        text: String,
        completed: time::OffsetDateTime,
        polished: bool,
    ) -> Result<(), Failure> {
        let mut state = self.lock_named("prepare-audit")?;
        state.manifest = self
            .journal
            .prepare_audit_snapshot(text, completed, polished, time::OffsetDateTime::now_utc())
            .map_err(Failure::Storage)?;
        self.publish(&state);
        Ok(())
    }
    fn publish(&self, state: &State) {
        #[cfg(test)]
        let _mutation = crate::mutation_trace::span("cursor-progress-publication");
        #[cfg(test)]
        let _probe = self.journal.fixture_publish_progress();
        let value = make_progress(
            state,
            self.capture.captured.load(Ordering::Acquire),
            self.capture.storage_failed.load(Ordering::Acquire),
        );
        self.capture.publish_progress(value);
    }

    fn checkpoint(&self, state: &mut State, complete: bool) -> Result<(), Failure> {
        if self.capture.storage_failed.load(Ordering::Acquire) {
            return Err(Failure::Storage(NativeError::Verification(
                "Recovery storage failed.",
            )));
        }
        if state.sealing.is_some() {
            self.fail();
            return Err(Failure::Storage(NativeError::Verification(
                "Streaming PCM publication boundary is invalid.",
            )));
        }
        let batch = self
            .capture
            .detach(state.manifest.captured_bytes, complete)
            .map_err(capture_failure)?;
        let end = batch.end;
        if !batch.pcm.0.is_empty() {
            state.sealing = Some(batch);
        }
        let result = (|| {
            let audio = {
                #[cfg(test)]
                let _mutation = crate::mutation_trace::span("wav-encode");
                state
                    .sealing
                    .as_ref()
                    .map(|sealing| {
                        to_wav(&sealing.pcm.0)
                            .map(AudioBuffer)
                            .map_err(|_| Failure::Worker)
                    })
                    .transpose()?
            };
            let chunks: Vec<_> = audio
                .iter()
                .map(|audio| AudioCheckpoint {
                    wav: &audio.0,
                    end_byte: state.sealing.as_ref().map_or(end, |sealing| sealing.end),
                    overlaps_previous: false,
                })
                .collect();
            self.journal
                .checkpoint_snapshot(
                    &state.manifest.tail,
                    end,
                    &chunks,
                    complete,
                    time::OffsetDateTime::now_utc(),
                )
                .map_err(Failure::Storage)
        })();
        match result {
            Ok(manifest) => {
                #[cfg(test)]
                let _mutation = crate::mutation_trace::span("manifest-cursor-update");
                state.manifest = manifest;
                state.sealing = None;
                self.capture.published(end).map_err(capture_failure)?;
                self.publish(state);
                Ok(())
            }
            Err(error) => {
                self.fail();
                self.publish(state);
                Err(error)
            }
        }
    }

    fn plan_read(
        &self,
        state: &mut State,
        position: u64,
        maximum: usize,
    ) -> Result<AudioRead, Failure> {
        if self.capture.storage_failed.load(Ordering::Acquire) {
            return Err(Failure::Storage(NativeError::Verification(
                "Recovery storage failed.",
            )));
        }
        let pcm = self.lock_pcm()?;
        if position >= pcm.buffer_start {
            let offset =
                usize::try_from(position - pcm.buffer_start).map_err(|_| Failure::Worker)?;
            if offset >= pcm.buffer.len() {
                return Ok(AudioRead::Memory(AudioBuffer(Vec::new())));
            }
            return Ok(AudioRead::Memory(AudioBuffer(
                pcm.buffer[offset..offset + maximum.min(pcm.buffer.len() - offset)].to_vec(),
            )));
        }
        drop(pcm);
        let cached = state.cache.as_ref().is_some_and(|(start, audio)| {
            position >= *start && position < *start + (audio.0.len() - 44) as u64
        });
        if !cached {
            let chunk = state
                .manifest
                .chunks
                .iter()
                .find(|chunk| chunk.end_byte > position)
                .ok_or(Failure::Storage(NativeError::Verification(
                    "Missing recovery audio range.",
                )))?;
            let start = chunk.end_byte - u64::from(chunk.audio_bytes - 44);
            let pin = self
                .journal
                .pin_audio(chunk, &state.manifest.context)
                .map_err(Failure::Storage)?;
            return Ok(AudioRead::Pinned { pin, start });
        }
        let (start, audio) = state.cache.as_ref().ok_or(Failure::Worker)?;
        let offset = usize::try_from(position.checked_sub(*start).ok_or(Failure::Worker)?)
            .map_err(|_| Failure::Worker)?;
        let bytes = audio.0.len() - 44;
        if offset >= bytes {
            return Err(Failure::Worker);
        }
        Ok(AudioRead::Memory(AudioBuffer(
            audio.0[44 + offset..44 + offset + maximum.min(bytes - offset)].to_vec(),
        )))
    }

    fn confirm(
        &self,
        state: &mut State,
        through: u64,
        text: String,
        fallback: bool,
        words: u32,
        context: Vec<u8>,
    ) -> Result<(), Failure> {
        if through <= state.manifest.confirmed_bytes || through > state.manifest.captured_bytes {
            return Err(Failure::Api(ApiErrorKind::Unexpected));
        }
        let manifest = self.journal.confirm_streaming_snapshot(
            through,
            text,
            context,
            fallback,
            Some(words),
            time::OffsetDateTime::now_utc(),
        );
        state.manifest = match manifest {
            Ok(manifest) => manifest,
            Err(error) => {
                self.fail();
                self.publish(state);
                return Err(Failure::Storage(error));
            }
        };
        state.cache = None;
        self.publish(state);
        Ok(())
    }
}

async fn read_range<S: StreamingBackend>(
    inner: Arc<Inner<S>>,
    position: u64,
    maximum: usize,
) -> Result<AudioBuffer, Failure> {
    let plan = {
        let mut state = inner.async_lock("send-audio-read").await?;
        inner.plan_read(&mut state, position, maximum)?
    };
    match plan {
        AudioRead::Memory(bytes) => Ok(bytes),
        AudioRead::Pinned { pin, start } => {
            let audio = tokio::task::spawn_blocking(move || pin.read().map(AudioBuffer))
                .await
                .map_err(|_| Failure::Worker)?
                .map_err(Failure::Storage)?;
            let offset = usize::try_from(position.checked_sub(start).ok_or(Failure::Worker)?)
                .map_err(|_| Failure::Worker)?;
            let bytes = audio.0.len().checked_sub(44).ok_or(Failure::Worker)?;
            if offset >= bytes {
                return Err(Failure::Worker);
            }
            let result = AudioBuffer(
                audio.0[44 + offset..44 + offset + maximum.min(bytes - offset)].to_vec(),
            );
            let mut state = inner.async_lock("read-audio-cache").await?;
            if start + bytes as u64 > state.manifest.confirmed_bytes {
                state.cache = Some((start, audio));
            }
            Ok(result)
        }
    }
}

async fn context_at<S: StreamingBackend>(
    inner: Arc<Inner<S>>,
    through: u64,
) -> Result<Vec<u8>, Failure> {
    let mut position = through.saturating_sub(OVERLAP_BYTES as u64);
    let mut context = AudioBuffer(Vec::new());
    while position < through {
        let memory = {
            let state = inner.async_lock("confirmation-context").await?;
            if position < state.manifest.confirmed_bytes {
                let start = state.manifest.confirmed_bytes - state.manifest.tail.len() as u64;
                if position < start {
                    return Err(Failure::Storage(NativeError::Verification(
                        "Missing confirmed recovery context.",
                    )));
                }
                let count = (through.min(state.manifest.confirmed_bytes) - position) as usize;
                let offset = (position - start) as usize;
                Some(AudioBuffer(
                    state.manifest.tail[offset..offset + count].to_vec(),
                ))
            } else {
                None
            }
        };
        let part = match memory {
            Some(bytes) => bytes,
            None => read_range(inner.clone(), position, (through - position) as usize).await?,
        };
        if part.0.is_empty() {
            return Err(Failure::Storage(NativeError::Verification(
                "Missing recovery context samples.",
            )));
        }
        position += part.0.len() as u64;
        context.0.extend_from_slice(&part.0);
    }
    Ok(std::mem::take(&mut context.0))
}

async fn send_audio<S: StreamingBackend>(
    inner: Arc<Inner<S>>,
    sender: S::Sender,
    start: u64,
    committed_to: Arc<AtomicU64>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Failure> {
    let mut position = start;
    let mut committed = start;
    let mut quiet = 0u64;
    let began = tokio::time::Instant::now();
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let audio = read_range(inner.clone(), position, 640).await?;
        if *stop.borrow() {
            return Ok(());
        }
        if !audio.0.is_empty() {
            let energy: f64 = audio
                .0
                .chunks_exact(2)
                .map(|sample| f64::from(i16::from_le_bytes([sample[0], sample[1]])).powi(2))
                .sum();
            quiet = if energy / ((audio.0.len() / 2) as f64) < 120.0 * 120.0 {
                quiet + audio.0.len() as u64
            } else {
                0
            };
            tokio::select! {
                biased;
                _ = stop.changed() => return Ok(()),
                result = sender.audio(&audio.0) => result.map_err(|error| Failure::Api(error.kind))?,
            };
            position += audio.0.len() as u64;
            if quiet >= 12_800 && position - committed >= 64_000 {
                if !checkpoint_for_send(
                    inner.clone(),
                    stop.clone(),
                    position,
                    "send-audio-checkpoint",
                )
                .await?
                {
                    return Ok(());
                }
                committed_to.store(position, Ordering::Release);
                tokio::select! {
                    biased;
                    _ = stop.changed() => return Ok(()),
                    result = sender.commit(false) => result.map_err(|error| Failure::Api(error.kind))?,
                };
                committed = position;
                quiet = 0;
            }
        } else {
            let complete = {
                let state = inner.async_lock("send-audio-complete").await?;
                state.manifest.capture_complete
                    && position == inner.capture.captured.load(Ordering::Acquire)
            };
            if complete {
                break;
            }
            tokio::select! { _ = stop.changed() => return Ok(()), _ = inner.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
        }
        if began.elapsed() >= Duration::from_secs(50 * 60) {
            break;
        }
    }
    if !checkpoint_for_send(inner, stop.clone(), position, "send-audio-finish").await? {
        return Ok(());
    }
    committed_to.store(position, Ordering::Release);
    tokio::select! {
        biased;
        _ = stop.changed() => Ok(()),
        result = sender.commit(true) => result.map_err(|error| Failure::Api(error.kind)),
    }
}

async fn checkpoint_for_send<S: StreamingBackend>(
    inner: Arc<Inner<S>>,
    stop: watch::Receiver<bool>,
    through: u64,
    operation: &'static str,
) -> Result<bool, Failure> {
    // One joined mutation per send loop; acquire its state lock on the blocking worker.
    tokio::task::spawn_blocking(move || {
        if *stop.borrow() {
            return Ok(false);
        }
        let mut state = inner.lock_named(operation)?;
        if *stop.borrow() {
            return Ok(false);
        }
        if through > inner.capture.captured.load(Ordering::Acquire) {
            return Err(Failure::Api(ApiErrorKind::Unexpected));
        }
        if inner.capture.storage_failed.load(Ordering::Acquire) {
            return Err(Failure::Storage(NativeError::Verification(
                "Recovery storage failed.",
            )));
        }
        if state.manifest.captured_bytes < through {
            inner.checkpoint(&mut state, false)?;
        }
        if state.manifest.captured_bytes < through {
            return Err(Failure::Storage(NativeError::Verification(
                "Streaming commit is not durably covered.",
            )));
        }
        Ok(true)
    })
    .await
    .map_err(|_| Failure::Worker)?
}

async fn receive_text<S: StreamingBackend>(
    inner: Arc<Inner<S>>,
    mut receiver: S::Receiver,
    committed_to: Arc<AtomicU64>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Failure> {
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let event = tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            result = receiver.receive() => result.map_err(|error| Failure::Api(error.kind))?,
        };
        let delta = matches!(&event, DictationMessage::Delta { .. });
        let context = if let DictationMessage::Confirmed { byte_end, .. } = &event {
            if *byte_end > committed_to.load(Ordering::Acquire) {
                return Err(Failure::Api(ApiErrorKind::Unexpected));
            }
            Some(context_at(inner.clone(), *byte_end).await?)
        } else {
            None
        };
        if *stop.borrow() {
            return Ok(());
        }
        let event = match event {
            DictationMessage::Confirmed {
                item_id,
                text,
                byte_end,
            } => {
                let writer = inner.clone();
                let stopped = stop.clone();
                let limit = committed_to.clone();
                let context = context.expect("Confirmed message owns its bounded context.");
                tokio::task::spawn_blocking(move || {
                    if *stopped.borrow() {
                        return Ok(());
                    }
                    let mut state = writer.lock_named("receive-transcript-confirm")?;
                    if *stopped.borrow() {
                        return Ok(());
                    }
                    if byte_end > limit.load(Ordering::Acquire) {
                        return Err(Failure::Api(ApiErrorKind::Unexpected));
                    }
                    state.service_error = None;
                    let words = text.split_whitespace().count() as u32;
                    let full = append_preserving_prefix(
                        &state.manifest.confirmed_text,
                        &text,
                        false,
                        None,
                    );
                    writer.confirm(&mut state, byte_end, full, false, words, context)?;
                    state
                        .hypotheses
                        .retain(|hypothesis| hypothesis.id != item_id);
                    writer.publish(&state);
                    Ok(())
                })
                .await
                .map_err(|_| Failure::Worker)??;
                continue;
            }
            event => event,
        };
        let mut state = inner.async_lock("receive-transcript").await?;
        state.service_error = None;
        match event {
            DictationMessage::Done { byte_end } => {
                if byte_end != state.manifest.confirmed_bytes {
                    return Err(Failure::Api(ApiErrorKind::Unexpected));
                }
                inner.publish(&state);
                return Ok(());
            }
            DictationMessage::Delta { item_id, text }
            | DictationMessage::Intermediate { item_id, text } => {
                let index = if let Some(index) =
                    state.hypotheses.iter().position(|item| item.id == item_id)
                {
                    index
                } else {
                    if state.hypotheses.len() == 32 {
                        return Err(Failure::Api(ApiErrorKind::Unexpected));
                    }
                    state.hypotheses.push(Hypothesis {
                        id: item_id,
                        delta: String::new(),
                        partial: String::new(),
                    });
                    state.hypotheses.len() - 1
                };
                let hypothesis = &mut state.hypotheses[index];
                if delta {
                    hypothesis.delta.push_str(&text);
                    hypothesis.partial.clear();
                } else {
                    hypothesis.partial = text;
                }
                if hypothesis.delta.encode_utf16().count()
                    + hypothesis.partial.encode_utf16().count()
                    > 1_048_576
                {
                    return Err(Failure::Api(ApiErrorKind::Unexpected));
                }
            }
            _ => return Err(Failure::Api(ApiErrorKind::Unexpected)),
        }
        inner.publish(&state);
    }
}

async fn stream<S: StreamingBackend>(
    inner: Arc<Inner<S>>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), Failure> {
    let (language, context, start) = {
        let mut state = inner.async_lock("connect-state").await?;
        state.hypotheses.clear();
        state.cache = None;
        (
            state.manifest.language.clone(),
            state.manifest.context.clone(),
            state.manifest.confirmed_bytes,
        )
    };
    let (sender, receiver) = tokio::select! {
        result = inner.service.connect(&language, start, &context) => result.map_err(|error| Failure::Api(error.kind))?,
        _ = cancel.changed() => return Ok(()),
    };
    let committed = Arc::new(AtomicU64::new(start));
    let (stop, stopped) = watch::channel(false);
    let mut sending = tokio::spawn(send_audio(
        inner.clone(),
        sender,
        start,
        committed.clone(),
        stopped.clone(),
    ));
    let mut receiving = tokio::spawn(receive_text(inner, receiver, committed, stopped));
    let mut sent = false;
    let mut received = false;
    let result = tokio::select! {
        _ = cancel.changed() => Ok(()),
        result = &mut receiving => { received = true; join_result(result) },
        result = &mut sending => {
            sent = true;
            match join_result(result) {
                Err(error) => Err(error),
                Ok(()) => tokio::select! {
                    _ = cancel.changed() => Ok(()),
                    result = tokio::time::timeout(Duration::from_secs(30), &mut receiving) => {
                        match result {
                            Ok(result) => { received = true; join_result(result) },
                            Err(_) => Err(Failure::Api(ApiErrorKind::Retryable)),
                        }
                    }
                },
            }
        }
    };
    stop.send_replace(true);
    if !sent {
        let _ = sending.await;
    }
    if !received {
        let _ = receiving.await;
    }
    result
}

fn join_result(result: Result<Result<(), Failure>, tokio::task::JoinError>) -> Result<(), Failure> {
    match result {
        Ok(result) => result,
        Err(_) => Err(Failure::Worker),
    }
}

async fn fallback<S: StreamingBackend>(
    inner: Arc<Inner<S>>,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Failure> {
    let (cursor, context, captured, language, binding) = {
        let state = inner.async_lock("fallback-snapshot").await?;
        (
            state.manifest.confirmed_bytes,
            AudioBuffer(state.manifest.tail.clone()),
            inner.capture.captured.load(Ordering::Acquire),
            state.manifest.language.clone(),
            state.manifest.context.clone(),
        )
    };
    if cursor == captured {
        return Ok(());
    }
    let start = cursor - context.0.len() as u64;
    let mut chunker = PcmChunker::new(0.0).map_err(|_| Failure::Worker)?;
    let mut first = true;
    let mut position = cursor;
    let mut batches = AudioBatch(chunker.append(&context.0).map_err(|_| Failure::Worker)?);
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        for chunk in &mut batches.0 {
            let audio = AudioBuffer(std::mem::take(&mut chunk.wav));
            let text = tokio::select! {
                biased;
                _ = stop.changed() => return Ok(()),
                result = inner.service.transcribe(&audio.0, &language, &binding) => result,
            };
            let text = text.map_err(|error| Failure::Api(error.kind))?;
            let through = start + chunk.end_byte;
            let next_context = context_at(inner.clone(), through).await?;
            if *stop.borrow() {
                return Ok(());
            }
            let writer = inner.clone();
            let stopped = stop.clone();
            let overlap = chunk.overlaps_previous || (first && !context.0.is_empty());
            let applied = tokio::task::spawn_blocking(move || {
                if *stopped.borrow() {
                    return Ok(false);
                }
                let mut state = writer.lock_named("fallback-confirm")?;
                if *stopped.borrow() {
                    return Ok(false);
                }
                let full = append_preserving_prefix(
                    &state.manifest.confirmed_text,
                    &text,
                    overlap,
                    Some(state.manifest.last_segment_words as usize),
                );
                writer.confirm(
                    &mut state,
                    through,
                    full,
                    true,
                    text.split_whitespace().count() as u32,
                    next_context,
                )?;
                Ok(true)
            })
            .await
            .map_err(|_| Failure::Worker)??;
            if !applied {
                return Ok(());
            }
            first = false;
        }
        if position == captured {
            break;
        }
        let audio = read_range(inner.clone(), position, 64_000).await?;
        if audio.0.is_empty() {
            return Err(Failure::Storage(NativeError::Verification(
                "Missing pending recovery audio.",
            )));
        }
        position += audio.0.len() as u64;
        batches = AudioBatch(chunker.append(&audio.0).map_err(|_| Failure::Worker)?);
        if position == captured {
            batches
                .0
                .extend(chunker.flush().map_err(|_| Failure::Worker)?);
        }
    }
    Ok(())
}

async fn pump<S: StreamingBackend>(inner: Arc<Inner<S>>) {
    let mut cancel = inner.cancel.subscribe();
    loop {
        if *cancel.borrow() {
            return;
        }
        let complete = match inner.async_lock("pump-status").await {
            Ok(state) => state.manifest.capture_complete,
            Err(error) => {
                eprintln!("Streaming state: {error}");
                inner
                    .progress
                    .send_modify(|progress| progress.storage_failed = true);
                return;
            }
        };
        let result = if inner.recovery {
            if !complete {
                tokio::select! { _ = cancel.changed() => {}, _ = inner.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
                continue;
            }
            fallback(inner.clone(), cancel.clone()).await
        } else {
            let done = match inner.async_lock("pump-verify").await {
                Ok(state) => {
                    state.manifest.capture_complete
                        && state.manifest.confirmed_bytes
                            == inner.capture.captured.load(Ordering::Acquire)
                }
                Err(_) => false,
            };
            if done {
                return;
            }
            stream(inner.clone(), &mut cancel).await
        };
        if *cancel.borrow() {
            return;
        }
        match result {
            Ok(()) if inner.recovery => return,
            Ok(()) => {}
            Err(error) => {
                eprintln!("Streaming dictation: {error}");
                let kind = match error {
                    Failure::Api(kind) => Some(kind),
                    _ => None,
                };
                if let Ok(mut state) = inner.async_lock("pump-error").await {
                    state.hypotheses.clear();
                    state.service_error = kind;
                    if kind.is_none() {
                        inner.fail();
                    }
                    inner.publish(&state);
                }
                if kind.is_none() {
                    return;
                }
                if matches!(
                    kind,
                    Some(ApiErrorKind::Unauthorized | ApiErrorKind::Forbidden)
                ) {
                    let _ = cancel.changed().await;
                    return;
                }
                tokio::select! { _ = cancel.changed() => {}, _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
            }
        }
    }
}

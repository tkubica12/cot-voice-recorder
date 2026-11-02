use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{Notify, watch};
use voiceprompt_client_protocol::api::{ApiError, ApiErrorKind};
use voiceprompt_client_protocol::dictation_stream::{
    DictationMessage, DictationReceiver, DictationSender,
};
use voiceprompt_client_protocol::dictation_text::{append_preserving_prefix, preview_tail};
use voiceprompt_client_protocol::recovery::{
    AudioCheckpoint, MAX_CAPTURED_BYTES, RecoveryManifest,
};
use voiceprompt_client_protocol::runtime::TokenPersistence;
use voiceprompt_core::chunker::{OVERLAP_BYTES, PcmChunker};
use voiceprompt_core::wav::to_wav;

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

struct AudioBuffer(Vec<u8>);
impl Drop for AudioBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
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
    captured: u64,
    buffer: Vec<u8>,
    cache: Option<(u64, AudioBuffer)>,
    hypotheses: Vec<Hypothesis>,
    service_error: Option<ApiErrorKind>,
    storage_failed: bool,
}

impl Drop for State {
    fn drop(&mut self) {
        self.buffer.fill(0);
    }
}

struct Inner<S: StreamingBackend> {
    journal: NativeRecoveryJournal,
    service: Arc<S>,
    state: Mutex<State>,
    progress: watch::Sender<DictationProgress>,
    cancel: watch::Sender<bool>,
    wake: Notify,
    recovery: bool,
}

pub struct StreamingSession<S: StreamingBackend> {
    inner: Arc<Inner<S>>,
    pump: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug)]
enum Failure {
    Api(ApiErrorKind),
    Storage(NativeError),
    Worker,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(kind) => write!(f, "Streaming service failure: {kind:?}; audio retained."),
            Self::Storage(_) => {
                f.write_str("Streaming recovery storage failed; durable audio retained.")
            }
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
        let manifest = journal.snapshot().map_err(SessionError::Storage)?;
        if !manifest.streaming {
            return Err(SessionError::Closed);
        }
        let state = State {
            captured: manifest.captured_bytes,
            manifest,
            buffer: Vec::with_capacity(64_000),
            cache: None,
            hypotheses: Vec::new(),
            service_error: None,
            storage_failed: false,
        };
        let (progress, _) = watch::channel(make_progress(&state));
        let (cancel, _) = watch::channel(false);
        let inner = Arc::new(Inner {
            journal,
            service,
            state: Mutex::new(state),
            progress,
            cancel,
            wake: Notify::new(),
            recovery,
        });
        let worker = inner.clone();
        let pump = tokio::spawn(async move { pump(worker).await });
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
    pub fn snapshot(&self) -> Result<RecoveryManifest, SessionError> {
        self.inner.journal.snapshot().map_err(SessionError::Storage)
    }

    pub fn append(&self, mut pcm: &[u8]) -> Result<(), SessionError> {
        if pcm.len() % 2 != 0 {
            return Err(SessionError::Audio(
                voiceprompt_core::chunker::ChunkerError::IncompleteSample,
            ));
        }
        let mut state = self.inner.lock().map_err(session_failure)?;
        if state.manifest.capture_complete || *self.inner.cancel.borrow() {
            return Err(SessionError::Closed);
        }
        if state.storage_failed {
            return Err(SessionError::Storage(NativeError::Verification(
                "Recovery storage failed.",
            )));
        }
        if state
            .captured
            .checked_add(pcm.len() as u64)
            .is_none_or(|end| end > MAX_CAPTURED_BYTES)
        {
            return Err(SessionError::Storage(NativeError::Verification(
                "Recovery captured-byte limit exceeded.",
            )));
        }
        while !pcm.is_empty() {
            let count = (64_000 - state.buffer.len()).min(pcm.len());
            state.buffer.extend_from_slice(&pcm[..count]);
            state.captured += count as u64;
            pcm = &pcm[count..];
            if state.buffer.len() == 64_000 {
                self.inner
                    .checkpoint(&mut state, false)
                    .map_err(session_failure)?;
            }
        }
        self.inner.publish(&state);
        self.inner.wake.notify_one();
        Ok(())
    }

    pub fn finish_capture(&self) -> Result<(), SessionError> {
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
        let mut state = self.inner.lock().map_err(session_failure)?;
        self.inner
            .journal
            .prepare_audit(text, completed, polished, time::OffsetDateTime::now_utc())
            .map_err(SessionError::Storage)?;
        state.manifest = self
            .inner
            .journal
            .snapshot()
            .map_err(SessionError::Storage)?;
        self.inner.publish(&state);
        Ok(())
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
        self.inner.cancel.send_replace(true);
        self.inner.wake.notify_one();
        if let Some(pump) = self.pump.take() {
            pump.await.map_err(|_| SessionError::Worker)?;
        }
        Ok(())
    }
}

impl<S: StreamingBackend> Drop for StreamingSession<S> {
    fn drop(&mut self) {
        self.inner.cancel.send_replace(true);
        self.inner.wake.notify_one();
    }
}

fn session_failure(failure: Failure) -> SessionError {
    match failure {
        Failure::Storage(error) => SessionError::Storage(error),
        _ => SessionError::Worker,
    }
}

fn make_progress(state: &State) -> DictationProgress {
    let suffix = state
        .hypotheses
        .iter()
        .map(|item| format!("{}{}", item.delta, item.partial))
        .collect::<Vec<_>>()
        .join(" ");
    let preview = append_preserving_prefix(&state.manifest.confirmed_text, &suffix, false, None);
    DictationProgress {
        captured_bytes: state.captured,
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
        storage_failed: state.storage_failed,
        capture_complete: state.manifest.capture_complete,
    }
}

impl<S: StreamingBackend> Inner<S> {
    fn lock(&self) -> Result<MutexGuard<'_, State>, Failure> {
        self.state.lock().map_err(|_| {
            Failure::Storage(NativeError::Verification(
                "Streaming state lock is poisoned.",
            ))
        })
    }
    fn publish(&self, state: &State) {
        self.progress.send_replace(make_progress(state));
    }

    fn checkpoint(&self, state: &mut State, complete: bool) -> Result<(), Failure> {
        if state.storage_failed {
            return Err(Failure::Storage(NativeError::Verification(
                "Recovery storage failed.",
            )));
        }
        let audio = if state.buffer.is_empty() {
            None
        } else {
            Some(AudioBuffer(
                to_wav(&state.buffer).map_err(|_| Failure::Worker)?,
            ))
        };
        let chunks: Vec<_> = audio
            .iter()
            .map(|audio| AudioCheckpoint {
                wav: &audio.0,
                end_byte: state.captured,
                overlaps_previous: false,
            })
            .collect();
        let result = self
            .journal
            .checkpoint(
                &state.manifest.tail,
                state.captured,
                &chunks,
                complete,
                time::OffsetDateTime::now_utc(),
            )
            .and_then(|()| self.journal.snapshot());
        match result {
            Ok(manifest) => {
                state.manifest = manifest;
                state.buffer.fill(0);
                state.buffer.clear();
                self.publish(state);
                Ok(())
            }
            Err(error) => {
                state.storage_failed = true;
                self.publish(state);
                Err(Failure::Storage(error))
            }
        }
    }

    fn read_range(
        &self,
        state: &mut State,
        position: u64,
        maximum: usize,
    ) -> Result<AudioBuffer, Failure> {
        let sealed = state
            .manifest
            .chunks
            .last()
            .map_or(0, |chunk| chunk.end_byte);
        if position >= sealed {
            let offset = usize::try_from(position - sealed).map_err(|_| Failure::Worker)?;
            if offset >= state.buffer.len() {
                return Ok(AudioBuffer(Vec::new()));
            }
            return Ok(AudioBuffer(
                state.buffer[offset..offset + maximum.min(state.buffer.len() - offset)].to_vec(),
            ));
        }
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
            let audio = self
                .journal
                .read_audio(chunk.index)
                .map_err(Failure::Storage)?;
            state.cache = Some((start, AudioBuffer(audio)));
        }
        let (start, audio) = state.cache.as_ref().ok_or(Failure::Worker)?;
        let offset = usize::try_from(position.checked_sub(*start).ok_or(Failure::Worker)?)
            .map_err(|_| Failure::Worker)?;
        let bytes = audio.0.len() - 44;
        if offset >= bytes {
            return Err(Failure::Worker);
        }
        Ok(AudioBuffer(
            audio.0[44 + offset..44 + offset + maximum.min(bytes - offset)].to_vec(),
        ))
    }

    fn context_at(&self, state: &mut State, through: u64) -> Result<Vec<u8>, Failure> {
        let mut position = through.saturating_sub(OVERLAP_BYTES as u64);
        let mut context = AudioBuffer(Vec::new());
        while position < through {
            let part = if position < state.manifest.confirmed_bytes {
                let start = state.manifest.confirmed_bytes - state.manifest.tail.len() as u64;
                if position < start {
                    return Err(Failure::Storage(NativeError::Verification(
                        "Missing confirmed recovery context.",
                    )));
                }
                let count = (through.min(state.manifest.confirmed_bytes) - position) as usize;
                let offset = (position - start) as usize;
                AudioBuffer(state.manifest.tail[offset..offset + count].to_vec())
            } else {
                self.read_range(state, position, (through - position) as usize)?
            };
            if part.0.is_empty() {
                return Err(Failure::Storage(NativeError::Verification(
                    "Missing recovery context samples.",
                )));
            }
            context.0.extend_from_slice(&part.0);
            position += part.0.len() as u64;
        }
        Ok(std::mem::take(&mut context.0))
    }

    fn confirm(
        &self,
        state: &mut State,
        through: u64,
        text: String,
        fallback: bool,
        words: u32,
    ) -> Result<(), Failure> {
        if through <= state.manifest.confirmed_bytes || through > state.manifest.captured_bytes {
            return Err(Failure::Api(ApiErrorKind::Unexpected));
        }
        let context = self.context_at(state, through)?;
        self.journal
            .confirm_streaming(
                through,
                text,
                context,
                fallback,
                Some(words),
                time::OffsetDateTime::now_utc(),
            )
            .map_err(Failure::Storage)?;
        state.manifest = self.journal.snapshot().map_err(Failure::Storage)?;
        state.cache = None;
        self.publish(state);
        Ok(())
    }
}

async fn send_audio<S: StreamingBackend>(
    inner: Arc<Inner<S>>,
    sender: S::Sender,
    start: u64,
    committed_to: Arc<AtomicU64>,
) -> Result<(), Failure> {
    let mut position = start;
    let mut committed = start;
    let mut quiet = 0u64;
    let began = tokio::time::Instant::now();
    loop {
        let audio = {
            let mut state = inner.lock()?;
            inner.read_range(&mut state, position, 640)?
        };
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
            sender
                .audio(&audio.0)
                .await
                .map_err(|error| Failure::Api(error.kind))?;
            position += audio.0.len() as u64;
            if quiet >= 12_800 && position - committed >= 64_000 {
                {
                    let mut state = inner.lock()?;
                    if !state.manifest.capture_complete {
                        inner.checkpoint(&mut state, false)?;
                    }
                }
                committed_to.store(position, Ordering::Release);
                sender
                    .commit(false)
                    .await
                    .map_err(|error| Failure::Api(error.kind))?;
                committed = position;
                quiet = 0;
            }
        } else {
            let complete = {
                let state = inner.lock()?;
                state.manifest.capture_complete && position == state.captured
            };
            if complete {
                break;
            }
            tokio::select! { _ = inner.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
        }
        if began.elapsed() >= Duration::from_secs(50 * 60) {
            break;
        }
    }
    {
        let mut state = inner.lock()?;
        if !state.manifest.capture_complete {
            inner.checkpoint(&mut state, false)?;
        }
    }
    committed_to.store(position, Ordering::Release);
    sender
        .commit(true)
        .await
        .map_err(|error| Failure::Api(error.kind))
}

async fn receive_text<S: StreamingBackend>(
    inner: Arc<Inner<S>>,
    mut receiver: S::Receiver,
    committed_to: Arc<AtomicU64>,
) -> Result<(), Failure> {
    loop {
        let event = receiver
            .receive()
            .await
            .map_err(|error| Failure::Api(error.kind))?;
        let delta = matches!(&event, DictationMessage::Delta { .. });
        let mut state = inner.lock()?;
        state.service_error = None;
        match event {
            DictationMessage::Confirmed {
                item_id,
                text,
                byte_end,
            } => {
                if byte_end > committed_to.load(Ordering::Acquire) {
                    return Err(Failure::Api(ApiErrorKind::Unexpected));
                }
                let words = text.split_whitespace().count() as u32;
                let full =
                    append_preserving_prefix(&state.manifest.confirmed_text, &text, false, None);
                inner.confirm(&mut state, byte_end, full, false, words)?;
                state
                    .hypotheses
                    .retain(|hypothesis| hypothesis.id != item_id);
            }
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
        let mut state = inner.lock()?;
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
    let mut sending = tokio::spawn(send_audio(inner.clone(), sender, start, committed.clone()));
    let mut receiving = tokio::spawn(receive_text(inner, receiver, committed));
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
    if !sent {
        sending.abort();
        let _ = sending.await;
    }
    if !received {
        receiving.abort();
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

async fn fallback<S: StreamingBackend>(inner: Arc<Inner<S>>) -> Result<(), Failure> {
    let (cursor, context, captured, language, binding) = {
        let state = inner.lock()?;
        (
            state.manifest.confirmed_bytes,
            AudioBuffer(state.manifest.tail.clone()),
            state.captured,
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
        for chunk in &mut batches.0 {
            let audio = AudioBuffer(std::mem::take(&mut chunk.wav));
            let text = inner
                .service
                .transcribe(&audio.0, &language, &binding)
                .await;
            let text = text.map_err(|error| Failure::Api(error.kind))?;
            let mut state = inner.lock()?;
            let full = append_preserving_prefix(
                &state.manifest.confirmed_text,
                &text,
                chunk.overlaps_previous || (first && !context.0.is_empty()),
                Some(state.manifest.last_segment_words as usize),
            );
            inner.confirm(
                &mut state,
                start + chunk.end_byte,
                full,
                true,
                text.split_whitespace().count() as u32,
            )?;
            first = false;
        }
        if position == captured {
            break;
        }
        let audio = {
            let mut state = inner.lock()?;
            inner.read_range(&mut state, position, 64_000)?
        };
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
        let complete = match inner.lock() {
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
            tokio::select! { result = fallback(inner.clone()) => result, _ = cancel.changed() => return }
        } else {
            let done = match inner.lock() {
                Ok(state) => {
                    state.manifest.capture_complete
                        && state.manifest.confirmed_bytes == state.captured
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
                if let Ok(mut state) = inner.lock() {
                    state.hypotheses.clear();
                    state.service_error = kind;
                    state.storage_failed |= kind.is_none();
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

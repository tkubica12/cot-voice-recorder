use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{Notify, watch};
use voiceprompt_client_protocol::recovery::MAX_CAPTURED_BYTES;
use voiceprompt_core::chunker::ChunkerError;

use crate::legacy_session::{DictationProgress, SessionError};
use crate::platform::NativeError;

pub(crate) const TRIGGER_BYTES: usize = 64_000;
pub(crate) const WINDOW_BYTES: usize = 128_000;

pub(crate) struct AudioBuffer(pub Vec<u8>);

impl Drop for AudioBuffer {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

pub(crate) struct SealingAudio {
    pub end: u64,
    pub pcm: AudioBuffer,
}

pub(crate) struct PcmState {
    pub captured: u64,
    pub buffer_start: u64,
    pub buffer: Vec<u8>,
    pub sealing_bytes: usize,
    pub reserved_bytes: usize,
    pub seal_requested: bool,
    pub closed: bool,
}

impl Drop for PcmState {
    fn drop(&mut self) {
        self.buffer.fill(0);
    }
}

pub(crate) struct CaptureWindow {
    pub pcm: Mutex<PcmState>,
    pub room: Condvar,
    pub seal: Notify,
    pub input: Notify,
    pub captured: AtomicU64,
    pub storage_failed: AtomicBool,
    pub finishing: AtomicBool,
    progress: watch::Sender<DictationProgress>,
    cancel: watch::Sender<bool>,
    wake: Arc<Notify>,
    #[cfg(test)]
    trace: Arc<crate::lock_trace::Lock>,
}

pub(crate) struct CaptureSlot {
    window: Arc<CaptureWindow>,
    remaining: usize,
}

impl CaptureSlot {
    pub fn append(&mut self, pcm: &[u8]) -> Result<(), SessionError> {
        if pcm.len() > self.remaining {
            return Err(SessionError::Storage(NativeError::Verification(
                "Capture packet exceeds its reserved native frame.",
            )));
        }
        if pcm.len() % 2 != 0 {
            return Err(SessionError::Audio(ChunkerError::IncompleteSample));
        }
        self.window.append_reserved(pcm, &mut self.remaining)
    }
}

impl Drop for CaptureSlot {
    fn drop(&mut self) {
        let mut pcm = self.window.pcm.lock().unwrap_or_else(|error| {
            self.window.fail();
            error.into_inner()
        });
        pcm.reserved_bytes -= self.remaining;
        self.window.room.notify_all();
    }
}

impl CaptureWindow {
    pub fn new(
        captured: u64,
        complete: bool,
        progress: watch::Sender<DictationProgress>,
        cancel: watch::Sender<bool>,
        wake: Arc<Notify>,
        #[cfg(test)] trace: Arc<crate::lock_trace::Lock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            pcm: Mutex::new(PcmState {
                captured,
                buffer_start: captured,
                buffer: Vec::with_capacity(WINDOW_BYTES),
                sealing_bytes: 0,
                reserved_bytes: 0,
                seal_requested: false,
                closed: complete,
            }),
            room: Condvar::new(),
            seal: Notify::new(),
            input: Notify::new(),
            captured: AtomicU64::new(captured),
            storage_failed: AtomicBool::new(false),
            finishing: AtomicBool::new(false),
            progress,
            cancel,
            wake,
            #[cfg(test)]
            trace,
        })
    }

    pub fn lock(&self) -> Result<MutexGuard<'_, PcmState>, SessionError> {
        self.pcm.lock().map_err(|_| {
            self.fail();
            SessionError::Storage(NativeError::Verification("Capture PCM lock is poisoned."))
        })
    }

    pub fn cancel(&self) {
        self.cancel.send_replace(true);
        self.room.notify_all();
        self.seal.notify_one();
        self.input.notify_one();
        self.wake.notify_one();
    }

    pub fn fail(&self) {
        self.storage_failed.store(true, Ordering::Release);
        self.progress
            .send_modify(|progress| progress.storage_failed = true);
        self.room.notify_all();
        self.seal.notify_one();
        self.input.notify_one();
        self.wake.notify_one();
    }

    pub fn finish_requested(&self) {
        self.finishing.store(true, Ordering::Release);
        self.room.notify_all();
        self.seal.notify_one();
        self.input.notify_one();
    }

    fn accepting(&self, pcm: &PcmState) -> Result<(), SessionError> {
        if self.storage_failed.load(Ordering::Acquire) {
            return Err(SessionError::Storage(NativeError::Verification(
                "Recovery storage failed.",
            )));
        }
        if pcm.closed || self.finishing.load(Ordering::Acquire) || *self.cancel.borrow() {
            return Err(SessionError::Closed);
        }
        Ok(())
    }

    fn wait_room<'a>(
        &self,
        pcm: MutexGuard<'a, PcmState>,
    ) -> Result<MutexGuard<'a, PcmState>, SessionError> {
        self.room
            .wait_timeout(pcm, Duration::from_millis(100))
            .map(|(pcm, _)| pcm)
            .map_err(|_| {
                self.fail();
                SessionError::Storage(NativeError::Verification("Capture PCM lock is poisoned."))
            })
    }

    pub fn reserve(
        self: &Arc<Self>,
        discard: &AtomicBool,
    ) -> Result<Option<CaptureSlot>, SessionError> {
        let mut pcm = self.lock()?;
        loop {
            if discard.load(Ordering::Acquire) {
                return Ok(None);
            }
            self.accepting(&pcm)?;
            let room = WINDOW_BYTES - pcm.buffer.len() - pcm.sealing_bytes - pcm.reserved_bytes;
            if room >= crate::microphone::BUFFER_BYTES {
                pcm.reserved_bytes += crate::microphone::BUFFER_BYTES;
                return Ok(Some(CaptureSlot {
                    window: self.clone(),
                    remaining: crate::microphone::BUFFER_BYTES,
                }));
            }
            pcm = self.wait_room(pcm)?;
        }
    }

    pub fn append(&self, pcm: &[u8]) -> Result<(), SessionError> {
        // Legacy byte fragments may split a sample; engine and native-slot adapters own alignment.
        self.append_reserved(pcm, &mut 0)
    }

    fn append_reserved(&self, mut pcm: &[u8], reserved: &mut usize) -> Result<(), SessionError> {
        #[cfg(test)]
        let mut phase = crate::mutation_trace::span("append");
        #[cfg(test)]
        if let Some(phase) = &mut phase {
            phase.counts(1, 0, pcm.len() as u64);
        }
        loop {
            let (captured, request) = {
                let mut state = self.lock()?;
                self.accepting(&state)?;
                if state
                    .captured
                    .checked_add(pcm.len() as u64)
                    .is_none_or(|end| end > MAX_CAPTURED_BYTES)
                {
                    return Err(SessionError::Storage(NativeError::Verification(
                        "Recovery captured-byte limit exceeded.",
                    )));
                }
                let room =
                    WINDOW_BYTES - state.buffer.len() - state.sealing_bytes - state.reserved_bytes
                        + *reserved;
                if room == 0 && !pcm.is_empty() {
                    drop(self.wait_room(state)?);
                    continue;
                }
                let count = room.min(pcm.len());
                let credit = count.min(*reserved);
                *reserved -= credit;
                state.reserved_bytes -= credit;
                state.buffer.extend_from_slice(&pcm[..count]);
                state.captured += count as u64;
                pcm = &pcm[count..];
                self.captured.store(state.captured, Ordering::Release);
                let request = state.buffer.len() >= TRIGGER_BYTES;
                state.seal_requested |= request;
                (state.captured, request)
            };
            self.progress.send_modify(|progress| {
                progress.captured_bytes = progress.captured_bytes.max(captured);
                #[cfg(test)]
                self.trace.progress(
                    progress.captured_bytes,
                    progress.saved_bytes,
                    progress.transcribed_bytes,
                );
            });
            self.wake.notify_one();
            self.input.notify_one();
            if request {
                self.seal.notify_one();
            }
            if pcm.is_empty() {
                return Ok(());
            }
        }
    }

    // D serializes drains and publication; charged working bytes may survive D releases.
    pub fn detach(
        &self,
        expected_start: u64,
        complete: bool,
    ) -> Result<SealingAudio, SessionError> {
        self.take(expected_start, complete, false)
    }

    pub fn drain_fragment(
        &self,
        expected_start: u64,
        complete: bool,
    ) -> Result<SealingAudio, SessionError> {
        self.take(expected_start, complete, true)
    }

    fn take(
        &self,
        expected_start: u64,
        complete: bool,
        accumulate_unpublished: bool,
    ) -> Result<SealingAudio, SessionError> {
        let mut pcm = self.lock()?;
        if self.storage_failed.load(Ordering::Acquire) {
            return Err(SessionError::Storage(NativeError::Verification(
                "Recovery storage failed.",
            )));
        }
        if (!accumulate_unpublished && pcm.sealing_bytes != 0) || pcm.buffer_start != expected_start
        {
            self.fail();
            return Err(SessionError::Storage(NativeError::Verification(
                "Capture PCM publication boundary is invalid.",
            )));
        }
        pcm.closed |= complete;
        pcm.seal_requested = false;
        let end = pcm.captured;
        let bytes = if pcm.buffer.is_empty() {
            Vec::new()
        } else {
            std::mem::replace(&mut pcm.buffer, Vec::with_capacity(WINDOW_BYTES))
        };
        pcm.sealing_bytes += bytes.len();
        pcm.buffer_start = end;
        self.room.notify_all();
        Ok(SealingAudio {
            end,
            pcm: AudioBuffer(bytes),
        })
    }

    pub fn published(&self, end: u64) -> Result<(), SessionError> {
        let mut pcm = self.lock()?;
        if pcm.buffer_start != end {
            self.fail();
            return Err(SessionError::Storage(NativeError::Verification(
                "Published capture PCM boundary is invalid.",
            )));
        }
        pcm.sealing_bytes = 0;
        pcm.seal_requested = !pcm.closed && pcm.buffer.len() >= TRIGGER_BYTES;
        if pcm.seal_requested {
            self.seal.notify_one();
        }
        self.room.notify_all();
        Ok(())
    }

    pub fn publish_progress(&self, mut value: DictationProgress) {
        self.progress.send_modify(|progress| {
            value.captured_bytes = value
                .captured_bytes
                .max(self.captured.load(Ordering::Acquire))
                .max(progress.captured_bytes);
            value.storage_failed |= self.storage_failed.load(Ordering::Acquire);
            #[cfg(test)]
            self.trace.progress(
                value.captured_bytes,
                value.saved_bytes,
                value.transcribed_bytes,
            );
            *progress = value;
        });
    }
}

use std::error::Error;
use std::fmt;

use crate::wav::{BYTES_PER_SECOND, WavError, to_wav};

pub const MAX_AUDIO_BYTES: usize = BYTES_PER_SECOND * 6;
pub const OVERLAP_BYTES: usize = BYTES_PER_SECOND * 600 / 1000;
const FRAME_BYTES: usize = BYTES_PER_SECOND / 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioChunk {
    pub wav: Vec<u8>,
    pub overlaps_previous: bool,
    pub end_byte: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChunkerState {
    pub audio: Vec<u8>,
    pub frame: Vec<u8>,
    pub silent_frames: usize,
    pub new_bytes: usize,
    pub has_speech: bool,
    pub overlaps: bool,
    pub silence_rms: f64,
    pub processed_bytes: u64,
    pub version: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChunkerError {
    InvalidThreshold,
    InvalidCheckpoint,
    IncompleteSample,
    CursorOverflow,
    Wav(WavError),
}

impl fmt::Display for ChunkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidThreshold => f.write_str("Silence RMS must be finite and nonnegative."),
            Self::InvalidCheckpoint => f.write_str("Invalid dictation audio checkpoint."),
            Self::IncompleteSample => f.write_str("Incomplete PCM sample."),
            Self::CursorOverflow => f.write_str("Dictation audio cursor exceeds supported bounds."),
            Self::Wav(error) => error.fmt(f),
        }
    }
}

impl Error for ChunkerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Wav(error) => Some(error),
            _ => None,
        }
    }
}

pub struct PcmChunker {
    audio: Box<[u8]>,
    frame: [u8; FRAME_BYTES],
    silence_rms: f64,
    frame_length: usize,
    length: usize,
    silent_frames: usize,
    new_bytes: usize,
    has_speech: bool,
    overlaps: bool,
    processed_bytes: u64,
    level: f64,
}

impl PcmChunker {
    pub fn new(silence_rms: f64) -> Result<Self, ChunkerError> {
        if !silence_rms.is_finite() || silence_rms < 0.0 {
            return Err(ChunkerError::InvalidThreshold);
        }
        Ok(Self {
            audio: vec![0; MAX_AUDIO_BYTES].into_boxed_slice(),
            frame: [0; FRAME_BYTES],
            silence_rms,
            frame_length: 0,
            length: 0,
            silent_frames: 0,
            new_bytes: 0,
            has_speech: false,
            overlaps: false,
            processed_bytes: 0,
            level: 0.0,
        })
    }

    pub fn restore(state: ChunkerState) -> Result<Self, ChunkerError> {
        let mut chunker = Self::new(state.silence_rms)?;
        if state.version != 1
            || state.audio.len() >= MAX_AUDIO_BYTES
            || state.audio.len() % FRAME_BYTES != 0
            || state.frame.len() >= FRAME_BYTES
            || state.new_bytes > state.audio.len()
            || state.processed_bytes < state.audio.len() as u64
            || state.processed_bytes % 2 != 0
        {
            return Err(ChunkerError::InvalidCheckpoint);
        }
        chunker.audio[..state.audio.len()].copy_from_slice(&state.audio);
        chunker.frame[..state.frame.len()].copy_from_slice(&state.frame);
        chunker.length = state.audio.len();
        chunker.frame_length = state.frame.len();
        chunker.silent_frames = state.silent_frames;
        chunker.new_bytes = state.new_bytes;
        chunker.has_speech = state.has_speech;
        chunker.overlaps = state.overlaps;
        chunker.processed_bytes = state.processed_bytes;
        Ok(chunker)
    }

    pub fn snapshot(&self) -> ChunkerState {
        ChunkerState {
            audio: self.audio[..self.length].to_vec(),
            frame: self.frame[..self.frame_length].to_vec(),
            silent_frames: self.silent_frames,
            new_bytes: self.new_bytes,
            has_speech: self.has_speech,
            overlaps: self.overlaps,
            silence_rms: self.silence_rms,
            processed_bytes: self.processed_bytes,
            version: 1,
        }
    }

    pub fn level(&self) -> f64 {
        self.level
    }

    pub fn append(&mut self, mut pcm: &[u8]) -> Result<Vec<AudioChunk>, ChunkerError> {
        let mut chunks = Vec::new();
        while !pcm.is_empty() {
            let count = (FRAME_BYTES - self.frame_length).min(pcm.len());
            self.frame[self.frame_length..self.frame_length + count].copy_from_slice(&pcm[..count]);
            self.frame_length += count;
            pcm = &pcm[count..];
            if self.frame_length == FRAME_BYTES {
                self.process_frame(FRAME_BYTES, &mut chunks)?;
                self.frame_length = 0;
            }
        }
        Ok(chunks)
    }

    pub fn flush(&mut self) -> Result<Vec<AudioChunk>, ChunkerError> {
        if self.frame_length % 2 != 0 {
            return Err(ChunkerError::IncompleteSample);
        }
        let mut chunks = Vec::new();
        if self.frame_length > 0 {
            self.process_frame(self.frame_length, &mut chunks)?;
            self.frame_length = 0;
        }
        if self.has_speech && self.new_bytes > 0 {
            chunks.push(self.emit(false)?);
        }
        self.clear();
        Ok(chunks)
    }

    fn process_frame(
        &mut self,
        length: usize,
        chunks: &mut Vec<AudioChunk>,
    ) -> Result<(), ChunkerError> {
        self.processed_bytes = self
            .processed_bytes
            .checked_add(length as u64)
            .ok_or(ChunkerError::CursorOverflow)?;
        let mut energy = 0.0;
        for sample in self.frame[..length].chunks_exact(2) {
            let value = f64::from(i16::from_le_bytes([sample[0], sample[1]]));
            energy += value * value;
        }
        let rms = (energy / (length / 2) as f64).sqrt();
        self.level = (rms / 6000.0).clamp(0.0, 1.0);
        let voiced = rms >= self.silence_rms;
        self.has_speech |= voiced;
        self.silent_frames = if voiced {
            0
        } else {
            self.silent_frames.saturating_add(1)
        };
        self.audio[self.length..self.length + length].copy_from_slice(&self.frame[..length]);
        self.length += length;
        self.new_bytes += length;

        if !self.has_speech && self.overlaps && self.silent_frames >= 20 {
            self.length = 0;
            self.new_bytes = 0;
            self.overlaps = false;
        } else if !self.has_speech && !self.overlaps && self.length >= BYTES_PER_SECOND / 5 {
            // Match the existing 180 ms pre-roll, including its silence-gating limitations.
            self.audio.copy_within(FRAME_BYTES..self.length, 0);
            self.length -= FRAME_BYTES;
            self.new_bytes = self.length;
        } else if self.has_speech && self.length >= BYTES_PER_SECOND * 2 && self.silent_frames >= 20
        {
            chunks.push(self.emit(false)?);
        } else if self.length == MAX_AUDIO_BYTES {
            chunks.push(self.emit(true)?);
        }
        Ok(())
    }

    fn emit(&mut self, keep_overlap: bool) -> Result<AudioChunk, ChunkerError> {
        let result = AudioChunk {
            wav: to_wav(&self.audio[..self.length]).map_err(ChunkerError::Wav)?,
            overlaps_previous: self.overlaps,
            end_byte: self.processed_bytes,
        };
        let overlap = if keep_overlap { OVERLAP_BYTES } else { 0 };
        if overlap > 0 {
            self.audio
                .copy_within(self.length - overlap..self.length, 0);
        }
        self.length = overlap;
        self.new_bytes = 0;
        self.overlaps = keep_overlap;
        self.has_speech = false;
        self.silent_frames = 0;
        Ok(result)
    }

    fn clear(&mut self) {
        self.audio.fill(0);
        self.frame.fill(0);
        self.length = 0;
        self.new_bytes = 0;
        self.frame_length = 0;
        self.silent_frames = 0;
        self.has_speech = false;
        self.overlaps = false;
    }
}

impl Drop for PcmChunker {
    fn drop(&mut self) {
        self.clear();
    }
}

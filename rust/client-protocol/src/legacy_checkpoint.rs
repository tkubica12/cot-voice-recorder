use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use voiceprompt_core::chunker::{ChunkerState, PcmChunker};

use crate::ProtocolError;
use crate::recovery::MAX_TAIL_BYTES;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Checkpoint {
    audio: String,
    frame: String,
    #[serde(default)]
    silent_frames: usize,
    #[serde(default)]
    new_bytes: usize,
    #[serde(default)]
    has_speech: bool,
    #[serde(default)]
    overlaps: bool,
    #[serde(default)]
    silence_rms: f64,
    #[serde(default)]
    processed_bytes: u64,
    #[serde(default = "one")]
    version: u32,
}

fn one() -> u32 {
    1
}

pub fn parse(data: &[u8]) -> Result<ChunkerState, ProtocolError> {
    if data.len() > MAX_TAIL_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    let checkpoint: Checkpoint = serde_json::from_slice(data)
        .map_err(|_| ProtocolError::InvalidJson("legacy audio checkpoint"))?;
    let state = ChunkerState {
        audio: STANDARD
            .decode(checkpoint.audio)
            .map_err(|_| ProtocolError::InvalidField("legacy audio checkpoint"))?,
        frame: STANDARD
            .decode(checkpoint.frame)
            .map_err(|_| ProtocolError::InvalidField("legacy audio checkpoint"))?,
        silent_frames: checkpoint.silent_frames,
        new_bytes: checkpoint.new_bytes,
        has_speech: checkpoint.has_speech,
        overlaps: checkpoint.overlaps,
        silence_rms: checkpoint.silence_rms,
        processed_bytes: checkpoint.processed_bytes,
        version: checkpoint.version,
    };
    PcmChunker::restore(state.clone())
        .map_err(|_| ProtocolError::InvalidField("legacy audio checkpoint"))?;
    Ok(state)
}

pub fn encode(state: &ChunkerState) -> Result<Vec<u8>, ProtocolError> {
    PcmChunker::restore(state.clone())
        .map_err(|_| ProtocolError::InvalidField("legacy audio checkpoint"))?;
    let checkpoint = Checkpoint {
        audio: STANDARD.encode(&state.audio),
        frame: STANDARD.encode(&state.frame),
        silent_frames: state.silent_frames,
        new_bytes: state.new_bytes,
        has_speech: state.has_speech,
        overlaps: state.overlaps,
        silence_rms: state.silence_rms,
        processed_bytes: state.processed_bytes,
        version: state.version,
    };
    let data = serde_json::to_vec(&checkpoint)
        .map_err(|_| ProtocolError::InvalidJson("legacy audio checkpoint"))?;
    if data.len() > MAX_TAIL_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    Ok(data)
}

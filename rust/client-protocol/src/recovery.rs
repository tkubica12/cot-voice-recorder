use std::fmt;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use voiceprompt_core::chunker::OVERLAP_BYTES;
use voiceprompt_core::wav::{BYTES_PER_SECOND, HEADER_BYTES, ParseMode, validate_capture_wav};

use crate::ProtocolError;

pub const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TAIL_BYTES: usize = 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 16 * 1024;
pub const MAX_CHUNKS: usize = 100_000;
pub const MAX_AUDIO_BYTES: usize = HEADER_BYTES + BYTES_PER_SECOND * 6;
pub const MAX_CAPTURED_BYTES: u64 = 48 * 60 * 60 * BYTES_PER_SECOND as u64;
pub const DOTNET_UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
const MAX_TICKS: i64 = 3_155_378_975_999_999_999;
const MAGIC: u32 = 0x5650_524a;

#[derive(Clone)]
pub struct RecoveryChunk {
    pub index: u32,
    pub overlaps_previous: bool,
    pub text: Option<String>,
    pub audio_bytes: u32,
    pub digest: [u8; 32],
    pub end_byte: u64,
}

pub struct AudioCheckpoint<'a> {
    pub wav: &'a [u8],
    pub overlaps_previous: bool,
    pub end_byte: u64,
}

#[derive(Clone)]
pub struct AuditCheckpoint {
    pub text: String,
    pub completed_at_ticks: i64,
    pub polished: bool,
}

#[derive(Clone)]
pub struct RecoveryManifest {
    pub id: String,
    pub created_at_ticks: i64,
    pub updated_at_ticks: i64,
    pub language: String,
    pub captured_bytes: u64,
    pub tail: Vec<u8>,
    pub capture_complete: bool,
    pub chunks: Vec<RecoveryChunk>,
    pub context: String,
    pub streaming: bool,
    pub confirmed_bytes: u64,
    pub confirmed_text: String,
    pub used_streaming: bool,
    pub used_fallback: bool,
    pub last_segment_words: u32,
    pub audit: Option<AuditCheckpoint>,
}

impl fmt::Debug for RecoveryManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryManifest(<redacted>)")
    }
}

impl fmt::Debug for RecoveryChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryChunk(<redacted>)")
    }
}

impl fmt::Debug for AuditCheckpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuditCheckpoint(<redacted>)")
    }
}

fn invalid() -> ProtocolError {
    ProtocolError::InvalidField("recovery manifest")
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn utc_ticks(now: time::OffsetDateTime) -> Result<i64, ProtocolError> {
    let ticks = now.unix_timestamp_nanos().div_euclid(100) + i128::from(DOTNET_UNIX_EPOCH_TICKS);
    let ticks = i64::try_from(ticks).map_err(|_| invalid())?;
    if !(0..=MAX_TICKS).contains(&ticks) {
        return Err(invalid());
    }
    Ok(ticks)
}

fn text_bound(text: &str, maximum: usize) -> Result<(), ProtocolError> {
    if text.len() > maximum {
        return Err(ProtocolError::TooLarge);
    }
    Ok(())
}

impl RecoveryManifest {
    pub fn create(
        language: String,
        context: String,
        streaming: bool,
        now_ticks: i64,
    ) -> Result<Self, ProtocolError> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| ProtocolError::RandomUnavailable)?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let id = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let manifest = Self {
            id,
            created_at_ticks: now_ticks,
            updated_at_ticks: now_ticks,
            language,
            context,
            streaming,
            captured_bytes: 0,
            tail: Vec::new(),
            capture_complete: false,
            chunks: Vec::new(),
            confirmed_bytes: 0,
            confirmed_text: String::new(),
            used_streaming: false,
            used_fallback: false,
            last_segment_words: 0,
            audit: None,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn checkpoint(
        &self,
        tail: &[u8],
        captured_bytes: u64,
        audio: &[AudioCheckpoint<'_>],
        complete: bool,
        now_ticks: i64,
    ) -> Result<Self, ProtocolError> {
        self.validate()?;
        if self.capture_complete
            || captured_bytes < self.captured_bytes
            || captured_bytes > MAX_CAPTURED_BYTES
            || tail.len() > MAX_TAIL_BYTES
            || audio.len() > MAX_CHUNKS - self.chunks.len()
            || !(0..=MAX_TICKS).contains(&now_ticks)
        {
            return Err(invalid());
        }
        let mut next = self.clone();
        next.tail.fill(0);
        next.tail = tail.to_vec();
        next.captured_bytes = captured_bytes;
        next.capture_complete = complete;
        next.updated_at_ticks = next.updated_at_ticks.max(now_ticks);
        for audio in audio {
            let chunk = RecoveryChunk {
                index: next.chunks.len() as u32,
                overlaps_previous: audio.overlaps_previous,
                text: None,
                audio_bytes: u32::try_from(audio.wav.len()).map_err(|_| ProtocolError::TooLarge)?,
                digest: Sha256::digest(audio.wav).into(),
                end_byte: audio.end_byte,
            };
            chunk.verify_audio(audio.wav)?;
            next.chunks.push(chunk);
        }
        next.validate()?;
        Ok(next)
    }

    pub fn commit_legacy_result(
        &mut self,
        index: u32,
        text: String,
        now_ticks: i64,
    ) -> Result<bool, ProtocolError> {
        self.validate()?;
        text_bound(&text, MAX_TEXT_BYTES)?;
        if self.streaming || !(0..=MAX_TICKS).contains(&now_ticks) {
            return Err(invalid());
        }
        let chunk = self.chunks.get_mut(index as usize).ok_or_else(invalid)?;
        match &chunk.text {
            Some(previous) if previous != &text => {
                return Err(ProtocolError::InvalidField("conflicting recovery result"));
            }
            Some(_) => return Ok(false),
            None => chunk.text = Some(text),
        }
        self.updated_at_ticks = self.updated_at_ticks.max(now_ticks);
        Ok(true)
    }

    pub fn parse(data: &[u8], id: &str) -> Result<Self, ProtocolError> {
        if data.len() > MAX_MANIFEST_BYTES {
            return Err(ProtocolError::TooLarge);
        }
        let mut reader = Reader { data, position: 0 };
        if reader.u32()? != MAGIC {
            return Err(invalid());
        }
        let version = reader.u32()?;
        if !(1..=4).contains(&version) || !valid_id(id) || reader.text(32)? != id {
            return Err(invalid());
        }
        let created_at_ticks = reader.i64()?;
        let updated_at_ticks = reader.i64()?;
        let language = reader.text(128)?;
        let captured_bytes = reader.u64()?;
        let tail = reader.bytes(MAX_TAIL_BYTES)?.to_vec();
        let capture_complete = reader.boolean()?;
        let count = reader.u32()? as usize;
        let minimum = if version >= 3 { 54 } else { 46 };
        if count > MAX_CHUNKS || count > reader.remaining() / minimum {
            return Err(invalid());
        }
        let mut chunks = Vec::with_capacity(count);
        for _ in 0..count {
            let index = reader.u32()?;
            let overlaps_previous = reader.boolean()?;
            let text = if reader.boolean()? {
                Some(reader.text(MAX_TEXT_BYTES)?)
            } else {
                None
            };
            let audio_bytes = reader.u32()?;
            let digest = reader.bytes(32)?.try_into().map_err(|_| invalid())?;
            let end_byte = if version >= 3 { reader.u64()? } else { 0 };
            chunks.push(RecoveryChunk {
                index,
                overlaps_previous,
                text,
                audio_bytes,
                digest,
                end_byte,
            });
        }
        let context = if version >= 2 {
            reader.text(MAX_CONTEXT_BYTES)?
        } else {
            String::new()
        };
        let mut manifest = Self {
            id: id.into(),
            created_at_ticks,
            updated_at_ticks,
            language,
            captured_bytes,
            tail,
            capture_complete,
            chunks,
            context,
            streaming: version == 4,
            confirmed_bytes: 0,
            confirmed_text: String::new(),
            used_streaming: false,
            used_fallback: false,
            last_segment_words: 0,
            audit: None,
        };
        if version == 4 {
            manifest.confirmed_bytes = reader.u64()?;
            manifest.confirmed_text = reader.text(MAX_TEXT_BYTES)?;
            manifest.used_streaming = reader.boolean()?;
            manifest.used_fallback = reader.boolean()?;
            manifest.last_segment_words = reader.u32()?;
            if reader.boolean()? {
                manifest.audit = Some(AuditCheckpoint {
                    text: reader.text(MAX_TEXT_BYTES)?,
                    completed_at_ticks: reader.i64()?,
                    polished: reader.boolean()?,
                });
            }
        }
        if reader.remaining() != 0 {
            return Err(invalid());
        }
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if !valid_id(&self.id)
            || !(0..=MAX_TICKS).contains(&self.created_at_ticks)
            || !(self.created_at_ticks..=MAX_TICKS).contains(&self.updated_at_ticks)
            || self.language.trim().is_empty()
            || self.captured_bytes > MAX_CAPTURED_BYTES
            || self.tail.len() > MAX_TAIL_BYTES
            || self.chunks.len() > MAX_CHUNKS
        {
            return Err(invalid());
        }
        text_bound(&self.language, 128)?;
        text_bound(&self.context, MAX_CONTEXT_BYTES)?;
        text_bound(&self.confirmed_text, MAX_TEXT_BYTES)?;
        let mut previous_end = 0;
        for (index, chunk) in self.chunks.iter().enumerate() {
            let bytes = chunk.audio_bytes as usize;
            if chunk.index as usize != index
                || !(46..=MAX_AUDIO_BYTES).contains(&bytes)
                || (bytes - HEADER_BYTES) % 2 != 0
                || chunk.end_byte > self.captured_bytes
                || (chunk.end_byte != 0 && chunk.end_byte < previous_end)
            {
                return Err(invalid());
            }
            if let Some(text) = &chunk.text {
                text_bound(text, MAX_TEXT_BYTES)?;
            }
            if self.streaming
                && (chunk.overlaps_previous
                    || chunk.end_byte != previous_end + (bytes - HEADER_BYTES) as u64
                    || chunk.text.is_some() && chunk.end_byte > self.confirmed_bytes)
            {
                return Err(invalid());
            }
            if chunk.end_byte != 0 {
                previous_end = chunk.end_byte;
            }
        }
        if self.streaming {
            if self.confirmed_bytes > self.captured_bytes
                || self.confirmed_bytes % 2 != 0
                || self.captured_bytes % 2 != 0
                || self.tail.len() as u64 > self.confirmed_bytes.min(OVERLAP_BYTES as u64)
                || self.tail.len() % 2 != 0
                || self.last_segment_words as usize > MAX_TEXT_BYTES
            {
                return Err(invalid());
            }
        } else if self.confirmed_bytes != 0
            || !self.confirmed_text.is_empty()
            || self.used_streaming
            || self.used_fallback
            || self.last_segment_words != 0
            || self.audit.is_some()
        {
            return Err(invalid());
        }
        if let Some(audit) = &self.audit {
            if !self.streaming
                || !self.capture_complete
                || self.confirmed_bytes != self.captured_bytes
                || audit.text.trim().is_empty()
                || !(0..=MAX_TICKS).contains(&audit.completed_at_ticks)
            {
                return Err(invalid());
            }
            text_bound(&audit.text, MAX_TEXT_BYTES)?;
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        let mut writer = Writer(Vec::new());
        writer.u32(MAGIC)?;
        writer.u32(if self.streaming { 4 } else { 3 })?;
        writer.bytes(self.id.as_bytes())?;
        writer.i64(self.created_at_ticks)?;
        writer.i64(self.updated_at_ticks)?;
        writer.bytes(self.language.as_bytes())?;
        writer.u64(self.captured_bytes)?;
        writer.bytes(&self.tail)?;
        writer.boolean(self.capture_complete)?;
        writer.u32(self.chunks.len() as u32)?;
        for chunk in &self.chunks {
            writer.u32(chunk.index)?;
            writer.boolean(chunk.overlaps_previous)?;
            writer.boolean(chunk.text.is_some())?;
            if let Some(text) = &chunk.text {
                writer.bytes(text.as_bytes())?;
            }
            writer.u32(chunk.audio_bytes)?;
            writer.bytes(&chunk.digest)?;
            writer.u64(chunk.end_byte)?;
        }
        writer.bytes(self.context.as_bytes())?;
        if self.streaming {
            writer.u64(self.confirmed_bytes)?;
            writer.bytes(self.confirmed_text.as_bytes())?;
            writer.boolean(self.used_streaming)?;
            writer.boolean(self.used_fallback)?;
            writer.u32(self.last_segment_words)?;
            writer.boolean(self.audit.is_some())?;
            if let Some(audit) = &self.audit {
                writer.bytes(audit.text.as_bytes())?;
                writer.i64(audit.completed_at_ticks)?;
                writer.boolean(audit.polished)?;
            }
        }
        Ok(writer.0)
    }

    pub fn confirm(
        &mut self,
        through: u64,
        text: String,
        context_audio: Vec<u8>,
        fallback: bool,
        segment_words: Option<u32>,
        now_ticks: i64,
    ) -> Result<Vec<u32>, ProtocolError> {
        self.validate()?;
        if !self.streaming
            || through < self.confirmed_bytes
            || through > self.captured_bytes
            || through % 2 != 0
            || !text.starts_with(&self.confirmed_text)
            || through == self.confirmed_bytes && text != self.confirmed_text
            || context_audio.len() as u64 > through.min(OVERLAP_BYTES as u64)
            || context_audio.len() % 2 != 0
            || !(0..=MAX_TICKS).contains(&now_ticks)
        {
            return Err(invalid());
        }
        text_bound(&text, MAX_TEXT_BYTES)?;
        let words = segment_words
            .unwrap_or_else(|| text[self.confirmed_text.len()..].split_whitespace().count() as u32);
        if words as usize > MAX_TEXT_BYTES {
            return Err(invalid());
        }
        let acknowledged: Vec<_> = self
            .chunks
            .iter()
            .filter(|chunk| chunk.text.is_none() && chunk.end_byte <= through)
            .map(|chunk| chunk.index)
            .collect();
        for index in &acknowledged {
            self.chunks[*index as usize].text = Some(String::new());
        }
        self.confirmed_bytes = through;
        self.confirmed_text = text;
        self.tail.fill(0);
        self.tail = context_audio;
        self.used_streaming |= !fallback;
        self.used_fallback |= fallback;
        self.last_segment_words = words;
        self.updated_at_ticks = self.updated_at_ticks.max(now_ticks);
        Ok(acknowledged)
    }

    pub fn freeze_audit(
        &mut self,
        text: String,
        completed_at_ticks: i64,
        polished: bool,
        now_ticks: i64,
    ) -> Result<(), ProtocolError> {
        self.validate()?;
        if !self.streaming
            || !self.capture_complete
            || self.confirmed_bytes != self.captured_bytes
            || text.trim().is_empty()
            || !(0..=MAX_TICKS).contains(&completed_at_ticks)
            || !(0..=MAX_TICKS).contains(&now_ticks)
        {
            return Err(invalid());
        }
        text_bound(&text, MAX_TEXT_BYTES)?;
        if self.audit.as_ref().is_some_and(|audit| {
            audit.text != text
                || audit.completed_at_ticks != completed_at_ticks
                || audit.polished != polished
        }) {
            return Err(ProtocolError::InvalidField("conflicting recovery audit"));
        }
        self.audit = Some(AuditCheckpoint {
            text,
            completed_at_ticks,
            polished,
        });
        self.updated_at_ticks = self.updated_at_ticks.max(now_ticks);
        self.tail.fill(0);
        self.tail.clear();
        Ok(())
    }
}

impl Drop for RecoveryManifest {
    fn drop(&mut self) {
        self.tail.fill(0);
    }
}

impl RecoveryChunk {
    pub fn verify_audio(&self, wav: &[u8]) -> Result<(), ProtocolError> {
        if wav.len() != self.audio_bytes as usize
            || wav.len() > MAX_AUDIO_BYTES
            || wav.len() < 46
            || wav.get(12..20) != Some(&[b'f', b'm', b't', b' ', 16, 0, 0, 0])
            || wav.get(36..40) != Some(b"data")
        {
            return Err(invalid());
        }
        let properties = validate_capture_wav(wav, ParseMode::Strict).map_err(|_| invalid())?;
        if properties.data_bytes as usize != wav.len() - HEADER_BYTES
            || !bool::from(self.digest.ct_eq(&<[u8; 32]>::from(Sha256::digest(wav))))
        {
            return Err(ProtocolError::InvalidField("recovery audio digest"));
        }
        Ok(())
    }
}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.data.len() - self.position
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], ProtocolError> {
        if count > self.remaining() {
            return Err(invalid());
        }
        let start = self.position;
        self.position += count;
        Ok(&self.data[start..self.position])
    }
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| invalid())?,
        ))
    }
    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| invalid())?,
        ))
    }
    fn i64(&mut self) -> Result<i64, ProtocolError> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| invalid())?,
        ))
    }
    fn boolean(&mut self) -> Result<bool, ProtocolError> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid()),
        }
    }
    fn bytes(&mut self, maximum: usize) -> Result<&'a [u8], ProtocolError> {
        let count = self.u32()? as usize;
        if count > maximum {
            return Err(invalid());
        }
        self.take(count)
    }
    fn text(&mut self, maximum: usize) -> Result<String, ProtocolError> {
        std::str::from_utf8(self.bytes(maximum)?)
            .map(str::to_owned)
            .map_err(|_| invalid())
    }
}

struct Writer(Vec<u8>);

impl Writer {
    fn raw(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        if bytes.len() > MAX_MANIFEST_BYTES - self.0.len() {
            return Err(ProtocolError::TooLarge);
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    fn u32(&mut self, value: u32) -> Result<(), ProtocolError> {
        self.raw(&value.to_le_bytes())
    }
    fn u64(&mut self, value: u64) -> Result<(), ProtocolError> {
        self.raw(&value.to_le_bytes())
    }
    fn i64(&mut self, value: i64) -> Result<(), ProtocolError> {
        self.raw(&value.to_le_bytes())
    }
    fn boolean(&mut self, value: bool) -> Result<(), ProtocolError> {
        self.raw(&[u8::from(value)])
    }
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        self.u32(u32::try_from(bytes.len()).map_err(|_| ProtocolError::TooLarge)?)?;
        self.raw(bytes)
    }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;

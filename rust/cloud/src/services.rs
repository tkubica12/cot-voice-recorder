use base64::Engine;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use time::OffsetDateTime;

use crate::config::Config;
use crate::domain::{self, ChunkState, RecordingState};
use crate::models;
use crate::ports::{Blobs, Queue};
use crate::problem::Problem;
use crate::repository::Repository;
use crate::storage::{Entity, StorageError};

#[cfg(test)]
#[path = "services_tests.rs"]
pub(crate) mod tests;

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub struct Services {
    pub config: Arc<Config>,
    pub repository: Repository,
    pub blobs: Blobs,
    pub queue: Queue,
}
pub fn now() -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    OffsetDateTime::from_unix_timestamp_nanos(now.unix_timestamp_nanos().div_euclid(1000) * 1000)
        .expect("current time is in supported range")
}
pub fn uuid() -> Result<String, Problem> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| Problem::internal())?;
    bytes[6] = (bytes[6] & 0xf) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
pub(crate) fn storage(error: StorageError) -> Problem {
    match error {
        StorageError::NotFound => Problem::not_found("Stored entity or body was not found."),
        StorageError::Conflict => {
            Problem::conflict("A concurrent update changed the stored entity.")
        }
        StorageError::MessageGone => Problem::unavailable("Queue receipt was lost."),
        StorageError::Failure(problem) => problem,
    }
}
fn codec(_: serde_json::Error) -> Problem {
    Problem::internal()
}

pub struct ChunkUpload {
    pub index: u32,
    pub wav: Vec<u8>,
    pub checksum: String,
    pub duration_ms: Option<u32>,
    pub overlap_ms: Option<u32>,
    pub started_at: Option<OffsetDateTime>,
    pub start_sample: Option<u64>,
    pub sample_count: Option<u64>,
}

impl Services {
    pub async fn archive(
        &self,
        id: &str,
        data: models::ArchiveDictation,
    ) -> Result<domain::Transcript, Problem> {
        let id = canonical_uuid(id)?;
        let current = now();
        let completed = OffsetDateTime::from_unix_timestamp_nanos(
            data.completed_at.unix_timestamp_nanos().div_euclid(1000) * 1000,
        )
        .map_err(|_| Problem::validation("Invalid audit completion."))?;
        let expires = completed + time::Duration::hours(48);
        if completed > current + time::Duration::minutes(5)
            || expires <= current
            || data.text.chars().all(crate::edits::python_whitespace)
            || data.raw_text.chars().all(crate::edits::python_whitespace)
            || data.text.len() > 1024 * 1024
            || data.raw_text.len() > 1024 * 1024
            || !matches!(data.language.as_str(), "auto" | "cs" | "en")
            || !matches!(
                data.transcribe_model.as_str(),
                "MAI-Transcribe-2-Streaming"
                    | "MAI-Transcribe-2"
                    | "MAI-Transcribe-2-Streaming+MAI-Transcribe-2"
            )
        {
            return Err(Problem::validation("Invalid or expired dictation audit."));
        }
        let mut envelope = serde_json::to_value(&data).map_err(codec)?;
        envelope
            .as_object_mut()
            .ok_or_else(Problem::internal)?
            .insert(
                "completed_at".into(),
                serde_json::json!(python_datetime(
                    completed.to_offset(data.completed_at.offset()),
                    true
                )),
            );
        let bytes = serde_json::to_vec(&envelope).map_err(codec)?;
        let digest = sha256_hex(&bytes);
        if let Some(entity) = self
            .repository
            .get(&self.config.transcripts_table, "transcript", &id)
            .await
            .map_err(storage)?
        {
            let previous = domain::Transcript::from_entity(&entity).map_err(codec)?;
            if previous.source != "windows_dictation"
                || previous.content_digest.as_deref() != Some(digest.as_str())
            {
                return Err(Problem::conflict(
                    "A different dictation is already archived for this session.",
                ));
            }
            return Ok(previous);
        }
        let path = format!("windows-dictation/{id}/{digest}.json");
        self.blobs
            .put(
                &self.config.transcript_container,
                &path,
                bytes,
                "application/json",
            )
            .await
            .map_err(storage)?;
        let transcript = domain::Transcript {
            transcript_id: id.clone(),
            recording_id: id.clone(),
            preview: preview(&data.text, 140),
            language: data.language,
            refine_model: if data.polished {
                self.config.refinement.clone()
            } else {
                "none".into()
            },
            completed_at: completed,
            expires_at: expires,
            character_count: data.text.chars().count(),
            body_path: path,
            source: "windows_dictation".into(),
            transcribe_model: Some(data.transcribe_model),
            content_digest: Some(digest.clone()),
            etag: String::new(),
        };
        let saved = match self
            .repository
            .insert(
                &self.config.transcripts_table,
                transcript.entity().map_err(codec)?,
            )
            .await
        {
            Ok(entity) => domain::Transcript::from_entity(&entity).map_err(codec)?,
            Err(StorageError::Conflict) => {
                let entity = self
                    .repository
                    .get(&self.config.transcripts_table, "transcript", &id)
                    .await
                    .map_err(storage)?
                    .ok_or_else(Problem::internal)?;
                domain::Transcript::from_entity(&entity).map_err(codec)?
            }
            Err(error) => return Err(storage(error)),
        };
        if saved.content_digest.as_deref() != Some(digest.as_str()) {
            return Err(Problem::conflict("A different dictation won publication."));
        }
        Ok(saved)
    }

    pub fn new(
        config: Arc<Config>,
        repository: Repository,
        blobs: Blobs,
        queue: Queue,
    ) -> Result<Self, Problem> {
        if config.environment == "production"
            && (matches!(repository, Repository::Memory(_))
                || matches!(blobs, Blobs::Memory(_))
                || matches!(queue, Queue::Memory(_)))
        {
            return Err(Problem::validation(
                "Production services cannot use memory storage/queue ports.",
            ));
        }
        Ok(Self {
            config,
            repository,
            blobs,
            queue,
        })
    }
    pub async fn recording(&self, id: &str) -> Result<domain::Recording, Problem> {
        let entity = self
            .repository
            .get(&self.config.recordings_table, "recording", id)
            .await
            .map_err(storage)?
            .ok_or_else(|| Problem::not_found("Recording not found."))?;
        domain::Recording::from_entity(&entity).map_err(codec)
    }
    pub async fn save_recording(
        &self,
        record: &domain::Recording,
    ) -> Result<domain::Recording, StorageError> {
        let entity = record
            .entity()
            .map_err(|_| StorageError::Failure(Problem::internal()))?;
        let entity = self
            .repository
            .replace(&self.config.recordings_table, entity)
            .await?;
        domain::Recording::from_entity(&entity)
            .map_err(|_| StorageError::Failure(Problem::internal()))
    }
    pub async fn chunks(&self, id: &str) -> Result<Vec<domain::Chunk>, Problem> {
        let entities = self
            .repository
            .partition(&self.config.chunks_table, id)
            .await
            .map_err(storage)?;
        let mut chunks: Vec<_> = entities
            .iter()
            .map(domain::Chunk::from_entity)
            .collect::<Result<_, _>>()
            .map_err(codec)?;
        chunks.sort_by_key(|chunk| chunk.index);
        Ok(chunks)
    }
    pub async fn create(
        &self,
        request: models::CreateRecording,
    ) -> Result<(domain::Recording, bool), Problem> {
        if request.client_recording_id.is_empty() || request.client_recording_id.len() > 1024 {
            return Err(Problem::validation("Invalid client idempotency key."));
        }
        let mut id = uuid()?;
        let mut index_values = serde_json::Map::new();
        index_values.insert("PartitionKey".into(), serde_json::json!("crid"));
        index_values.insert(
            "RowKey".into(),
            serde_json::json!(request.client_recording_id),
        );
        index_values.insert("recording_id".into(), serde_json::json!(id));
        match self
            .repository
            .insert(
                &self.config.recordings_table,
                Entity {
                    values: index_values,
                    etag: String::new(),
                },
            )
            .await
        {
            Ok(_) => {}
            Err(StorageError::Conflict) => {
                let index = self
                    .repository
                    .get(
                        &self.config.recordings_table,
                        "crid",
                        &request.client_recording_id,
                    )
                    .await
                    .map_err(storage)?
                    .ok_or_else(Problem::internal)?;
                id = index
                    .values
                    .get("recording_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(Problem::internal)?
                    .into();
                match self.recording(&id).await {
                    Ok(recording) => return Ok((recording, false)),
                    Err(problem) if problem.status == 404 => {}
                    Err(problem) => return Err(problem),
                }
            }
            Err(error) => return Err(storage(error)),
        }
        let now = now();
        let record = domain::Recording {
            recording_id: id.clone(),
            client_recording_id: request.client_recording_id,
            state: RecordingState::Recording,
            refine_model: request.refine_model.name().into(),
            language: request.language,
            created_at: now,
            updated_at: now,
            expected_chunk_count: None,
            transcript_id: None,
            failure_reason: None,
            complete_requested_at: None,
            finalize_enqueued: false,
            transcription_mode: if request.transcription_mode
                == models::TranscriptionMode::Streaming
            {
                "streaming"
            } else {
                "chunked"
            }
            .into(),
            audio_layout: if request.audio_layout == models::AudioLayout::Contiguous {
                "contiguous"
            } else {
                "legacy_overlap"
            }
            .into(),
            refinement_enabled: request.refinement_enabled,
            stream_lease_id: None,
            stream_lease_expires_at: None,
            stream_raw_path: None,
            stream_asr_ready: false,
            stream_attempt: 0,
            streamed_samples: 0,
            stream_preview: String::new(),
            stream_error: None,
            stream_delivery_completed: false,
            etag: String::new(),
        };
        match self
            .repository
            .insert(
                &self.config.recordings_table,
                record.entity().map_err(codec)?,
            )
            .await
        {
            Ok(entity) => Ok((
                domain::Recording::from_entity(&entity).map_err(codec)?,
                true,
            )),
            Err(StorageError::Conflict) => Ok((self.recording(&id).await?, false)),
            Err(error) => Err(storage(error)),
        }
    }
    pub async fn wire_recording(
        &self,
        record: &domain::Recording,
    ) -> Result<models::Recording, Problem> {
        let chunks = self.chunks(&record.recording_id).await?;
        Ok(models::Recording {
            recording_id: record.recording_id.clone(),
            client_recording_id: record.client_recording_id.clone(),
            state: record.state,
            refine_model: record.refine_model.clone(),
            language: record.language.clone(),
            progress: models::RecordingProgress {
                expected_chunk_count: record.expected_chunk_count,
                received_chunk_count: chunks.len(),
                transcribed_chunk_count: if record.stream_asr_ready {
                    chunks.len()
                } else {
                    chunks
                        .iter()
                        .filter(|chunk| chunk.state == ChunkState::Transcribed)
                        .count()
                },
                streamed_audio_ms: record.streamed_samples * 1000 / 16000,
                stream_preview: record.stream_preview.clone(),
                stream_attempt: record.stream_attempt,
                stream_error: record.stream_error.clone(),
            },
            transcript_id: record.transcript_id.clone(),
            failure_reason: record.failure_reason.clone(),
            created_at: record.created_at,
            updated_at: record.updated_at,
            transcription_mode: record.transcription_mode.clone(),
            audio_layout: record.audio_layout.clone(),
            refinement_enabled: record.refinement_enabled,
        })
    }
    pub async fn advance(&self, id: &str) -> Result<(), Problem> {
        for _ in 0..5 {
            let mut record = self.recording(id).await?;
            if matches!(
                record.state,
                RecordingState::Refining | RecordingState::Completed | RecordingState::Failed
            ) {
                return Ok(());
            }
            let target = active_state(&record, &self.chunks(id).await?);
            if record.state == target || !record.state.can_transition(target) {
                return Ok(());
            }
            record.state = target;
            record.updated_at = now();
            match self.save_recording(&record).await {
                Ok(_) => return Ok(()),
                Err(StorageError::Conflict) => continue,
                Err(error) => return Err(storage(error)),
            }
        }
        Err(Problem::conflict(
            "Recording remained busy during progress update.",
        ))
    }
    pub async fn upload(
        &self,
        id: &str,
        upload: ChunkUpload,
    ) -> Result<(models::ChunkAccepted, u16), Problem> {
        let ChunkUpload {
            index,
            wav,
            checksum,
            duration_ms,
            overlap_ms,
            started_at,
            start_sample,
            sample_count,
        } = upload;
        let record = self.recording(id).await?;
        let mut start_sample = start_sample;
        let mut sample_count = sample_count;
        if record.transcription_mode == "streaming" {
            let (start, count) = crate::audio::streaming_samples(
                &wav,
                index,
                record.audio_layout == "contiguous",
                start_sample,
                sample_count,
                overlap_ms,
            )?;
            start_sample = Some(start);
            sample_count = Some(count);
        }
        let existing = self
            .repository
            .get(&self.config.chunks_table, id, &format!("{index:08}"))
            .await
            .map_err(storage)?;
        if let Some(entity) = existing {
            let chunk = domain::Chunk::from_entity(&entity).map_err(codec)?;
            if chunk.checksum != checksum {
                return Err(chunk_conflict());
            }
            if record.transcription_mode == "streaming"
                && !record.state.terminal()
                && !record.stream_asr_ready
            {
                self.queue
                    .send(serde_json::json!({"type":"stream","recording_id":id}), 0)
                    .await
                    .map_err(storage)?;
            }
            return Ok((accepted(&chunk), 200));
        }
        if record.transcription_mode == "streaming"
            && (record.state.terminal() || record.stream_asr_ready)
            || record
                .expected_chunk_count
                .is_some_and(|count| index >= count)
        {
            return Err(chunk_conflict());
        }
        let path = if record.transcription_mode == "streaming" {
            format!("{id}/{index}-{}.wav", sha256_hex(&wav))
        } else {
            format!("{id}/{index}.wav")
        };
        let bytes = wav.len() as u64;
        self.blobs
            .put(&self.config.audio_container, &path, wav, "audio/wav")
            .await
            .map_err(storage)?;
        let chunk = domain::Chunk {
            recording_id: id.into(),
            index,
            checksum,
            state: ChunkState::Accepted,
            received_at: now(),
            size_bytes: bytes,
            blob_path: Some(path),
            text: None,
            duration_ms,
            overlap_ms,
            started_at,
            start_sample,
            sample_count,
            etag: String::new(),
        };
        let (chunk, created) = match self
            .repository
            .insert(&self.config.chunks_table, chunk.entity().map_err(codec)?)
            .await
        {
            Ok(entity) => (domain::Chunk::from_entity(&entity).map_err(codec)?, true),
            Err(StorageError::Conflict) => {
                let entity = self
                    .repository
                    .get(&self.config.chunks_table, id, &format!("{index:08}"))
                    .await
                    .map_err(storage)?
                    .ok_or_else(Problem::internal)?;
                let old = domain::Chunk::from_entity(&entity).map_err(codec)?;
                if old.checksum != chunk.checksum {
                    return Err(chunk_conflict());
                }
                (old, false)
            }
            Err(error) => return Err(storage(error)),
        };
        if created || record.transcription_mode == "streaming" {
            let message = if record.transcription_mode == "streaming" {
                serde_json::json!({"type":"stream","recording_id":id})
            } else {
                serde_json::json!({"type":"transcribe","recording_id":id,"index":index})
            };
            self.queue.send(message, 0).await.map_err(storage)?;
        }
        self.advance(id).await?;
        Ok((accepted(&chunk), if created { 202 } else { 200 }))
    }
    pub async fn complete(
        &self,
        id: &str,
        count: u32,
    ) -> Result<(domain::Recording, u16), Problem> {
        if count == 0 {
            return Err(Problem::validation("chunk_count must be at least one."));
        }
        for _ in 0..5 {
            let mut record = self.recording(id).await?;
            if let Some(previous) = record.expected_chunk_count {
                if previous != count {
                    return Err(complete_conflict());
                }
                self.arm_finalize(id).await?;
                return Ok((self.recording(id).await?, 200));
            }
            let chunks = self.chunks(id).await?;
            if chunks.iter().any(|chunk| chunk.index >= count) {
                return Err(complete_conflict());
            }
            record.expected_chunk_count = Some(count);
            let target = active_state(&record, &chunks);
            if record.state.can_transition(target) {
                record.state = target;
            }
            record.complete_requested_at = Some(now());
            record.updated_at = now();
            match self.save_recording(&record).await {
                Ok(_) => {
                    self.arm_finalize(id).await?;
                    return Ok((self.recording(id).await?, 202));
                }
                Err(StorageError::Conflict) => continue,
                Err(error) => return Err(storage(error)),
            }
        }
        Err(complete_conflict())
    }
    pub async fn arm_finalize(&self, id: &str) -> Result<(), Problem> {
        let record = self.recording(id).await?;
        if record.expected_chunk_count.is_none() || record.state.terminal() {
            return Ok(());
        }
        if record.transcription_mode == "streaming" && !record.stream_asr_ready {
            return self
                .queue
                .send(serde_json::json!({"type":"stream","recording_id":id}), 0)
                .await
                .map_err(storage);
        }
        for _ in 0..5 {
            let mut current = self.recording(id).await?;
            if current.finalize_enqueued
                || current.state == RecordingState::Refining
                || current.state.terminal()
            {
                break;
            }
            let chunks = self.chunks(id).await?;
            if !all_expected(&current, &chunks, true) {
                break;
            }
            current.finalize_enqueued = true;
            current.state = RecordingState::Transcribing;
            current.updated_at = now();
            match self.save_recording(&current).await {
                Ok(_) => {}
                Err(StorageError::Conflict) => continue,
                Err(error) => return Err(storage(error)),
            }
            if let Err(error) = self
                .queue
                .send(serde_json::json!({"type":"finalize","recording_id":id}), 0)
                .await
            {
                for _ in 0..5 {
                    let mut rollback = self.recording(id).await?;
                    if !rollback.finalize_enqueued
                        || rollback.state == RecordingState::Refining
                        || rollback.state.terminal()
                    {
                        break;
                    }
                    rollback.finalize_enqueued = false;
                    rollback.updated_at = now();
                    match self.save_recording(&rollback).await {
                        Ok(_) => break,
                        Err(StorageError::Conflict) => continue,
                        Err(error) => return Err(storage(error)),
                    }
                }
                return Err(storage(error));
            }
            return Ok(());
        }
        self.queue
            .send(
                serde_json::json!({"type":"finalize","recording_id":id}),
                900,
            )
            .await
            .map_err(storage)
    }
    pub async fn transcript(&self, id: &str) -> Result<models::Transcript, Problem> {
        let entity = self
            .repository
            .get(&self.config.transcripts_table, "transcript", id)
            .await
            .map_err(storage)?
            .ok_or_else(|| Problem::not_found("Transcript not found or expired."))?;
        let transcript = domain::Transcript::from_entity(&entity).map_err(codec)?;
        if transcript.expires_at <= now() {
            return Err(Problem::not_found("Transcript not found or expired."));
        }
        let bytes = self
            .blobs
            .get(
                &self.config.transcript_container,
                &transcript.body_path,
                16 * 1024 * 1024,
            )
            .await
            .map_err(storage)?;
        let mut body = String::from_utf8(bytes).map_err(|_| Problem::internal())?;
        let raw_body = if transcript.source == "windows_dictation" {
            let archive: models::ArchiveDictation = serde_json::from_str(&body).map_err(codec)?;
            body = archive.text;
            Some(archive.raw_text)
        } else {
            None
        };
        Ok(models::Transcript {
            transcript_id: transcript.transcript_id,
            recording_id: transcript.recording_id,
            body,
            preview: transcript.preview,
            language: transcript.language,
            refine_model: transcript.refine_model,
            completed_at: transcript.completed_at,
            expires_at: transcript.expires_at,
            character_count: transcript.character_count,
            source: transcript.source,
            transcribe_model: transcript.transcribe_model,
            raw_body,
        })
    }
    pub async fn list_transcripts(
        &self,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<models::TranscriptPage, Problem> {
        if !(1..=100).contains(&limit) {
            return Err(Problem::new(
                400,
                "bad-request",
                "Bad request",
                "Invalid pagination limit.",
            ));
        }
        let cursor = cursor
            .map(|cursor| {
                let mut padded = cursor.to_owned();
                while padded.len() % 4 != 0 {
                    padded.push('=');
                }
                base64::engine::general_purpose::URL_SAFE
                    .decode(padded)
                    .map_err(|_| ())
                    .and_then(|bytes| String::from_utf8(bytes).map_err(|_| ()))
            })
            .transpose()
            .map_err(|_| {
                Problem::new(
                    400,
                    "bad-request",
                    "Bad request",
                    "Invalid pagination cursor.",
                )
            })?;
        let entities = self
            .repository
            .partition(&self.config.transcripts_table, "transcript")
            .await
            .map_err(storage)?;
        let mut all: Vec<_> = entities
            .iter()
            .map(domain::Transcript::from_entity)
            .collect::<Result<_, _>>()
            .map_err(codec)?;
        all.sort_by(|left, right| {
            (right.completed_at, &right.transcript_id)
                .cmp(&(left.completed_at, &left.transcript_id))
        });
        let start = cursor
            .and_then(|cursor| all.iter().position(|value| raw_cursor(value) == cursor))
            .map_or(0, |index| index + 1);
        let page: Vec<_> = all.iter().skip(start).take(limit).collect();
        let next_cursor = if start + limit < all.len() {
            page.last().map(|value| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw_cursor(value))
            })
        } else {
            None
        };
        Ok(models::TranscriptPage {
            items: page
                .into_iter()
                .filter(|value| value.expires_at > now())
                .map(models::summary)
                .collect(),
            next_cursor,
        })
    }
}

fn accepted(chunk: &domain::Chunk) -> models::ChunkAccepted {
    models::ChunkAccepted {
        recording_id: chunk.recording_id.clone(),
        index: chunk.index,
        chunk_state: chunk.state,
        checksum: chunk.checksum.clone(),
        received_at: chunk.received_at,
    }
}
fn chunk_conflict() -> Problem {
    Problem::new(
        409,
        "chunk-conflict",
        "Chunk conflict",
        "Chunk payload/index conflicts with stored data.",
    )
}
fn complete_conflict() -> Problem {
    Problem::new(
        409,
        "complete-conflict",
        "Complete conflict",
        "Declared chunk count conflicts with existing data.",
    )
}
fn all_expected(record: &domain::Recording, chunks: &[domain::Chunk], transcribed: bool) -> bool {
    let Some(expected) = record.expected_chunk_count else {
        return false;
    };
    if chunks.len() < expected as usize
        || transcribed && record.transcription_mode == "streaming" && !record.stream_asr_ready
    {
        return false;
    }
    chunks
        .iter()
        .take(expected as usize)
        .enumerate()
        .all(|(index, chunk)| {
            chunk.index as usize == index
                && (!transcribed
                    || record.transcription_mode == "streaming"
                    || chunk.state == ChunkState::Transcribed)
        })
}
fn active_state(record: &domain::Recording, chunks: &[domain::Chunk]) -> RecordingState {
    if record.expected_chunk_count.is_none() {
        RecordingState::Recording
    } else if all_expected(record, chunks, false) {
        RecordingState::Transcribing
    } else {
        RecordingState::Uploading
    }
}
fn raw_cursor(value: &domain::Transcript) -> String {
    format!(
        "{}|{}",
        python_datetime(value.completed_at, false),
        value.transcript_id
    )
}
pub fn canonical_uuid(id: &str) -> Result<String, Problem> {
    let id = id
        .trim()
        .trim_start_matches("urn:uuid:")
        .trim_start_matches('{')
        .trim_end_matches('}');
    let hex = id.replace('-', "").to_ascii_lowercase();
    if hex.len() != 32 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Problem::validation("Invalid UUID."));
    }
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

pub fn preview(text: &str, maximum: usize) -> String {
    let normalized = text
        .split(crate::edits::python_whitespace)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.chars().count() <= maximum {
        normalized
    } else {
        let prefix: String = normalized.chars().take(maximum.saturating_sub(1)).collect();
        format!(
            "{}\u{2026}",
            prefix.trim_end_matches(crate::edits::python_whitespace)
        )
    }
}

pub fn python_datetime(date: OffsetDateTime, utc_as_z: bool) -> String {
    let mut value = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        date.year(),
        date.month() as u8,
        date.day(),
        date.hour(),
        date.minute(),
        date.second()
    );
    if date.microsecond() != 0 {
        value.push_str(&format!(".{:06}", date.microsecond()));
    }

    let offset = date.offset().whole_seconds();
    if offset == 0 && utc_as_z {
        value.push('Z');
    } else {
        value.push_str(&format!(
            "{}{:02}:{:02}",
            if offset < 0 { "-" } else { "+" },
            offset.abs() / 3600,
            offset.abs() % 3600 / 60
        ));
    }
    value
}

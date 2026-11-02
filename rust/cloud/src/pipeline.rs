use crate::ai::{AzureAi, TranscriptionHints};
use crate::domain::{Chunk, ChunkState, RecordingState, Transcript};
use crate::gateway::WebPubSub;
use crate::ports::{BlobLease, QueueReceipt};
use crate::problem::Problem;
use crate::services::{Services, now, preview};
use crate::storage::StorageError;
use crate::stream_worker::StreamWorkError;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

pub struct Pipeline {
    pub services: Arc<Services>,
    pub ai: Arc<AzureAi>,
    pub realtime: Arc<WebPubSub>,
}
fn stopped() -> StreamWorkError {
    StreamWorkError::Pending("Worker shutdown.")
}
fn check_stop(stop: &watch::Receiver<bool>) -> Result<(), StreamWorkError> {
    if *stop.borrow() || stop.has_changed().is_err() {
        Err(stopped())
    } else {
        Ok(())
    }
}
fn codec(_: serde_json::Error) -> StreamWorkError {
    Problem::internal().into()
}
impl Pipeline {
    async fn chunk(&self, id: &str, index: u32) -> Result<Option<Chunk>, StreamWorkError> {
        self.services
            .repository
            .get(
                &self.services.config.chunks_table,
                id,
                &format!("{index:08}"),
            )
            .await?
            .map(|entity| Chunk::from_entity(&entity).map_err(codec))
            .transpose()
    }
    async fn physical<T>(
        &self,
        work: impl Future<Output = Result<T, Problem>>,
        lease: &BlobLease,
        receipt: &QueueReceipt,
        mut stop: watch::Receiver<bool>,
    ) -> Result<T, StreamWorkError> {
        check_stop(&stop)?;
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(work);
        loop {
            tokio::select! {
                result = &mut work => return result.map_err(Into::into),
                _ = heartbeat.tick() => {
                    lease.renew().await?;
                    receipt.renew(self.services.config.queue_visibility_seconds).await?;
                }
                _ = stop.changed() => return Err(stopped()),
            }
        }
    }
    pub async fn transcribe_chunk(
        &self,
        id: &str,
        index: u32,
        receipt: &QueueReceipt,
        stop: watch::Receiver<bool>,
    ) -> Result<(), StreamWorkError> {
        check_stop(&stop)?;
        let Some(chunk) = self.chunk(id, index).await? else {
            return Ok(());
        };
        let record = match self.services.recording(id).await {
            Ok(record) => record,
            Err(error) if error.status == 404 => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if record.transcription_mode != "chunked" {
            return Err(StreamWorkError::Terminal(
                "Legacy work cannot run over a continuous recording.",
            ));
        }
        let path = chunk
            .blob_path
            .clone()
            .unwrap_or_else(|| format!("{id}/{index}.wav"));
        if chunk.state == ChunkState::Transcribed {
            self.services
                .blobs
                .delete(&self.services.config.audio_container, &path)
                .await?;
            self.services.arm_finalize(id).await?;
            return Ok(());
        }
        if chunk.state == ChunkState::Failed || record.state.terminal() {
            return Ok(());
        }
        let lease = BlobLease::existing(
            self.services.blobs.clone(),
            self.services.config.audio_container.clone(),
            path.clone(),
        )
        .await?;
        let result = async {
            let audio = self
                .services
                .blobs
                .get(
                    &self.services.config.audio_container,
                    &path,
                    8 * 1024 * 1024,
                )
                .await?;
            crate::audio::content_digest(Some(&chunk.checksum), &audio)
                .map_err(|_| StreamWorkError::Terminal("Stored audio checksum mismatch."))?;
            let hints = TranscriptionHints {
                language: record.language,
                prompt: format!(
                    "Domain glossary (spell these technical terms correctly): {}.",
                    self.services.config.glossary_terms
                ),
                phrases: self
                    .services
                    .config
                    .glossary_terms
                    .split(',')
                    .map(str::trim)
                    .filter(|term| !term.is_empty())
                    .map(str::to_owned)
                    .collect(),
            };
            let text = self
                .physical(
                    self.ai.transcribe(audio, hints, false),
                    &lease,
                    receipt,
                    stop.clone(),
                )
                .await?;
            for attempt in 0..5 {
                check_stop(&stop)?;
                lease.renew().await?;
                let Some(mut current) = self.chunk(id, index).await? else {
                    return Ok(());
                };
                if current.state == ChunkState::Transcribed {
                    break;
                }
                if current.state == ChunkState::Failed {
                    return Err(StreamWorkError::Terminal(
                        "Chunk was failed during transcription.",
                    ));
                }
                current.state = ChunkState::Transcribed;
                current.text = Some(text.clone());
                match self
                    .services
                    .repository
                    .replace(
                        &self.services.config.chunks_table,
                        current.entity().map_err(codec)?,
                    )
                    .await
                {
                    Ok(_) => break,
                    Err(StorageError::Conflict) if attempt < 4 => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            lease.delete().await?;
            self.services.arm_finalize(id).await?;
            Ok(())
        }
        .await;
        if let Err(error) = lease.release().await {
            if !matches!(error, StorageError::NotFound | StorageError::Conflict) {
                eprintln!("Legacy audio lease release failed: {error}");
                if result.is_ok() {
                    return Err(error.into());
                }
            }
        }
        result
    }
    pub async fn fail_recording(&self, id: &str, reason: &str) -> Result<(), StreamWorkError> {
        for _ in 0..5 {
            let mut record = match self.services.recording(id).await {
                Ok(record) => record,
                Err(error) if error.status == 404 => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            if record.state.terminal() {
                return Ok(());
            }
            record.state = RecordingState::Failed;
            record.failure_reason = Some(reason.into());
            record.updated_at = now();
            match self.services.save_recording(&record).await {
                Ok(_) => return Ok(()),
                Err(StorageError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(Problem::unavailable("Could not publish recording failure.").into())
    }
    pub async fn fail_chunk(&self, id: &str, index: u32) -> Result<(), StreamWorkError> {
        let record = match self.services.recording(id).await {
            Ok(record) => record,
            Err(error) if error.status == 404 => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if record.transcription_mode != "chunked" {
            return Ok(());
        }
        for attempt in 0..5 {
            let Some(mut chunk) = self.chunk(id, index).await? else {
                return Ok(());
            };
            if matches!(chunk.state, ChunkState::Failed | ChunkState::Transcribed) {
                return Ok(());
            }
            chunk.state = ChunkState::Failed;
            match self
                .services
                .repository
                .replace(
                    &self.services.config.chunks_table,
                    chunk.entity().map_err(codec)?,
                )
                .await
            {
                Ok(_) => {
                    self.services
                        .blobs
                        .delete(
                            &self.services.config.audio_container,
                            &chunk
                                .blob_path
                                .unwrap_or_else(|| format!("{id}/{index}.wav")),
                        )
                        .await?;
                    return Ok(());
                }
                Err(StorageError::Conflict) if attempt < 4 => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(Problem::unavailable("Could not publish chunk failure.").into())
    }
    async fn metadata(&self, id: &str) -> Result<Option<Transcript>, StreamWorkError> {
        self.services
            .repository
            .get(&self.services.config.transcripts_table, "transcript", id)
            .await?
            .map(|entity| Transcript::from_entity(&entity).map_err(codec))
            .transpose()
    }
    pub async fn finish_delivery(&self, id: &str) -> Result<(), StreamWorkError> {
        let record = self.services.recording(id).await?;
        let transcript_id = record.transcript_id.ok_or_else(|| {
            Problem::unavailable("Completed recording has no canonical transcript.")
        })?;
        let transcript = self
            .metadata(&transcript_id)
            .await?
            .ok_or_else(|| Problem::unavailable("Canonical transcript metadata is unavailable."))?;
        if !record.stream_delivery_completed {
            self.realtime.notify(&transcript).await?;
            let mut saved = false;
            for _ in 0..5 {
                let mut current = self.services.recording(id).await?;
                if current.stream_delivery_completed {
                    saved = true;
                    break;
                }
                if current.transcript_id.as_deref() != Some(&transcript_id)
                    || current.state != RecordingState::Completed
                {
                    return Err(Problem::unavailable(
                        "Canonical transcript changed during notification.",
                    )
                    .into());
                }
                current.stream_delivery_completed = true;
                current.updated_at = now();
                match self.services.save_recording(&current).await {
                    Ok(_) => {
                        saved = true;
                        break;
                    }
                    Err(StorageError::Conflict) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            if !saved {
                return Err(
                    Problem::unavailable("Could not acknowledge transcript notification.").into(),
                );
            }
        }
        for chunk in self.services.chunks(id).await? {
            let path = chunk
                .blob_path
                .unwrap_or_else(|| format!("{id}/{}.wav", chunk.index));
            self.services
                .blobs
                .delete(&self.services.config.audio_container, &path)
                .await?;
        }
        if let Some(path) = record.stream_raw_path {
            self.services
                .blobs
                .delete(&self.services.config.raw_container, &path)
                .await?;
        }
        Ok(())
    }
    pub async fn finalize(
        &self,
        id: &str,
        receipt: &QueueReceipt,
        stop: watch::Receiver<bool>,
    ) -> Result<(), StreamWorkError> {
        check_stop(&stop)?;
        let record = match self.services.recording(id).await {
            Ok(record) => record,
            Err(error) if error.status == 404 => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if record.state == RecordingState::Completed || record.transcript_id.is_some() {
            return self.finish_delivery(id).await;
        }
        if record.state == RecordingState::Failed || record.expected_chunk_count.is_none() {
            return Ok(());
        }
        let chunks = self.services.chunks(id).await?;
        if chunks.iter().any(|chunk| chunk.state == ChunkState::Failed) {
            return self.fail_recording(id, "transcription_failed").await;
        }
        let expected = record.expected_chunk_count.unwrap_or(0) as usize;
        if chunks.len() < expected
            || (0..expected).any(|index| {
                chunks[index].index as usize != index
                    || record.transcription_mode != "streaming"
                        && chunks[index].state != ChunkState::Transcribed
            })
            || record.transcription_mode == "streaming" && !record.stream_asr_ready
        {
            if record.complete_requested_at.is_some_and(|completed| {
                now() - completed
                    > time::Duration::seconds(i64::from(self.services.config.chunk_grace_seconds))
            }) {
                return self.fail_recording(id, "missing_chunks").await;
            }
            return Err(Problem::unavailable("Waiting for all expected audio/text.").into());
        }
        let lease = BlobLease::acquire(
            self.services.blobs.clone(),
            self.services.config.raw_container.clone(),
            format!("finalize/{id}.lock"),
        )
        .await?;
        let result = async {
            let mut current = self.services.recording(id).await?;
            if current.state == RecordingState::Completed {
                return self.finish_delivery(id).await;
            }
            if current.state == RecordingState::Failed {
                return Ok(());
            }
            if current.state != RecordingState::Refining {
                if !current.state.can_transition(RecordingState::Refining) {
                    return Err(Problem::conflict(
                        "Recording cannot be finalized in its current state.",
                    )
                    .into());
                }
                current.state = RecordingState::Refining;
                current.updated_at = now();
                self.services.save_recording(&current).await?;
            }
            let transcript_id = crate::pipeline_text::transcript_id(id);
            let raw = if record.transcription_mode == "streaming" {
                let path = record
                    .stream_raw_path
                    .as_ref()
                    .ok_or(StreamWorkError::Terminal(
                        "Streaming raw publication is missing.",
                    ))?;
                String::from_utf8(
                    self.services
                        .blobs
                        .get(&self.services.config.raw_container, path, 16 * 1024 * 1024)
                        .await?,
                )
                .map_err(|_| {
                    StreamWorkError::Terminal("Streaming raw publication is invalid UTF-8.")
                })?
            } else {
                crate::pipeline_text::stitch_chunks(
                    &chunks
                        .iter()
                        .take(expected)
                        .map(|chunk| chunk.text.clone().unwrap_or_default())
                        .collect::<Vec<_>>(),
                    self.services.config.overlap_min_chars,
                    60,
                )
            };
            match self
                .services
                .blobs
                .put_if_absent(
                    &self.services.config.raw_container,
                    &format!("{id}.txt"),
                    raw.as_bytes().to_vec(),
                    "text/plain; charset=utf-8",
                )
                .await
            {
                Ok(()) | Err(StorageError::Conflict) => {}
                Err(error) => return Err(error.into()),
            }
            let body_path = format!("{transcript_id}.txt");
            let canonical = match self
                .services
                .blobs
                .get(
                    &self.services.config.transcript_container,
                    &body_path,
                    16 * 1024 * 1024,
                )
                .await
            {
                Ok(body) => body,
                Err(StorageError::NotFound) => {
                    let text = if record.refinement_enabled {
                        self.physical(
                            self.ai.refine_recording(&raw, &record.refine_model),
                            &lease,
                            receipt,
                            stop.clone(),
                        )
                        .await?
                    } else {
                        raw.clone()
                    };
                    check_stop(&stop)?;
                    lease.renew().await?;
                    match self
                        .services
                        .blobs
                        .put_if_absent(
                            &self.services.config.transcript_container,
                            &body_path,
                            text.into_bytes(),
                            "text/plain; charset=utf-8",
                        )
                        .await
                    {
                        Ok(()) | Err(StorageError::Conflict) => {}
                        Err(error) => return Err(error.into()),
                    }
                    self.services
                        .blobs
                        .get(
                            &self.services.config.transcript_container,
                            &body_path,
                            16 * 1024 * 1024,
                        )
                        .await?
                }
                Err(error) => return Err(error.into()),
            };
            let canonical = String::from_utf8(canonical)
                .map_err(|_| StreamWorkError::Terminal("Canonical transcript is invalid UTF-8."))?;
            check_stop(&stop)?;
            lease.renew().await?;
            let completed = now();
            let transcript = Transcript {
                transcript_id: transcript_id.clone(),
                recording_id: id.into(),
                preview: preview(&canonical, 140),
                language: record.language,
                refine_model: if record.refinement_enabled {
                    record.refine_model
                } else {
                    "none".into()
                },
                completed_at: completed,
                expires_at: completed + time::Duration::hours(48),
                character_count: canonical.chars().count(),
                body_path,
                source: "recording".into(),
                transcribe_model: if record.transcription_mode == "streaming" {
                    Some(self.services.config.recording_stream.clone())
                } else {
                    None
                },
                content_digest: None,
                etag: String::new(),
            };
            let published = match self
                .services
                .repository
                .insert(
                    &self.services.config.transcripts_table,
                    transcript.entity().map_err(codec)?,
                )
                .await
            {
                Ok(entity) => Transcript::from_entity(&entity).map_err(codec)?,
                Err(StorageError::Conflict) => self
                    .metadata(&transcript_id)
                    .await?
                    .ok_or_else(Problem::internal)?,
                Err(error) => return Err(error.into()),
            };
            if published.recording_id != id
                || published.body_path != format!("{transcript_id}.txt")
                || published.character_count != canonical.chars().count()
                || published.preview != preview(&canonical, 140)
            {
                return Err(StreamWorkError::Terminal(
                    "Canonical transcript metadata conflicts with its immutable body.",
                ));
            }
            let mut saved = false;
            for _ in 0..5 {
                lease.renew().await?;
                let mut current = self.services.recording(id).await?;
                if current.state == RecordingState::Completed {
                    saved = true;
                    break;
                }
                if current.state == RecordingState::Failed {
                    return Err(StreamWorkError::Terminal(
                        "Recording failed before final publication.",
                    ));
                }
                current.state = RecordingState::Completed;
                current.transcript_id = Some(transcript_id.clone());
                current.updated_at = now();
                match self.services.save_recording(&current).await {
                    Ok(_) => {
                        saved = true;
                        break;
                    }
                    Err(StorageError::Conflict) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            if !saved {
                return Err(Problem::unavailable("Could not publish completed recording.").into());
            }
            self.finish_delivery(id).await
        }
        .await;
        if let Err(error) = lease.release().await {
            eprintln!("Finalizer lease release failed: {error}");
            if result.is_ok() {
                return Err(error.into());
            }
        }
        result
    }
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;

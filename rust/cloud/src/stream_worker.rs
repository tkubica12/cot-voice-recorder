use crate::domain::Recording;
use crate::problem::Problem;
use crate::services::{Services, now};
use crate::storage::StorageError;
use std::sync::Arc;

#[derive(Debug)]
pub enum StreamWorkError {
    Pending(&'static str),
    Terminal(&'static str),
    Failure(Problem),
}
impl From<Problem> for StreamWorkError {
    fn from(value: Problem) -> Self {
        Self::Failure(value)
    }
}
impl From<StorageError> for StreamWorkError {
    fn from(value: StorageError) -> Self {
        Self::Failure(crate::services::storage(value))
    }
}
impl std::fmt::Display for StreamWorkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending(reason) => write!(formatter, "Stream work pending: {reason}"),
            Self::Terminal(reason) => write!(formatter, "Stream work terminal: {reason}"),
            Self::Failure(problem) => std::fmt::Display::fmt(problem, formatter),
        }
    }
}
impl std::error::Error for StreamWorkError {}

pub struct StreamLeases {
    services: Arc<Services>,
}
impl StreamLeases {
    pub fn new(services: Arc<Services>) -> Self {
        Self { services }
    }
    pub async fn claim(&self, id: &str, owner: &str) -> Result<Option<Recording>, StreamWorkError> {
        if owner.is_empty() || owner.len() > 1024 || owner.chars().any(char::is_control) {
            return Err(Problem::validation("Invalid stream lease owner.").into());
        }
        for _ in 0..8 {
            let mut record = match self.services.recording(id).await {
                Ok(record) => record,
                Err(error) if error.status == 404 => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            if record.state.terminal() {
                return Ok(None);
            }
            if record.stream_asr_ready {
                self.services.arm_finalize(id).await?;
                return Ok(None);
            }
            let current = now();
            if record.stream_lease_id.is_some()
                && record
                    .stream_lease_expires_at
                    .is_some_and(|expiry| expiry > current)
            {
                return Ok(None);
            }
            record.stream_lease_id = Some(owner.into());
            record.stream_lease_expires_at = Some(
                current
                    + time::Duration::seconds(i64::from(
                        self.services.config.recording_stream_lease_seconds,
                    )),
            );
            record.stream_attempt = record
                .stream_attempt
                .checked_add(1)
                .ok_or_else(Problem::internal)?;
            record.streamed_samples = 0;
            record.stream_preview.clear();
            record.stream_error = None;
            record.updated_at = current;
            match self.services.save_recording(&record).await {
                Ok(record) => return Ok(Some(record)),
                Err(StorageError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(StreamWorkError::Pending("Recording lease is contended."))
    }
    pub async fn update_owned(
        &self,
        id: &str,
        owner: &str,
        mut changes: impl FnMut(&mut Recording),
    ) -> Result<Recording, StreamWorkError> {
        for _ in 0..8 {
            let mut record = match self.services.recording(id).await {
                Ok(record) => record,
                Err(error) if error.status == 404 => {
                    return Err(StreamWorkError::Pending("Recording lease was lost."));
                }
                Err(error) => return Err(error.into()),
            };
            let current = now();
            if record.state.terminal() || record.stream_lease_id.as_deref() != Some(owner) {
                return Err(StreamWorkError::Pending("Recording lease was lost."));
            }
            if !record
                .stream_lease_expires_at
                .is_some_and(|expiry| expiry > current)
            {
                return Err(StreamWorkError::Pending("Recording lease expired."));
            }
            changes(&mut record);
            record.updated_at = current;
            match self.services.save_recording(&record).await {
                Ok(record) => return Ok(record),
                Err(StorageError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(StreamWorkError::Pending(
            "Recording lease update is contended.",
        ))
    }
    pub async fn renew(&self, id: &str, owner: &str) -> Result<Recording, StreamWorkError> {
        self.update_owned(id, owner, |record| {
            record.stream_lease_expires_at = Some(
                now()
                    + time::Duration::seconds(i64::from(
                        self.services.config.recording_stream_lease_seconds,
                    )),
            );
        })
        .await
    }
    pub async fn release(
        &self,
        id: &str,
        owner: &str,
        error: Option<&'static str>,
    ) -> Result<(), StreamWorkError> {
        for _ in 0..8 {
            let mut record = match self.services.recording(id).await {
                Ok(record) => record,
                Err(error) if error.status == 404 => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            if record.stream_lease_id.as_deref() != Some(owner) {
                return Ok(());
            }
            record.stream_lease_id = None;
            record.stream_lease_expires_at = None;
            record.stream_error = if record.stream_asr_ready {
                None
            } else {
                error.map(str::to_owned)
            };
            record.updated_at = now();
            match self.services.save_recording(&record).await {
                Ok(_) => return Ok(()),
                Err(StorageError::Conflict) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(StreamWorkError::Failure(Problem::unavailable(
            "Could not release recording lease.",
        )))
    }
}

pub async fn pcm_for_chunk(
    services: &Services,
    id: &str,
    index: u32,
    cursor: u64,
) -> Result<Option<Vec<u8>>, StreamWorkError> {
    let entity = services
        .repository
        .get(&services.config.chunks_table, id, &format!("{index:08}"))
        .await?;
    let Some(entity) = entity else {
        return Ok(None);
    };
    let chunk = crate::domain::Chunk::from_entity(&entity).map_err(|_| Problem::internal())?;
    let path = chunk
        .blob_path
        .unwrap_or_else(|| format!("{id}/{index}.wav"));
    let wav = services
        .blobs
        .get(&services.config.audio_container, &path, 8 * 1024 * 1024)
        .await?;
    crate::audio::content_digest(Some(&chunk.checksum), &wav)
        .map_err(|_| StreamWorkError::Terminal("Stored audio checksum mismatch."))?;
    let pcm = voiceprompt_core::wav::capture_pcm(&wav)
        .map_err(|_| StreamWorkError::Terminal("Stored streaming WAV is invalid."))?;
    let samples = (pcm.len() / 2) as u64;
    let start = chunk
        .start_sample
        .ok_or(StreamWorkError::Terminal("Missing source sample metadata."))?;
    if chunk.sample_count != Some(samples) {
        return Err(StreamWorkError::Terminal(
            "Stored sample count does not match audio.",
        ));
    }
    let skip = cursor
        .checked_sub(start)
        .filter(|skip| *skip < samples)
        .ok_or(StreamWorkError::Terminal(
            "Gap or redundant source audio range.",
        ))?;
    Ok(Some(pcm[skip as usize * 2..].to_vec()))
}

#[cfg(test)]
#[path = "stream_worker_tests.rs"]
mod tests;

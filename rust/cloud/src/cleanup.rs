use crate::domain::{Recording, Transcript};
use crate::ports::BlobLease;
use crate::problem::Problem;
use crate::services::{Services, now};
use crate::storage::StorageError;

#[derive(Default, serde::Serialize)]
pub struct CleanupResult {
    pub transcripts_deleted: usize,
    pub recordings_deleted: usize,
    pub audio_blobs_deleted: usize,
}
fn codec(_: serde_json::Error) -> StorageError {
    StorageError::Failure(Problem::internal())
}
async fn release(lease: &BlobLease) -> Result<(), StorageError> {
    match lease.release().await {
        Ok(()) | Err(StorageError::NotFound | StorageError::Conflict) => Ok(()),
        Err(error) => Err(error),
    }
}
async fn delete_blob(
    services: &Services,
    container: &str,
    name: &str,
) -> Result<bool, StorageError> {
    let lease =
        match BlobLease::existing(services.blobs.clone(), container.into(), name.into()).await {
            Ok(lease) => lease,
            Err(StorageError::NotFound | StorageError::Conflict) => return Ok(false),
            Err(error) => return Err(error),
        };
    match lease.delete().await {
        Ok(deleted) => Ok(deleted),
        Err(error) => {
            if let Err(release) = release(&lease).await {
                eprintln!("Cleanup release failed: {release}");
            }
            Err(error)
        }
    }
}
fn active(record: &Recording, current: time::OffsetDateTime) -> bool {
    record.stream_lease_id.is_some()
        && record
            .stream_lease_expires_at
            .is_some_and(|expires| expires > current)
}
async fn expired_record(
    services: &Services,
    record: &Recording,
    cutoff: time::OffsetDateTime,
) -> Result<Option<usize>, StorageError> {
    let id = &record.recording_id;
    let lock = match BlobLease::acquire(
        services.blobs.clone(),
        services.config.raw_container.clone(),
        format!("finalize/{id}.lock"),
    )
    .await
    {
        Ok(lock) => lock,
        Err(StorageError::Conflict) => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut audio = Vec::new();
    let outcome = async {
        for entry in services
            .blobs
            .list(&services.config.audio_container, &format!("{id}/"))
            .await?
        {
            match BlobLease::existing(
                services.blobs.clone(),
                services.config.audio_container.clone(),
                entry.name,
            )
            .await
            {
                Ok(lease) => audio.push(lease),
                Err(StorageError::NotFound) => {}
                Err(StorageError::Conflict) => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        let mut current = match services.recording(id).await {
            Ok(record) => record,
            Err(error) if error.status == 404 => return Ok(None),
            Err(error) => return Err(StorageError::Failure(error)),
        };
        if current.etag != record.etag || current.updated_at >= cutoff || active(&current, now()) {
            return Ok(None);
        }
        lock.renew().await?;
        for lease in &audio {
            lease.renew().await?;
        }
        if !current.state.terminal() {
            current.state = crate::domain::RecordingState::Failed;
            current.failure_reason = Some("internal_error".into());
        }
        current.stream_lease_id = Some(format!(
            "cleanup-{}",
            crate::services::uuid().map_err(StorageError::Failure)?
        ));
        current.stream_lease_expires_at = Some(now() + time::Duration::seconds(120));
        let current = match services.save_recording(&current).await {
            Ok(record) => record,
            Err(StorageError::Conflict | StorageError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut removed = 0;
        for lease in &audio {
            lease.renew().await?;
            if lease.delete().await? {
                removed += 1;
            }
        }
        for chunk in services.chunks(id).await.map_err(StorageError::Failure)? {
            lock.renew().await?;
            match services
                .repository
                .delete(
                    &services.config.chunks_table,
                    id,
                    &format!("{:08}", chunk.index),
                    &chunk.etag,
                )
                .await
            {
                Ok(()) | Err(StorageError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        for path in services
            .blobs
            .list(&services.config.raw_container, &format!("stream/{id}/"))
            .await?
        {
            lock.renew().await?;
            delete_blob(services, &services.config.raw_container, &path.name).await?;
        }
        delete_blob(
            services,
            &services.config.raw_container,
            &format!("{id}.txt"),
        )
        .await?;
        if let Some(index) = services
            .repository
            .get(
                &services.config.recordings_table,
                "crid",
                &current.client_recording_id,
            )
            .await?
        {
            if index.values["recording_id"].as_str() == Some(id) {
                match services
                    .repository
                    .delete(
                        &services.config.recordings_table,
                        "crid",
                        &current.client_recording_id,
                        &index.etag,
                    )
                    .await
                {
                    Ok(()) | Err(StorageError::NotFound | StorageError::Conflict) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        lock.renew().await?;
        services
            .repository
            .delete(
                &services.config.recordings_table,
                "recording",
                id,
                &current.etag,
            )
            .await?;
        Ok(Some(removed))
    }
    .await;
    let mut release_error = None;
    for lease in &audio {
        if let Err(error) = release(lease).await {
            eprintln!("Cleanup audio lease release failed: {error}");
            release_error = Some(error);
        }
    }
    let lock_result = if matches!(&outcome, Ok(Some(_))) {
        lock.delete().await.map(|_| ())
    } else {
        release(&lock).await
    };
    if let Err(error) = lock_result {
        eprintln!("Cleanup lock release failed: {error}");
        release_error = Some(error);
    }
    if let Some(error) = release_error {
        if outcome.is_ok() {
            return Err(error);
        }
    }
    outcome
}
pub async fn run(services: &Services) -> Result<CleanupResult, StorageError> {
    let current = now();
    let cutoff = current - time::Duration::hours(48);
    let mut result = CleanupResult::default();
    for entity in services
        .repository
        .partition(&services.config.transcripts_table, "transcript")
        .await?
    {
        let transcript = Transcript::from_entity(&entity).map_err(codec)?;
        if transcript.expires_at > current {
            continue;
        }
        let lease = match BlobLease::existing(
            services.blobs.clone(),
            services.config.transcript_container.clone(),
            transcript.body_path.clone(),
        )
        .await
        {
            Ok(lease) => Some(lease),
            Err(StorageError::NotFound) => None,
            Err(StorageError::Conflict) => continue,
            Err(error) => return Err(error),
        };
        if let Some(lease) = &lease {
            if let Err(error) = lease.delete().await {
                if let Err(release) = release(lease).await {
                    eprintln!("Expired transcript release failed: {release}");
                }
                return Err(error);
            }
        }
        let deleted = services
            .repository
            .delete(
                &services.config.transcripts_table,
                "transcript",
                &transcript.transcript_id,
                &entity.etag,
            )
            .await;
        match deleted {
            Ok(()) => {
                result.transcripts_deleted += 1;
            }
            Err(StorageError::NotFound | StorageError::Conflict) => {}
            Err(error) => {
                if let Some(lease) = &lease {
                    release(lease).await?;
                }
                return Err(error);
            }
        }
        if let Some(lease) = &lease {
            release(lease).await?;
        }
    }
    for entry in services
        .blobs
        .list(&services.config.transcript_container, "windows-dictation/")
        .await?
    {
        let parts: Vec<_> = entry.name.split('/').collect();
        let metadata = if parts.len() == 3 {
            services
                .repository
                .get(&services.config.transcripts_table, "transcript", parts[1])
                .await?
        } else {
            None
        };
        if metadata
            .as_ref()
            .is_some_and(|entity| entity.values["body_path"].as_str() == Some(&entry.name))
        {
            continue;
        }
        let bytes = match services
            .blobs
            .get(
                &services.config.transcript_container,
                &entry.name,
                16 * 1024 * 1024,
            )
            .await
        {
            Ok(bytes) => bytes,
            Err(StorageError::NotFound) => continue,
            Err(error) => return Err(error),
        };
        let completion = serde_json::from_slice::<crate::models::ArchiveDictation>(&bytes)
            .ok()
            .map(|archive| archive.completed_at);
        let expired = if let Some(completed) = completion {
            completed + time::Duration::hours(48) <= current
        } else {
            eprintln!(
                "Cleanup found an invalid orphan audit; using its object retention timestamp."
            );
            entry.modified_at < cutoff
        };
        if expired {
            delete_blob(services, &services.config.transcript_container, &entry.name).await?;
        }
    }
    for entry in services
        .blobs
        .list(&services.config.transcript_container, "")
        .await?
    {
        if entry.modified_at >= cutoff || entry.name.contains('/') || !entry.name.ends_with(".txt")
        {
            continue;
        }
        let Some(id) = entry.name.strip_suffix(".txt") else {
            continue;
        };
        let lease = match BlobLease::existing(
            services.blobs.clone(),
            services.config.transcript_container.clone(),
            entry.name.clone(),
        )
        .await
        {
            Ok(lease) => lease,
            Err(StorageError::Conflict | StorageError::NotFound) => continue,
            Err(error) => return Err(error),
        };
        let metadata = services
            .repository
            .get(&services.config.transcripts_table, "transcript", id)
            .await?;
        if metadata.is_none() {
            lease.delete().await?;
        } else {
            release(&lease).await?;
        }
    }
    for entity in services
        .repository
        .partition(&services.config.recordings_table, "recording")
        .await?
    {
        let record = Recording::from_entity(&entity).map_err(codec)?;
        if record.updated_at >= cutoff || active(&record, current) {
            continue;
        }
        if let Some(deleted) = expired_record(services, &record, cutoff).await? {
            result.recordings_deleted += 1;
            result.audio_blobs_deleted += deleted;
        }
    }
    for entry in services
        .blobs
        .list(&services.config.audio_container, "")
        .await?
    {
        let id = entry.name.split('/').next().unwrap_or("");
        let record = match services.recording(id).await {
            Ok(record) => Some(record),
            Err(error) if error.status == 404 => None,
            Err(error) => return Err(StorageError::Failure(error)),
        };
        if record
            .as_ref()
            .is_some_and(|record| active(record, current))
        {
            continue;
        }
        let settled = record.as_ref().is_some_and(|record| {
            record.state.terminal()
                && (record.transcription_mode != "streaming"
                    || record.state == crate::domain::RecordingState::Completed
                        && record.stream_delivery_completed)
        });
        if (settled || entry.modified_at < cutoff && record.is_none())
            && delete_blob(services, &services.config.audio_container, &entry.name).await?
        {
            result.audio_blobs_deleted += 1;
        }
    }
    for prefix in ["stream/", "finalize/"] {
        for entry in services
            .blobs
            .list(&services.config.raw_container, prefix)
            .await?
        {
            if entry.modified_at >= cutoff {
                continue;
            }
            let id = entry.name.split('/').nth(1).unwrap_or("");
            let record = match services.recording(id).await {
                Ok(record) => Some(record),
                Err(error) if error.status == 404 => None,
                Err(error) => return Err(StorageError::Failure(error)),
            };
            if record
                .as_ref()
                .is_some_and(|record| !record.state.terminal() || active(record, current))
            {
                continue;
            }
            delete_blob(services, &services.config.raw_container, &entry.name).await?;
        }
    }
    Ok(result)
}

#[cfg(test)]
#[path = "cleanup_tests.rs"]
mod tests;

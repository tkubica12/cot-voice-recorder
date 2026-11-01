use crate::storage::Entity;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingState {
    Recording,
    Uploading,
    Transcribing,
    Refining,
    Completed,
    Failed,
}

impl RecordingState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
    pub fn can_transition(self, next: Self) -> bool {
        self == next
            || match self {
                Self::Recording => {
                    matches!(next, Self::Uploading | Self::Transcribing | Self::Failed)
                }
                Self::Uploading => matches!(next, Self::Transcribing | Self::Failed),
                Self::Transcribing => matches!(next, Self::Refining | Self::Failed),
                Self::Refining => matches!(next, Self::Completed | Self::Failed),
                Self::Completed | Self::Failed => false,
            }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkState {
    Accepted,
    Transcribed,
    Failed,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Recording {
    pub recording_id: String,
    pub client_recording_id: String,
    pub state: RecordingState,
    pub refine_model: String,
    pub language: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(default)]
    pub expected_chunk_count: Option<u32>,
    #[serde(default)]
    pub transcript_id: Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub complete_requested_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub finalize_enqueued: bool,
    #[serde(default = "chunked")]
    pub transcription_mode: String,
    #[serde(default = "legacy_overlap")]
    pub audio_layout: String,
    #[serde(default = "enabled")]
    pub refinement_enabled: bool,
    #[serde(default)]
    pub stream_lease_id: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub stream_lease_expires_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub stream_raw_path: Option<String>,
    #[serde(default)]
    pub stream_asr_ready: bool,
    #[serde(default)]
    pub stream_attempt: u32,
    #[serde(default)]
    pub streamed_samples: u64,
    #[serde(default)]
    pub stream_preview: String,
    #[serde(default)]
    pub stream_error: Option<String>,
    #[serde(default)]
    pub stream_delivery_completed: bool,
    #[serde(skip)]
    pub etag: String,
}
fn chunked() -> String {
    "chunked".into()
}
fn legacy_overlap() -> String {
    "legacy_overlap".into()
}
fn enabled() -> bool {
    true
}
fn recording() -> String {
    "recording".into()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub recording_id: String,
    pub index: u32,
    pub checksum: String,
    pub state: ChunkState,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    pub size_bytes: u64,
    pub blob_path: Option<String>,
    pub text: Option<String>,
    pub duration_ms: Option<u32>,
    pub overlap_ms: Option<u32>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    pub start_sample: Option<u64>,
    pub sample_count: Option<u64>,
    #[serde(skip)]
    pub etag: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub transcript_id: String,
    pub recording_id: String,
    pub preview: String,
    pub language: String,
    pub refine_model: String,
    #[serde(with = "time::serde::rfc3339")]
    pub completed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub character_count: usize,
    pub body_path: String,
    #[serde(default = "recording")]
    pub source: String,
    pub transcribe_model: Option<String>,
    pub content_digest: Option<String>,
    #[serde(skip)]
    pub etag: String,
}

const DATES: &[&str] = &[
    "created_at",
    "updated_at",
    "complete_requested_at",
    "stream_lease_expires_at",
    "received_at",
    "started_at",
    "completed_at",
    "expires_at",
];

fn to_entity<T: Serialize>(
    value: &T,
    partition: &str,
    row: &str,
    etag: &str,
) -> Result<Entity, serde_json::Error> {
    let json = serde_json::to_value(value)?;
    let mut values = match json {
        serde_json::Value::Object(values) => values,
        _ => unreachable!("domain structs serialize as objects"),
    };
    values.retain(|_, value| !value.is_null());
    values.insert("PartitionKey".into(), serde_json::json!(partition));
    values.insert("RowKey".into(), serde_json::json!(row));
    for name in DATES {
        if let Some(serde_json::Value::String(value)) = values.get(*name) {
            let date = OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                .map_err(|_| {
                    <serde_json::Error as serde::ser::Error>::custom("Invalid entity timestamp.")
                })?;
            let date = OffsetDateTime::from_unix_timestamp_nanos(
                date.unix_timestamp_nanos().div_euclid(1000) * 1000,
            )
            .map_err(|_| {
                <serde_json::Error as serde::ser::Error>::custom("Invalid entity timestamp range.")
            })?;
            let date = date
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| {
                    <serde_json::Error as serde::ser::Error>::custom(
                        "Invalid entity timestamp format.",
                    )
                })?;
            values.insert((*name).into(), serde_json::json!(date));
            values.insert(
                format!("{name}@odata.type"),
                serde_json::json!("Edm.DateTime"),
            );
        }
    }
    let integer_fields: Vec<_> = values
        .iter()
        .filter(|(_, value)| value.is_u64() || value.is_i64())
        .map(|(key, _)| key.clone())
        .collect();
    for name in integer_fields {
        if let Some(value) = values.get_mut(&name) {
            if value
                .as_i64()
                .is_some_and(|number| (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&number))
            {
                values.insert(format!("{name}@odata.type"), serde_json::json!("Edm.Int32"));
            } else {
                let number = value.as_i64().ok_or_else(|| {
                    <serde_json::Error as serde::ser::Error>::custom(
                        "Entity integer exceeds Azure Int64 bounds.",
                    )
                })?;
                *value = serde_json::Value::String(number.to_string());
                values.insert(format!("{name}@odata.type"), serde_json::json!("Edm.Int64"));
            }
        }
    }
    Ok(Entity {
        values,
        etag: etag.into(),
    })
}

fn from_entity<T: serde::de::DeserializeOwned>(entity: &Entity) -> Result<T, serde_json::Error> {
    let mut values = entity.values.clone();
    let integers: Vec<_> = values
        .iter()
        .filter(|(key, value)| key.ends_with("@odata.type") && value.as_str() == Some("Edm.Int64"))
        .map(|(key, _)| key.trim_end_matches("@odata.type").to_owned())
        .collect();
    for key in integers {
        if let Some(serde_json::Value::String(value)) = values.get(&key) {
            if let Ok(number) = value.parse::<serde_json::Number>() {
                values.insert(key, serde_json::Value::Number(number));
            }
        }
    }
    serde_json::from_value(serde_json::Value::Object(values))
}

impl Recording {
    pub fn entity(&self) -> Result<Entity, serde_json::Error> {
        to_entity(self, "recording", &self.recording_id, &self.etag)
    }
    pub fn from_entity(entity: &Entity) -> Result<Self, serde_json::Error> {
        let mut record: Self = from_entity(entity)?;
        if entity
            .values
            .get("PartitionKey")
            .and_then(serde_json::Value::as_str)
            != Some("recording")
            || entity
                .values
                .get("RowKey")
                .and_then(serde_json::Value::as_str)
                != Some(record.recording_id.as_str())
        {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "Recording entity key does not match its payload.",
            ));
        }
        record.etag = entity.etag.clone();
        Ok(record)
    }
}
impl Chunk {
    pub fn entity(&self) -> Result<Entity, serde_json::Error> {
        let mut entity = to_entity(
            self,
            &self.recording_id,
            &format!("{:08}", self.index),
            &self.etag,
        )?;
        entity.values.remove("recording_id");
        Ok(entity)
    }
    pub fn from_entity(entity: &Entity) -> Result<Self, serde_json::Error> {
        let mut entity = entity.clone();
        entity.values.insert(
            "recording_id".into(),
            entity
                .values
                .get("PartitionKey")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        let mut chunk: Self = from_entity(&entity)?;
        if entity
            .values
            .get("RowKey")
            .and_then(serde_json::Value::as_str)
            != Some(format!("{:08}", chunk.index).as_str())
        {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "Chunk entity index/key mismatch.",
            ));
        }
        chunk.etag = entity.etag;
        Ok(chunk)
    }
}
impl Transcript {
    pub fn entity(&self) -> Result<Entity, serde_json::Error> {
        let mut entity = to_entity(self, "transcript", &self.transcript_id, &self.etag)?;
        entity.values.remove("transcript_id");
        Ok(entity)
    }
    pub fn from_entity(entity: &Entity) -> Result<Self, serde_json::Error> {
        let mut entity = entity.clone();
        entity.values.insert(
            "transcript_id".into(),
            entity
                .values
                .get("RowKey")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        let mut transcript: Self = from_entity(&entity)?;
        transcript.etag = entity.etag;
        Ok(transcript)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_recordings_keep_pipeline_defaults_and_table_edm_types_roundtrip() {
        let raw = serde_json::json!({
            "PartitionKey":"recording","RowKey":"fixture","recording_id":"fixture","client_recording_id":"client",
            "state":"recording","refine_model":"gpt-6-luna","language":"cs",
            "created_at":"2026-11-01T10:00:00Z","updated_at":"2026-11-01T10:00:00Z",
        });
        let entity = Entity {
            values: raw.as_object().unwrap().clone(),
            etag: "opaque-etag".into(),
        };
        let record = Recording::from_entity(&entity).unwrap();
        assert_eq!(record.transcription_mode, "chunked");
        assert_eq!(record.audio_layout, "legacy_overlap");
        assert!(record.refinement_enabled);
        let encoded = record.entity().unwrap();
        assert_eq!(encoded.values["PartitionKey"], "recording");
        assert_eq!(encoded.values["created_at@odata.type"], "Edm.DateTime");
        assert_eq!(
            Recording::from_entity(&encoded).unwrap().etag,
            "opaque-etag"
        );
    }
    #[test]
    fn state_machine_preserves_terminal_and_legacy_forward_transition_rules() {
        assert!(RecordingState::Recording.can_transition(RecordingState::Transcribing));
        assert!(!RecordingState::Transcribing.can_transition(RecordingState::Completed));
        assert!(RecordingState::Refining.can_transition(RecordingState::Completed));
        assert!(RecordingState::Completed.can_transition(RecordingState::Completed));
        assert!(!RecordingState::Completed.can_transition(RecordingState::Failed));
    }
}

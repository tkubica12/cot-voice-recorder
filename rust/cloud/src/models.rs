use crate::domain::{ChunkState, RecordingState};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientInfo {
    pub platform: Platform,
    pub app_version: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Android,
    Windows,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub enum RefineModel {
    #[serde(rename = "gpt-6-luna")]
    #[default]
    Gpt6Luna,
    #[serde(rename = "gpt-5.6-luna")]
    Gpt56Luna,
    #[serde(rename = "gpt-5.6-terra")]
    Gpt56Terra,
}
impl RefineModel {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Gpt6Luna => "gpt-6-luna",
            Self::Gpt56Luna => "gpt-5.6-luna",
            Self::Gpt56Terra => "gpt-5.6-terra",
        }
    }
}
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionMode {
    #[default]
    Chunked,
    Streaming,
}
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioLayout {
    #[default]
    LegacyOverlap,
    Contiguous,
}
fn cs() -> String {
    "cs".into()
}
fn auto() -> String {
    "auto".into()
}
fn enabled() -> bool {
    true
}
fn stream_model() -> String {
    "MAI-Transcribe-2-Streaming".into()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRecording {
    pub client_recording_id: String,
    #[serde(default)]
    pub refine_model: RefineModel,
    #[serde(default = "cs")]
    pub language: String,
    pub client: Option<ClientInfo>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub transcription_mode: TranscriptionMode,
    #[serde(default)]
    pub audio_layout: AudioLayout,
    #[serde(default = "enabled")]
    pub refinement_enabled: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteRecording {
    pub chunk_count: u32,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub stopped_at: Option<OffsetDateTime>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RecordingProgress {
    pub expected_chunk_count: Option<u32>,
    pub received_chunk_count: usize,
    pub transcribed_chunk_count: usize,
    pub streamed_audio_ms: u64,
    pub stream_preview: String,
    pub stream_attempt: u32,
    pub stream_error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Recording {
    pub recording_id: String,
    pub client_recording_id: String,
    pub state: RecordingState,
    pub refine_model: String,
    pub language: String,
    pub progress: RecordingProgress,
    pub transcript_id: Option<String>,
    pub failure_reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub transcription_mode: String,
    pub audio_layout: String,
    pub refinement_enabled: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ChunkAccepted {
    pub recording_id: String,
    pub index: u32,
    pub chunk_state: ChunkState,
    pub checksum: String,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TranscriptSummary {
    pub transcript_id: String,
    pub recording_id: String,
    pub preview: String,
    #[serde(with = "time::serde::rfc3339")]
    pub completed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub language: String,
    pub refine_model: String,
    pub source: String,
    pub transcribe_model: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub transcript_id: String,
    pub recording_id: String,
    pub body: String,
    pub preview: String,
    pub language: String,
    pub refine_model: String,
    #[serde(with = "time::serde::rfc3339")]
    pub completed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub character_count: usize,
    pub source: String,
    pub transcribe_model: Option<String>,
    pub raw_body: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TranscriptPage {
    pub items: Vec<TranscriptSummary>,
    pub next_cursor: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Negotiate {
    pub url: String,
    pub hub: String,
    pub group: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NegotiateRequest {
    pub client: Option<ClientInfo>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveDictation {
    pub text: String,
    pub raw_text: String,
    #[serde(default = "auto")]
    pub language: String,
    #[serde(with = "time::serde::rfc3339")]
    pub completed_at: OffsetDateTime,
    #[serde(default)]
    pub polished: bool,
    #[serde(default = "stream_model")]
    pub transcribe_model: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefineDictation {
    pub text: String,
    #[serde(default)]
    pub previous_text: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Edit {
    pub original: String,
    pub replacement: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Refinement {
    pub text: String,
    pub edits: Vec<Edit>,
}

pub fn summary(value: &crate::domain::Transcript) -> TranscriptSummary {
    TranscriptSummary {
        transcript_id: value.transcript_id.clone(),
        recording_id: value.recording_id.clone(),
        preview: value.preview.clone(),
        completed_at: value.completed_at,
        expires_at: value.expires_at,
        language: value.language.clone(),
        refine_model: value.refine_model.clone(),
        source: value.source.clone(),
        transcribe_model: value.transcribe_model.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_create_contract_defaults_are_preserved_and_unknown_fields_or_models_are_rejected() {
        let request: CreateRecording =
            serde_json::from_str(r#"{"client_recording_id":"fixture"}"#).unwrap();
        assert_eq!(request.refine_model.name(), "gpt-6-luna");
        assert_eq!(request.language, "cs");
        assert!(request.refinement_enabled);
        assert!(matches!(
            request.transcription_mode,
            TranscriptionMode::Chunked
        ));
        assert!(matches!(request.audio_layout, AudioLayout::LegacyOverlap));
        assert!(
            serde_json::from_str::<CreateRecording>(
                r#"{"client_recording_id":"fixture","refine_model":"unsupported"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<CreateRecording>(
                r#"{"client_recording_id":"fixture","unknown":true}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<CompleteRecording>(r#"{"chunk_count":-1}"#).is_err());
    }
}

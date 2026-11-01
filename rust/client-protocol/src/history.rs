use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ProtocolError;

pub const RETENTION_SECONDS: i64 = 48 * 60 * 60;
pub const MAX_ENTRIES: usize = 200;
pub const MAX_HISTORY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub transcript_id: String,
    pub recording_id: String,
    pub preview: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_body: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub refinement_fallback_blocks: usize,
    #[serde(with = "time::serde::rfc3339")]
    pub completed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub cached_at: OffsetDateTime,
    pub character_count: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl fmt::Debug for HistoryEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HistoryEntry(<redacted>)")
    }
}

pub fn parse_history(data: &[u8]) -> Result<Vec<HistoryEntry>, ProtocolError> {
    if data.len() > MAX_HISTORY_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    serde_json::from_slice(data).map_err(|_| ProtocolError::InvalidJson("history"))
}

pub fn live_entries(mut entries: Vec<HistoryEntry>, now: OffsetDateTime) -> Vec<HistoryEntry> {
    entries.retain(|entry| {
        !entry.transcript_id.is_empty()
            && now - entry.completed_at < time::Duration::seconds(RETENTION_SECONDS)
    });
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.completed_at));
    entries.truncate(MAX_ENTRIES);
    entries
}

pub fn add_entry(
    entries: Vec<HistoryEntry>,
    entry: HistoryEntry,
    now: OffsetDateTime,
) -> Vec<HistoryEntry> {
    let mut entries: Vec<_> = entries
        .into_iter()
        .filter(|old| old.transcript_id != entry.transcript_id)
        .collect();
    entries.push(entry);
    live_entries(entries, now)
}

pub fn encode_history(entries: &[HistoryEntry]) -> Result<Vec<u8>, ProtocolError> {
    let data = serde_json::to_vec(entries).map_err(|_| ProtocolError::InvalidJson("history"))?;
    if data.len() > MAX_HISTORY_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    Ok(data)
}

pub fn should_auto_copy(paused: bool, dictation_busy: bool, source: &str) -> bool {
    !paused && !dictation_busy && source != "windows_dictation"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, completed_at: OffsetDateTime) -> HistoryEntry {
        HistoryEntry {
            transcript_id: id.into(),
            recording_id: "fixture".into(),
            preview: "synthetic preview".into(),
            body: "synthetic body".into(),
            raw_body: Some("synthetic original".into()),
            refinement_fallback_blocks: 0,
            completed_at,
            cached_at: completed_at,
            character_count: 14,
        }
    }

    #[test]
    fn retention_boundary_capacity_order_and_replacement_match_the_current_cache() {
        let now = OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();
        let mut entries: Vec<_> = (0..220)
            .map(|index| entry(&index.to_string(), now - time::Duration::seconds(index)))
            .collect();
        entries.push(entry("expired", now - time::Duration::hours(48)));
        entries.push(entry("", now));
        let live = live_entries(entries, now);
        assert_eq!(live.len(), 200);
        assert_eq!(live[0].transcript_id, "0");
        assert!(!live.iter().any(|entry| entry.transcript_id == "expired"));
        let mut replacement = entry("0", now);
        replacement.body = "replacement".into();
        let updated = add_entry(live, replacement, now);
        assert_eq!(updated.len(), 200);
        assert_eq!(updated[0].body, "replacement");
    }

    #[test]
    fn history_roundtrips_csharp_fields_without_debugging_text() {
        let value = entry("fixture", OffsetDateTime::UNIX_EPOCH);
        let bytes = encode_history(&[value]).unwrap();
        let roundtrip = parse_history(&bytes).unwrap();
        assert_eq!(roundtrip[0].raw_body.as_deref(), Some("synthetic original"));
        assert!(!format!("{:?}", roundtrip[0]).contains("synthetic"));
        assert!(parse_history(b"corrupt").is_err());
    }

    #[test]
    fn audit_source_pause_and_dictation_never_autocopy() {
        assert!(should_auto_copy(false, false, "recording"));
        assert!(!should_auto_copy(true, false, "recording"));
        assert!(!should_auto_copy(false, true, "recording"));
        assert!(!should_auto_copy(false, false, "windows_dictation"));
    }
}

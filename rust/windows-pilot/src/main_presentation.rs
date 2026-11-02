use time::{Duration, OffsetDateTime, UtcOffset};
use voiceprompt_client_protocol::history::HistoryEntry;
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Globalization::{
    DATE_SHORTDATE, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS,
};
use windows::Win32::System::Time::SystemTimeToTzSpecificLocalTime;
use windows::core::PCWSTR;

use crate::NativeError;

fn local_date(value: OffsetDateTime) -> Result<String, NativeError> {
    let value = value.to_offset(UtcOffset::UTC);
    let year = u16::try_from(value.year()).map_err(|_| {
        NativeError::Verification("Displayed timestamp is outside Windows date range.")
    })?;
    let universal = SYSTEMTIME {
        wYear: year,
        wMonth: value.month() as u16,
        wDay: value.day() as u16,
        wDayOfWeek: value.weekday().number_days_from_sunday() as u16,
        wHour: value.hour() as u16,
        wMinute: value.minute() as u16,
        wSecond: value.second() as u16,
        wMilliseconds: value.millisecond(),
    };
    let mut local = SYSTEMTIME::default();
    unsafe { SystemTimeToTzSpecificLocalTime(None, &universal, &mut local) }
        .map_err(|error| NativeError::Api("Convert displayed timestamp to local time", error))?;
    let date = |buffer: Option<&mut [u16]>| unsafe {
        GetDateFormatEx(
            PCWSTR::null(),
            DATE_SHORTDATE,
            Some(&local),
            PCWSTR::null(),
            buffer,
            PCWSTR::null(),
        )
    };
    let time = |buffer: Option<&mut [u16]>| unsafe {
        GetTimeFormatEx(
            PCWSTR::null(),
            TIME_NOSECONDS,
            Some(&local),
            PCWSTR::null(),
            buffer,
        )
    };
    fn formatted(format: impl Fn(Option<&mut [u16]>) -> i32) -> Result<String, NativeError> {
        let length = format(None);
        if length == 0 {
            return Err(NativeError::Api(
                "Inspect user date/time format",
                windows::core::Error::from_thread(),
            ));
        }
        if !(1..=512).contains(&length) {
            return Err(NativeError::Verification(
                "User date/time format exceeds supported display bounds.",
            ));
        }
        let mut output = vec![0; length as usize];
        let count = format(Some(&mut output));
        if count == 0 {
            return Err(NativeError::Api(
                "Format displayed local date/time",
                windows::core::Error::from_thread(),
            ));
        }
        String::from_utf16(&output[..count as usize - 1])
            .map_err(|_| NativeError::Verification("Windows returned invalid date/time text."))
    }
    Ok(format!("{} {}", formatted(date)?, formatted(time)?))
}

fn remaining(completed: OffsetDateTime, now: OffsetDateTime) -> Result<String, NativeError> {
    let expires = completed
        .checked_add(Duration::hours(48))
        .ok_or(NativeError::Verification(
            "History expiry is outside the supported date range.",
        ))?;
    let left = expires - now;
    Ok(if left <= Duration::ZERO {
        "expired".into()
    } else if left >= Duration::hours(1) {
        format!("{} h left", left.whole_hours())
    } else {
        format!("{} min left", left.whole_minutes().max(1))
    })
}

pub(crate) fn history_metadata(
    entry: &HistoryEntry,
    now: OffsetDateTime,
) -> Result<String, NativeError> {
    let suffix = if entry.raw_body.is_none() {
        ""
    } else if entry.refinement_fallback_blocks > 0 {
        " \u{b7} AI errors; original available"
    } else {
        " \u{b7} Background AI; ending may be original"
    };
    Ok(format!(
        "{} \u{b7} {} chars \u{b7} {}{suffix}",
        local_date(entry.completed_at)?,
        entry.character_count,
        remaining(entry.completed_at, now)?
    ))
}

pub(crate) fn recovery_lines(
    entry: &crate::NativeRecoveryInfo,
    compact: bool,
) -> Result<Vec<String>, NativeError> {
    if let Some(error) = entry.error {
        return Ok(vec![format!("Recovery {}", entry.id), error.into()]);
    }
    let epoch = voiceprompt_client_protocol::recovery::utc_ticks(OffsetDateTime::UNIX_EPOCH)
        .map_err(|_| NativeError::Verification("Invalid recovery date epoch."))?;
    let updated = OffsetDateTime::from_unix_timestamp_nanos(
        (i128::from(entry.updated_at_ticks) - i128::from(epoch)) * 100,
    )
    .map_err(|_| NativeError::Verification("Recovery timestamp is outside supported bounds."))?;
    let date = local_date(updated)?;
    let audit = if entry.audit_pending { "True" } else { "False" };
    Ok(if compact {
        vec![
            format!("{date} \u{b7} Language: {}", entry.language),
            format!(
                "{} chunks pending \u{b7} Audit pending: {audit}",
                entry.pending_chunks
            ),
        ]
    } else {
        vec![
            date,
            format!(
                "{} sealed chunks pending (recovery also finishes the audio tail)",
                entry.pending_chunks
            ),
            format!("Language: {}", entry.language),
            format!("Cloud audit upload pending: {audit}"),
        ]
    })
}

pub(crate) fn recovery_status(status: &crate::client::ClientStatus, compact: bool) -> String {
    use crate::dictation::DictationStage;
    let view = &status.dictation;
    if view.recovering {
        return match view.stage {
            DictationStage::Starting | DictationStage::Finishing if compact => "Recovering; no paste.".into(),
            DictationStage::Saved if compact && !view.preview.is_empty() => "Recovered to History.".into(),
            DictationStage::Failed if compact => "Recovery failed; saved data kept.".into(),
            _ if compact => "Recovery stopped; saved data kept.".into(),
            DictationStage::Starting | DictationStage::Finishing => {
                "Recovering saved audio; nothing will be pasted. Esc stops recovery and keeps saved data.".into()
            }
            DictationStage::Saved if !view.preview.is_empty() => {
                "Recovery complete. Open History and Copy the recovered text.".into()
            }
            DictationStage::Failed => view.issue.unwrap_or(
                "Recovery incomplete; saved data is retained. Check sign-in, backend and disk access.",
            ).into(),
            _ => "Recovery stopped. Saved data is retained for another attempt.".into(),
        };
    }
    if status.recovery.is_empty() {
        "No unfinished dictations.".into()
    } else if compact {
        format!("{} saved sessions.", status.recovery.len())
    } else {
        format!(
            "{} saved dictations. Select a session to recover to History.",
            status.recovery.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_display_matches_original_hour_minute_and_expired_boundaries() {
        let completed = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(remaining(completed, completed).unwrap(), "48 h left");
        assert_eq!(
            remaining(completed, completed + Duration::hours(47)).unwrap(),
            "1 h left"
        );
        assert_eq!(
            remaining(
                completed,
                completed + Duration::hours(47) + Duration::seconds(1)
            )
            .unwrap(),
            "59 min left"
        );
        assert_eq!(
            remaining(
                completed,
                completed + Duration::hours(48) - Duration::seconds(1)
            )
            .unwrap(),
            "1 min left"
        );
        assert_eq!(
            remaining(completed, completed + Duration::hours(48)).unwrap(),
            "expired"
        );
        let metadata = history_metadata(
            &HistoryEntry {
                transcript_id: "synthetic".into(),
                recording_id: "synthetic".into(),
                preview: "Synthetic".into(),
                body: "Synthetic".into(),
                raw_body: Some("".into()),
                refinement_fallback_blocks: 1,
                completed_at: completed,
                cached_at: completed,
                character_count: 9,
            },
            completed,
        )
        .unwrap();
        assert!(
            metadata
                .ends_with(" \u{b7} 9 chars \u{b7} 48 h left \u{b7} AI errors; original available")
        );
    }

    #[test]
    fn recovery_details_keep_dates_language_pending_audit_and_corruption_visible() {
        let mut entry = crate::NativeRecoveryInfo {
            id: "0123456789abcdef0123456789abcdef".into(),
            created_at_ticks: 621355968000000000,
            updated_at_ticks: 621355968000000000,
            language: "cs".into(),
            captured_bytes: 32_000,
            pending_chunks: 2,
            capture_complete: true,
            audit_pending: true,
            error: None,
        };
        let full = recovery_lines(&entry, false).unwrap();
        let compact = recovery_lines(&entry, true).unwrap();
        assert_eq!(full.len(), 4);
        assert_eq!(compact.len(), 2);
        assert!(compact[0].starts_with(&full[0]));
        assert!(compact[0].contains("Language: cs"));
        assert!(full[1].starts_with("2 sealed chunks pending"));
        assert!(compact[1].contains("2 chunks pending"));
        assert_eq!(full[3], "Cloud audit upload pending: True");
        assert!(compact[1].ends_with("Audit pending: True"));
        entry.error = Some("Recovery data is unreadable or corrupt.");
        entry.updated_at_ticks = 0;
        let corrupt = recovery_lines(&entry, false).unwrap();
        assert_eq!(corrupt[1], entry.error.unwrap());
        assert!(!corrupt.iter().any(|line| line.contains("0 chunks")));
    }

    #[test]
    fn recovery_feedback_distinguishes_busy_complete_retained_and_unreadable() {
        use crate::dictation::DictationStage;
        let mut status = crate::client::ClientStatus::default();
        assert_eq!(recovery_status(&status, false), "No unfinished dictations.");
        status.dictation.recovering = true;
        status.dictation.stage = DictationStage::Finishing;
        assert!(recovery_status(&status, false).contains("nothing will be pasted"));
        assert_eq!(recovery_status(&status, true), "Recovering; no paste.");
        status.dictation.stage = DictationStage::Saved;
        assert!(recovery_status(&status, false).contains("Saved data is retained"));
        status.dictation.preview = "Synthetic recovered text".into();
        assert!(recovery_status(&status, false).starts_with("Recovery complete."));
        status.dictation.stage = DictationStage::Failed;
        status.dictation.issue = Some("Recovery data is unreadable or corrupt.");
        assert_eq!(
            recovery_status(&status, false),
            status.dictation.issue.unwrap()
        );
        assert_eq!(
            recovery_status(&status, true),
            "Recovery failed; saved data kept."
        );
    }
}

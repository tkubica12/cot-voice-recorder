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
        NativeError::Verification("History timestamp is outside Windows date range.")
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
        .map_err(|error| NativeError::Api("Convert cached transcript to local time", error))?;
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
                "Format cached transcript date/time",
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
}

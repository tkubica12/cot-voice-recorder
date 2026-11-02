use crate::problem::Problem;
use base64::Engine;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use voiceprompt_core::wav::{ParseMode, validate_capture_wav};

pub fn content_digest(header: Option<&str>, data: &[u8]) -> Result<String, Problem> {
    let header = header
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            Problem::new(
                400,
                "bad-request",
                "Bad request",
                "Content-Digest header is required.",
            )
        })?
        .trim();
    let encoded = header
        .strip_prefix("sha-256=:")
        .and_then(|value| value.strip_suffix(':'))
        .ok_or_else(|| {
            Problem::field(
                "Content-Digest must use sha-256=:base64: syntax.",
                "Content-Digest",
                "malformed sha-256 digest",
            )
        })?;
    let digest = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| {
            Problem::field(
                "Content-Digest base64 payload is invalid.",
                "Content-Digest",
                "invalid base64",
            )
        })?;
    let expected: [u8; 32] = Sha256::digest(data).into();
    if digest.len() != 32 {
        return Err(Problem::field(
            "Content-Digest must be a 32-byte SHA-256 value.",
            "Content-Digest",
            "expected 32-byte digest",
        ));
    }
    if !bool::from(digest.as_slice().ct_eq(&expected)) {
        return Err(Problem::field(
            "Request body does not match the Content-Digest.",
            "Content-Digest",
            "digest mismatch",
        ));
    }

    Ok(format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(digest)
    ))
}

pub fn wav_problem(error: voiceprompt_core::wav::WavError) -> Problem {
    match error {
        voiceprompt_core::wav::WavError::FormatMismatch(fields) => {
            let mut problem =
                Problem::validation("WAV PCM properties do not match the required capture format.");
            problem.errors = fields
                .into_iter()
                .map(|field| crate::problem::FieldError {
                    field: field.field.into(),
                    message: format!("expected {}, got {}", field.expected, field.actual),
                })
                .collect();
            problem
        }
        error => Problem::field(error.to_string(), "body", error.to_string()),
    }
}

pub fn streaming_samples(
    wav: &[u8],
    index: u32,
    contiguous: bool,
    start: Option<u64>,
    count: Option<u64>,
    overlap_ms: Option<u32>,
) -> Result<(u64, u64), Problem> {
    let props = validate_capture_wav(wav, ParseMode::Strict)
        .map_err(|_| Problem::validation("Invalid streaming PCM WAV."))?;
    let actual = u64::from(props.data_bytes / 2);
    if contiguous {
        if start != Some(u64::from(index) * 160_000)
            || count != Some(actual)
            || !(1..=160_000).contains(&actual)
            || overlap_ms.is_some_and(|value| value != 0)
        {
            return Err(Problem::validation(
                "Streaming segments require exact contiguous ten-second sample positions.",
            ));
        }
        Ok((u64::from(index) * 160_000, actual))
    } else {
        if actual == 0 || actual > 480_000 || index > 0 && actual <= 24_000 {
            return Err(Problem::validation(
                "Invalid legacy overlap streaming window.",
            ));
        }
        Ok((u64::from(index) * 456_000, actual))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn digest_and_contiguous_vs_legacy_audio_invariants_match_existing_clients() {
        let data = b"synthetic";
        let header = format!(
            "sha-256=:{}:",
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(data))
        );
        assert_eq!(content_digest(Some(&header), data).unwrap(), header);
        assert_eq!(content_digest(None, data).unwrap_err().status, 400);
        assert_eq!(
            content_digest(Some(&header), b"other").unwrap_err().status,
            422
        );
        let wav = voiceprompt_core::wav::to_wav(&vec![0; 320_000]).unwrap();
        assert_eq!(
            streaming_samples(&wav, 2, true, Some(320_000), Some(160_000), None).unwrap(),
            (320_000, 160_000)
        );
        assert!(streaming_samples(&wav, 2, true, Some(0), Some(160_000), None).is_err());
        let tail = voiceprompt_core::wav::to_wav(&vec![0; 48_000]).unwrap();
        assert!(streaming_samples(&tail, 1, false, None, None, None).is_err());
        assert_eq!(
            streaming_samples(&wav, 1, false, None, None, None)
                .unwrap()
                .0,
            456_000
        );
    }
}

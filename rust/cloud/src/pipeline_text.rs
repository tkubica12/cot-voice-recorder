use caseless::Caseless;
use sha1::{Digest, Sha1};
use unicode_normalization::UnicodeNormalization;

pub fn normalize_token(token: &str) -> String {
    token
        .nfkc()
        .default_case_fold()
        .filter(|value| crate::edits::word(*value))
        .collect()
}
pub fn stitch_chunks(texts: &[String], minimum_chars: usize, maximum_tokens: usize) -> String {
    let mut result: Vec<&str> = Vec::new();
    for text in texts {
        let tokens: Vec<_> = text
            .split(crate::edits::python_whitespace)
            .filter(|value| !value.is_empty())
            .collect();
        let limit = result.len().min(tokens.len()).min(maximum_tokens);
        let tail: Vec<_> = result
            .iter()
            .rev()
            .take(limit)
            .rev()
            .map(|token| normalize_token(token))
            .collect();
        let head: Vec<_> = tokens
            .iter()
            .take(limit)
            .map(|token| normalize_token(token))
            .collect();
        let mut skip = 0;
        for count in (1..=limit).rev() {
            let tail = &tail[limit - count..];
            if tail != &head[..count] || !tail.iter().any(|value| !value.is_empty()) {
                continue;
            }
            if count < 2
                && tail
                    .iter()
                    .map(|value| value.chars().count())
                    .sum::<usize>()
                    < minimum_chars
            {
                continue;
            }
            skip = count;
            break;
        }
        result.extend(tokens[skip..].iter().copied());
    }
    result.join(" ")
}
fn uuid5(namespace: [u8; 16], name: &str) -> [u8; 16] {
    let mut hash = Sha1::new();
    hash.update(namespace);
    hash.update(name.as_bytes());
    let mut id = [0; 16];
    id.copy_from_slice(&hash.finalize()[..16]);
    id[6] = (id[6] & 0x0f) | 0x50;
    id[8] = (id[8] & 0x3f) | 0x80;
    id
}
fn id(kind: &str, recording_id: &str) -> String {
    let namespace = uuid5(
        [
            0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4,
            0x30, 0xc8,
        ],
        kind,
    );
    let bytes = uuid5(namespace, recording_id);
    let hex: String = bytes.iter().map(|value| format!("{value:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
pub fn transcript_id(recording_id: &str) -> String {
    id("voice-recorder/transcript", recording_id)
}
pub fn event_id(recording_id: &str) -> String {
    id("voice-recorder/event", recording_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stitching_preserves_original_punctuation_and_only_removes_a_proven_normalized_suffix() {
        assert_eq!(
            stitch_chunks(&["First Straße.".into(), "STRASSE next".into()], 8, 60),
            "First Straße. STRASSE next"
        );
        assert_eq!(
            stitch_chunks(
                &["check the Straße".into(), "THE STRASSE next".into()],
                8,
                60
            ),
            "check the Straße next"
        );
        assert_eq!(normalize_token("İ"), "i");
        assert_eq!(normalize_token("ß"), "ss");
        assert_ne!(event_id("fixture"), transcript_id("fixture"));
        assert_eq!(event_id("fixture"), event_id("fixture"));
    }
}

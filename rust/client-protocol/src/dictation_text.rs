use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

pub fn normalize_word(word: &str) -> String {
    // The legacy matcher filters UTF-16 chars; supplementary surrogate halves are not letters.
    word.nfkc()
        .filter(|value| legacy_character(*value))
        .map(|value| {
            let mut lower = value.to_lowercase();
            let first = lower.next().unwrap_or(value);
            if lower.next().is_none() && legacy_character(first) {
                first
            } else {
                value
            }
        })
        .collect()
}

fn legacy_character(value: char) -> bool {
    u32::from(value) <= 0xffff
        && (value == '_'
            || matches!(
                get_general_category(value),
                GeneralCategory::UppercaseLetter
                    | GeneralCategory::LowercaseLetter
                    | GeneralCategory::TitlecaseLetter
                    | GeneralCategory::ModifierLetter
                    | GeneralCategory::OtherLetter
                    | GeneralCategory::DecimalNumber
            ))
}

fn overlap_count(previous: &[&str], incoming: &[&str], previous_count: usize) -> usize {
    let tail: Vec<_> = previous
        .iter()
        .rev()
        .take(12.min(previous_count))
        .rev()
        .map(|word| normalize_word(word))
        .collect();
    for count in (1..=tail.len().min(incoming.len())).rev() {
        let end = &tail[tail.len() - count..];
        if end
            .iter()
            .zip(&incoming[..count])
            .all(|(left, right)| *left == normalize_word(right))
            && end.iter().any(|word| !word.is_empty())
        {
            return count;
        }
    }
    0
}

pub fn stitch<'a>(chunks: impl IntoIterator<Item = (&'a str, bool)>) -> String {
    let mut words = Vec::new();
    let mut previous_count = 0;
    for (text, overlap) in chunks {
        let incoming: Vec<_> = text.split_whitespace().collect();
        let skip = if overlap {
            overlap_count(&words, &incoming, previous_count)
        } else {
            0
        };
        words.extend(incoming.iter().skip(skip).copied());
        previous_count = incoming.len();
    }
    words.join(" ")
}

pub fn append_preserving_prefix(
    previous: &str,
    incoming: &str,
    overlap: bool,
    previous_words: Option<usize>,
) -> String {
    if incoming.trim().is_empty() {
        return previous.into();
    }
    let words: Vec<_> = incoming.split_whitespace().collect();
    let prior: Vec<_> = previous.split_whitespace().collect();
    let skip = if overlap {
        overlap_count(&prior, &words, previous_words.unwrap_or(prior.len()))
    } else {
        0
    };
    let suffix = words[skip..].join(" ");
    if suffix.is_empty() {
        previous.into()
    } else if previous.is_empty() {
        suffix
    } else {
        format!("{previous} {suffix}")
    }
}

pub fn preview_tail(text: &str) -> String {
    let words: Vec<_> = text.split_whitespace().collect();
    let tail = words
        .iter()
        .rev()
        .take(30)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let count = tail.chars().count();
    let truncated = if count > 220 {
        tail.chars().skip(count - 220).collect()
    } else {
        tail
    };
    if words.len() > 30 || count > 220 {
        format!("... {truncated}")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_only_at_adjacent_boundaries_preserves_short_words_and_empty_segments() {
        assert_eq!(
            stitch([("go to", false), ("to the store", true)]),
            "go to the store"
        );
        assert_eq!(
            stitch([("that", false), ("that matters", true)]),
            "that matters"
        );
        assert_eq!(stitch([("again", false), ("again", false)]), "again again");
        assert_eq!(
            stitch([("again", false), ("", false), ("again", true)]),
            "again again"
        );
        assert_eq!(stitch([("!!!", false), ("!!! okay", true)]), "!!! !!! okay");
    }

    #[test]
    fn prefix_is_never_reformatted_and_preview_uses_runes_not_utf16_fragments() {
        assert_eq!(
            append_preserving_prefix("exact \r\n prefix", "prefix next", true, None),
            "exact \r\n prefix next"
        );
        assert_eq!(
            append_preserving_prefix("exact prefix", "  ", false, None),
            "exact prefix"
        );
        assert_eq!(
            preview_tail(&"a".repeat(221)),
            format!("... {}", "a".repeat(220))
        );
        assert_eq!(preview_tail(&"\u{1f600}".repeat(221)).chars().count(), 224);
    }
}

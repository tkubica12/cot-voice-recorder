use crate::models::{Edit, Refinement};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use unicode_general_category::{GeneralCategory, get_general_category};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidEdits(pub &'static str);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    edits: Vec<(String, String, String)>,
}

fn invalid(category: &'static str) -> InvalidEdits {
    InvalidEdits(category)
}
pub(crate) fn python_whitespace(value: char) -> bool {
    value.is_whitespace() || matches!(value, '\u{1c}'..='\u{1f}')
}
fn word(value: char) -> bool {
    value == '_'
        || matches!(
            get_general_category(value),
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
                | GeneralCategory::DecimalNumber
                | GeneralCategory::LetterNumber
                | GeneralCategory::OtherNumber
        )
}
fn linebreak(value: char) -> bool {
    matches!(
        value,
        '\r' | '\n' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

fn digit(value: char) -> bool {
    if get_general_category(value) == GeneralCategory::DecimalNumber {
        return true;
    }
    if get_general_category(value) != GeneralCategory::OtherNumber {
        return false;
    }
    unicode_names2::name(value).is_some_and(|name| {
        let name = name.to_string();
        name.contains("DIGIT") || name.starts_with("SUPERSCRIPT ") || name.starts_with("SUBSCRIPT ")
    })
}
fn boundaries(text: &str) -> Vec<(String, String)> {
    text.split(linebreak)
        .map(|line| {
            (
                line[..line.len() - line.trim_start_matches(python_whitespace).len()].into(),
                line[line.trim_end_matches(python_whitespace).len()..].into(),
            )
        })
        .collect()
}
fn numbers(text: &str) -> Result<Vec<String>, InvalidEdits> {
    static PATTERN: OnceLock<fancy_regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        fancy_regex::RegexBuilder::new(
            r"(?<![\p{L}\p{N}_])[+-]?\p{Nd}+(?:[.,:/-]\p{Nd}+)*(?:%|(?![\p{L}\p{N}_]))",
        )
        .backtrack_limit(100_000)
        .build()
        .expect("fixed numeric protection regex")
    });
    pattern
        .find_iter(text)
        .map(|value| {
            value
                .map(|value| value.as_str().to_owned())
                .map_err(|_| invalid("protected_numbers"))
        })
        .collect()
}
fn identifiers(text: &str) -> HashMap<String, usize> {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(r"[\p{L}\p{N}_]+(?:[._:/@\\+#-][\p{L}\p{N}_+#]+)*[+#]*")
            .expect("fixed identifier protection regex")
    });
    let mut counts = HashMap::new();
    for token in pattern.find_iter(text).map(|value| value.as_str()) {
        let characters: Vec<_> = token.chars().collect();
        if characters.iter().any(|value| digit(*value))
            || characters
                .windows(2)
                .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_uppercase())
            || characters.len() > 1
                && characters.iter().any(|value| value.is_uppercase())
                && characters.iter().all(|value| !value.is_lowercase())
            || token.contains(['_', ':', '/', '@', '\\', '+', '#'])
            || characters
                .windows(3)
                .any(|pair| word(pair[0]) && matches!(pair[1], '.' | '-') && word(pair[2]))
        {
            *counts.entry(token.into()).or_insert(0) += 1;
        }
    }
    counts
}
fn scripts(text: &str) -> HashSet<String> {
    let mut scripts = HashSet::new();
    for value in text.chars() {
        if matches!(
            get_general_category(value),
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
        ) {
            let name = unicode_names2::name(value)
                .map(|name| name.to_string())
                .unwrap_or_else(|| "UNKNOWN".into());
            let script = if name.contains("IDEOGRAPH") {
                "CJK"
            } else {
                name.split_whitespace().next().unwrap_or("UNKNOWN")
            };
            scripts.insert(script.into());
        }
    }
    scripts
}
fn windows(text: &str) -> HashMap<String, usize> {
    let characters: Vec<_> = text.chars().collect();
    let mut counts = HashMap::new();
    for window in characters.windows(60) {
        *counts.entry(window.iter().collect()).or_insert(0) += 1;
    }
    counts
}

pub fn validate(text: &str, payload: &str, previous: &str) -> Result<Refinement, InvalidEdits> {
    if payload.chars().count() > 32768 {
        return Err(invalid("response_size"));
    }
    let document: Document = serde_json::from_str(payload).map_err(|_| invalid("invalid_json"))?;
    if document.edits.len() > 64 {
        return Err(invalid("edit_count"));
    }
    let mut spans = Vec::new();
    for (block, anchor, replacement) in document.edits {
        if block != "current"
            || anchor.is_empty()
            || anchor.chars().count() > 256
            || replacement.chars().count() > 256
        {
            return Err(invalid("edit_bounds"));
        }
        let start = text
            .find(&anchor)
            .ok_or_else(|| invalid("anchor_missing"))?;
        if text.rfind(&anchor) != Some(start) {
            return Err(invalid("anchor_not_unique"));
        }
        if anchor != replacement {
            if anchor.replace(' ', "") == replacement.replace(' ', "")
                || anchor.chars().all(python_whitespace)
            {
                return Err(invalid("whitespace_only"));
            }
            if anchor.chars().any(linebreak) || replacement.chars().any(linebreak) {
                return Err(invalid("line_structure"));
            }
            let nonspace = |value: &str| {
                value
                    .chars()
                    .filter(|value| python_whitespace(*value) && *value != ' ')
                    .collect::<String>()
            };
            if nonspace(&anchor) != nonspace(&replacement) {
                return Err(invalid("whitespace_structure"));
            }
        }
        spans.push((start, start + anchor.len(), anchor, replacement));
    }
    spans.sort_by_key(|span| span.0);
    if spans.windows(2).any(|spans| spans[0].1 > spans[1].0) {
        return Err(invalid("overlapping_edits"));
    }
    let mut result = String::new();
    let mut offset = 0;
    for (start, end, _, replacement) in &spans {
        result.push_str(&text[offset..*start]);
        result.push_str(replacement);
        offset = *end;
    }
    result.push_str(&text[offset..]);
    if result.chars().count() > 4000
        || result.trim().is_empty()
        || text.chars().any(char::is_alphanumeric) && !result.chars().any(char::is_alphanumeric)
    {
        return Err(invalid("result_bounds"));
    }
    if numbers(text)? != numbers(&result)? {
        return Err(invalid("protected_numbers"));
    }
    if identifiers(text) != identifiers(&result) {
        return Err(invalid("protected_identifiers"));
    }
    if !scripts(&result).is_subset(&scripts(text)) {
        return Err(invalid("new_script"));
    }
    if boundaries(text) != boundaries(&result) {
        return Err(invalid("boundary_whitespace"));
    }
    if result.chars().any(|value| {
        matches!(
            get_general_category(value),
            GeneralCategory::Control | GeneralCategory::Format | GeneralCategory::Surrogate
        ) && !text.contains(value)
    }) {
        return Err(invalid("control_characters"));
    }
    let original = windows(text);
    let context = windows(previous);
    for (window, count) in windows(&result) {
        if count > 1 && count > original.get(&window).copied().unwrap_or(0) {
            return Err(invalid("new_repetition"));
        }
        if context.contains_key(&window) && !original.contains_key(&window) {
            return Err(invalid("context_echo"));
        }
    }
    let edits = if result == text {
        Vec::new()
    } else {
        spans
            .into_iter()
            .filter(|(_, _, anchor, replacement)| anchor != replacement)
            .map(|(_, _, original, replacement)| Edit {
                original,
                replacement,
            })
            .collect()
    };
    Ok(Refinement {
        text: result,
        edits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provenance_and_protected_literals_are_atomic() {
        let accepted = validate(
            "This is is a test.",
            r#"{"edits":[["current"," is is"," is"]]}"#,
            "",
        )
        .unwrap();
        assert_eq!(accepted.text, "This is a test.");
        assert_eq!(
            validate("port 8443", r#"{"edits":[["current","8443","9443"]]}"#, "")
                .err()
                .unwrap()
                .0,
            "protected_numbers"
        );
        assert_eq!(
            validate("SDK case", r#"{"edits":[["current","SDK","api"]]}"#, "")
                .err()
                .unwrap()
                .0,
            "protected_identifiers"
        );
        assert_eq!(
            validate(
                "repeat repeat",
                r#"{"edits":[["current","repeat",""]]}"#,
                ""
            )
            .err()
            .unwrap()
            .0,
            "anchor_not_unique"
        );
        assert!(
            validate(
                "hello world",
                r#"{"edits":[["current","world","世界"]]}"#,
                ""
            )
            .is_err()
        );
    }
}

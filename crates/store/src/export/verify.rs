//! The verify pass (#28 D5.4): scan the finished text for every high-entropy identifier the
//! export knows, and for any 17–20 digit run left inside a JSON string. A hit fails the export
//! closed, naming the section, field and kind, never the value.

use serde_json::Value;

use super::ExportError;
use super::render::{Block, Section};
use super::scrub::digit_runs;

/// Checks each rendered section against `targets` (`(value, kind)`).
pub(crate) fn verify(
    sections: &[Section],
    rendered: &[String],
    targets: &[(String, &'static str)],
) -> Result<(), ExportError> {
    // ASCII case-insensitive, like the free-text pass: a Session ID in upper case is still it.
    let targets: Vec<(String, &'static str)> = targets
        .iter()
        .filter(|(v, _)| !v.is_empty())
        .map(|(v, k)| (v.to_ascii_lowercase(), *k))
        .collect();
    for (section, text) in sections.iter().zip(rendered) {
        let lower = text.to_ascii_lowercase();
        for (value, kind) in &targets {
            // JSON escaping hides a value with `\` or `"` from the text scan; the structured
            // walk still sees it.
            let escapable = value.contains(['\\', '"']) || value.chars().any(char::is_control);
            if contains_bounded(&lower, value) || (escapable && locate(section, value).is_some()) {
                return Err(ExportError::Leak {
                    section: section.kind,
                    field: locate(section, value).unwrap_or_else(|| "text".into()),
                    kind,
                });
            }
        }
        for block in &section.blocks {
            let hit = match block {
                Block::Prose(text) => has_digit_run(text).then(|| "text".to_owned()),
                Block::Json(value) => digit_run_field(value, "text"),
                Block::Jsonl(values) => values.iter().find_map(|v| digit_run_field(v, "text")),
            };
            if let Some(field) = hit {
                return Err(ExportError::Leak {
                    section: section.kind,
                    field,
                    kind: "discord",
                });
            }
        }
    }
    Ok(())
}

/// `value` occurs in `text` without a `[A-Za-z0-9_-]` character on either side, so a short
/// identifier inside a longer word doesn't count.
fn contains_bounded(text: &str, value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let word =
        |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let first_word = word(value.chars().next());
    let last_word = word(value.chars().next_back());
    text.match_indices(value).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + value.len()..].chars().next();
        !(first_word && word(before) || last_word && word(after))
    })
}

fn has_digit_run(text: &str) -> bool {
    digit_runs(text, 0, None, None).next().is_some()
}

/// The field holding `value`, for the error.
fn locate(section: &Section, value: &str) -> Option<String> {
    section.blocks.iter().find_map(|block| match block {
        Block::Prose(_) => None,
        Block::Json(v) => field_with(v, "text", value),
        Block::Jsonl(vs) => vs.iter().find_map(|v| field_with(v, "text", value)),
    })
}

fn field_with(v: &Value, name: &str, needle: &str) -> Option<String> {
    match v {
        Value::String(s) => {
            contains_bounded(&s.to_ascii_lowercase(), needle).then(|| name.to_owned())
        }
        Value::Number(n) => (n.to_string() == needle).then(|| name.to_owned()),
        Value::Array(items) => items.iter().find_map(|x| field_with(x, name, needle)),
        Value::Object(map) => map.iter().find_map(|(k, x)| {
            if contains_bounded(&k.to_ascii_lowercase(), needle) {
                Some(k.clone())
            } else {
                field_with(x, k, needle)
            }
        }),
        _ => None,
    }
}

/// The field whose string (or key) holds a 17–20 digit run; JSON numbers are never scanned.
fn digit_run_field(v: &Value, name: &str) -> Option<String> {
    match v {
        Value::String(s) => has_digit_run(s).then(|| name.to_owned()),
        Value::Array(items) => items.iter().find_map(|x| digit_run_field(x, name)),
        Value::Object(map) => map.iter().find_map(|(k, x)| {
            if has_digit_run(k) {
                Some(k.clone())
            } else {
                digit_run_field(x, k)
            }
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::export::ExportSection;
    use serde_json::json;

    use super::*;

    #[test]
    fn bounded_matching() {
        assert!(contains_bounded("user owner here", "owner"));
        assert!(!contains_bounded("co-owner", "owner"));
        assert!(!contains_bounded("owners", "owner"));
        assert!(contains_bounded("at /x/y.", "/x/y"));
        assert!(contains_bounded("<redacted:x>tok", "<redacted:x>"));
    }

    #[test]
    fn a_planted_value_names_its_section_and_field() {
        let sections = vec![
            Section::new(ExportSection::Summary).prose("fine"),
            Section::new(ExportSection::DaemonLog)
                .jsonl(vec![json!({"seq": 1, "fields": {"boot_id": "dev-abc"}})]),
        ];
        let rendered: Vec<String> = sections.iter().map(Section::render).collect();
        let err = verify(&sections, &rendered, &[("dev-abc".into(), "device")]).unwrap_err();
        let ExportError::Leak {
            section,
            field,
            kind,
        } = err
        else {
            panic!("expected a leak")
        };
        assert_eq!(
            (section, field.as_str(), kind),
            (ExportSection::DaemonLog, "boot_id", "device")
        );
    }

    #[test]
    fn a_digit_run_in_a_string_fails_but_a_number_does_not() {
        let run: String = std::iter::repeat_n('8', 18).collect();
        let number: Value = serde_json::from_str(&run).unwrap();
        let ok = vec![Section::new(ExportSection::Runs).json(json!({"n": number}))];
        let rendered: Vec<String> = ok.iter().map(Section::render).collect();
        // The rendered number is in the text, but no target names it and it isn't a string.
        assert!(verify(&ok, &rendered, &[]).is_ok());

        let bad = vec![Section::new(ExportSection::Runs).json(json!({"err": format!("id {run}")}))];
        let rendered: Vec<String> = bad.iter().map(Section::render).collect();
        let err = verify(&bad, &rendered, &[]).unwrap_err();
        assert!(matches!(err, ExportError::Leak { field, kind: "discord", .. } if field == "err"));
    }
}

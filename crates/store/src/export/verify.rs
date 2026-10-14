//! The verify pass (#28 D5.4): scan the finished text for every high-entropy identifier the
//! export knows, and for any 17–20 digit run left inside a JSON string. A hit fails the export
//! closed, naming the section, field and kind, never the value.
//!
//! High-entropy values (Session, Device and token IDs, snowflakes, IPs, paths, secrets, the
//! project and home dirs) match anywhere, as in the scrub; only word-like names (the OS user,
//! the hostname, labels, token names) need word boundaries. Job, agent, Launcher and MCP names
//! are checked only in data keys (settings keys, unclassified Daemon log keys), where no code
//! word can explain them.

use claudeclaw_api::export::{ExportSection, FieldClass, field_class};
use serde_json::{Map, Value};

use super::ExportError;
use super::render::{Block, Section};
use super::scrub::{
    Boundary, Target, char_after, char_before, classified, digit_runs, snowflake_number,
};

/// Checks each rendered section against `targets` (from `Known::verify_targets`) and its data
/// keys against `key_targets` (`Known::key_targets`). Targets are lowercased; matching is ASCII
/// case-insensitive, like the free-text pass.
pub(crate) fn verify(
    sections: &[Section],
    rendered: &[String],
    targets: &[Target],
    key_targets: &[Target],
) -> Result<(), ExportError> {
    for (section, text) in sections.iter().zip(rendered) {
        let lower = text.to_ascii_lowercase();
        for (value, kind, boundary) in targets {
            // JSON escaping hides a value with `\` or `"` from the text scan; the structured
            // walk still sees it.
            let escapable = value.contains(['\\', '"']) || value.chars().any(char::is_control);
            let in_text = contains(&lower, value, *boundary);
            if in_text || (escapable && locate(section, value, *boundary).is_some()) {
                return Err(ExportError::Leak {
                    section: section.kind,
                    field: locate(section, value, *boundary).unwrap_or_else(|| "text".into()),
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
        if let Some(rule) = key_rule(section.kind) {
            for block in &section.blocks {
                let values: &[Value] = match block {
                    Block::Prose(_) => &[],
                    Block::Json(v) => std::slice::from_ref(v),
                    Block::Jsonl(vs) => vs,
                };
                for v in values {
                    if let Some((field, kind)) = key_hit(v, "settings", rule, key_targets) {
                        return Err(ExportError::Leak {
                            section: section.kind,
                            field,
                            kind,
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Which keys of a section are data: every settings key, and the Daemon log's keys that have
/// no Field class (the line's own `span` and `fields` are code-given).
#[derive(Clone, Copy)]
enum KeyRule {
    All,
    Unclassified,
}

fn key_rule(section: ExportSection) -> Option<KeyRule> {
    match section {
        ExportSection::Settings => Some(KeyRule::All),
        ExportSection::DaemonLog => Some(KeyRule::Unclassified),
        _ => None,
    }
}

/// The first data key holding a short name: `(the field it sits under, kind)`.
fn key_hit(
    v: &Value,
    parent: &str,
    rule: KeyRule,
    targets: &[Target],
) -> Option<(String, &'static str)> {
    match v {
        Value::Array(items) => items.iter().find_map(|x| key_hit(x, parent, rule, targets)),
        Value::Object(map) => key_hit_map(map, parent, rule, targets),
        _ => None,
    }
}

fn key_hit_map(
    map: &Map<String, Value>,
    parent: &str,
    rule: KeyRule,
    targets: &[Target],
) -> Option<(String, &'static str)> {
    map.iter().find_map(|(k, x)| {
        let data = match rule {
            KeyRule::All => true,
            KeyRule::Unclassified => !classified(k) && k != "span" && k != "fields",
        };
        let lower = k.to_ascii_lowercase();
        let hit = data
            .then(|| targets.iter().find(|(t, _, b)| contains(&lower, t, *b)))
            .flatten();
        match hit {
            Some((_, kind, _)) => Some((parent.to_owned(), *kind)),
            None => key_hit(x, k, rule, targets),
        }
    })
}

/// `value` occurs in `text` as `boundary` allows. Both are lowercased.
fn contains(text: &str, value: &str, boundary: Boundary) -> bool {
    if value.is_empty() {
        return false;
    }
    text.match_indices(value).any(|(i, _)| {
        boundary.holds(
            char_before(text, i, None),
            char_after(text, i + value.len(), None),
        )
    })
}

fn has_digit_run(text: &str) -> bool {
    digit_runs(text, 0, None, None).next().is_some()
}

/// The field holding `value`, for the error.
fn locate(section: &Section, value: &str, boundary: Boundary) -> Option<String> {
    section.blocks.iter().find_map(|block| match block {
        Block::Prose(_) => None,
        Block::Json(v) => field_with(v, "text", value, boundary),
        Block::Jsonl(vs) => vs
            .iter()
            .find_map(|v| field_with(v, "text", value, boundary)),
    })
}

fn field_with(v: &Value, name: &str, needle: &str, boundary: Boundary) -> Option<String> {
    match v {
        Value::String(s) => {
            contains(&s.to_ascii_lowercase(), needle, boundary).then(|| name.to_owned())
        }
        Value::Number(n) => (n.to_string() == needle).then(|| name.to_owned()),
        Value::Array(items) => items
            .iter()
            .find_map(|x| field_with(x, name, needle, boundary)),
        Value::Object(map) => map.iter().find_map(|(k, x)| {
            if contains(&k.to_ascii_lowercase(), needle, boundary) {
                Some(name.to_owned())
            } else {
                field_with(x, k, needle, boundary)
            }
        }),
        _ => None,
    }
}

/// The field whose string (or key) holds a 17–20 digit run, or whose JSON integer has 17–20
/// digits outside a Structural field (a snowflake stored as a number). Floats and Structural
/// numbers are not scanned (#28 Evidence: `0.30000000000000004`).
fn digit_run_field(v: &Value, name: &str) -> Option<String> {
    match v {
        Value::String(s) => has_digit_run(s).then(|| name.to_owned()),
        Value::Number(n) => (field_class(name) != FieldClass::Structural
            && snowflake_number(n).is_some())
        .then(|| name.to_owned()),
        Value::Array(items) => items.iter().find_map(|x| digit_run_field(x, name)),
        Value::Object(map) => map.iter().find_map(|(k, x)| {
            if has_digit_run(k) {
                Some(name.to_owned())
            } else {
                digit_run_field(x, k)
            }
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn word(v: &str, kind: &'static str) -> Target {
        (v.into(), kind, Boundary::Word)
    }

    fn anywhere(v: &str, kind: &'static str) -> Target {
        (v.into(), kind, Boundary::Anywhere)
    }

    fn check(sections: &[Section], targets: &[Target], keys: &[Target]) -> Result<(), ExportError> {
        let rendered: Vec<String> = sections.iter().map(Section::render).collect();
        verify(sections, &rendered, targets, keys)
    }

    fn leak(r: Result<(), ExportError>) -> (ExportSection, String, &'static str) {
        match r {
            Err(ExportError::Leak {
                section,
                field,
                kind,
            }) => (section, field, kind),
            other => panic!("expected a leak, got {other:?}"),
        }
    }

    #[test]
    fn word_boundaries_only_for_word_like_kinds() {
        assert!(contains("user owner here", "owner", Boundary::Word));
        assert!(!contains("co-owner", "owner", Boundary::Word));
        assert!(!contains("owners", "owner", Boundary::Word));
        assert!(contains(
            "resume_5e55-ab12 failed",
            "5e55-ab12",
            Boundary::Anywhere
        ));
        assert!(contains("chan_12345_x", "12345", Boundary::Digit));
        assert!(!contains("912345", "12345", Boundary::Digit));
    }

    #[test]
    fn a_planted_value_names_its_section_and_field() {
        let sections = vec![
            Section::new(ExportSection::Summary).prose("fine"),
            Section::new(ExportSection::DaemonLog)
                .jsonl(vec![json!({"seq": 1, "fields": {"boot_id": "dev-abc"}})]),
        ];
        assert_eq!(
            leak(check(&sections, &[anywhere("dev-abc", "device")], &[])),
            (ExportSection::DaemonLog, "boot_id".into(), "device")
        );
    }

    #[test]
    fn a_session_glued_to_a_word_is_still_caught() {
        let sections = vec![
            Section::new(ExportSection::DaemonLog)
                .jsonl(vec![json!({"message": "resume_5E55-AB12 failed"})]),
        ];
        assert_eq!(
            leak(check(&sections, &[anywhere("5e55-ab12", "session")], &[])),
            (ExportSection::DaemonLog, "message".into(), "session")
        );
    }

    #[test]
    fn a_digit_run_before_a_full_stop_fails_but_a_decimal_does_not() {
        let run: String = std::iter::repeat_n('8', 18).collect();
        let bad = vec![
            Section::new(ExportSection::DaemonLog)
                .jsonl(vec![json!({"message": format!("Unknown user {run}.")})]),
        ];
        assert_eq!(
            leak(check(&bad, &[], &[])),
            (ExportSection::DaemonLog, "message".into(), "discord")
        );
        let ok = vec![
            Section::new(ExportSection::DaemonLog)
                .jsonl(vec![json!({"message": format!("cost 0.{run}")})]),
        ];
        assert!(check(&ok, &[], &[]).is_ok());
    }

    #[test]
    fn a_digit_run_in_a_string_fails_and_a_number_only_outside_structural_fields() {
        let run: String = std::iter::repeat_n('8', 18).collect();
        let number: Value = serde_json::from_str(&run).unwrap();
        let ok = vec![
            Section::new(ExportSection::Runs).json(json!({"duration_ms": number, "cost_usd": 0.5})),
        ];
        assert!(check(&ok, &[], &[]).is_ok(), "a Structural number is kept");
        let bad = vec![
            Section::new(ExportSection::DaemonLog)
                .jsonl(vec![json!({"fields": {"thread_id": number}})]),
        ];
        assert_eq!(
            leak(check(&bad, &[], &[])),
            (ExportSection::DaemonLog, "thread_id".into(), "discord")
        );

        let bad = vec![Section::new(ExportSection::Runs).json(json!({"err": format!("id {run}")}))];
        assert_eq!(
            leak(check(&bad, &[], &[])),
            (ExportSection::Runs, "err".into(), "discord")
        );
    }

    #[test]
    fn a_surviving_fragment_of_a_home_path_fails() {
        let sections = vec![Section::new(ExportSection::DaemonLog).jsonl(vec![
            json!({"message": "open path:#1 Documents/AcmeCorp/plan.md: denied"}),
        ])];
        assert_eq!(
            leak(check(&sections, &[word("documents/acmecorp", "path")], &[])),
            (ExportSection::DaemonLog, "message".into(), "path")
        );
    }

    #[test]
    fn short_names_are_checked_in_data_keys_only() {
        let keys = [word("payroll-sync", "job")];
        let log = |v: Value| vec![Section::new(ExportSection::DaemonLog).jsonl(vec![v])];
        assert_eq!(
            leak(check(
                &log(json!({"fields": {"per_job": {"payroll-sync": 2}}})),
                &[],
                &keys
            )),
            (ExportSection::DaemonLog, "per_job".into(), "job")
        );
        let settings = vec![
            Section::new(ExportSection::Settings).json(json!({"jobs": {"payroll-sync": true}})),
        ];
        assert_eq!(
            leak(check(&settings, &[], &keys)),
            (ExportSection::Settings, "jobs".into(), "job")
        );
        // A classified key is code-given, and values aren't key-checked.
        let job_keys = [word("job", "job")];
        assert!(check(&log(json!({"fields": {"job": "job:#1"}})), &[], &job_keys).is_ok());
        let status = vec![Section::new(ExportSection::Status).json(json!({"payroll-sync": 1}))];
        assert!(check(&status, &[], &keys).is_ok());
    }
}

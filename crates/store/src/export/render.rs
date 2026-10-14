//! The export's Markdown (#28 D6): `##` sections, data in fenced `json` and `jsonl` blocks.

use claudeclaw_api::export::ExportSection;
use serde_json::Value;

/// One piece of a section.
pub(crate) enum Block {
    /// Code-given prose, with scrubbed values already in it.
    Prose(String),
    /// One object, in a `json` fence.
    Json(Value),
    /// One value per line, in a `jsonl` fence.
    Jsonl(Vec<Value>),
}

/// A section of the file, in D1's order.
pub(crate) struct Section {
    pub kind: ExportSection,
    pub blocks: Vec<Block>,
}

impl Section {
    pub(crate) fn new(kind: ExportSection) -> Self {
        Section {
            kind,
            blocks: Vec::new(),
        }
    }

    pub(crate) fn prose(mut self, text: impl Into<String>) -> Self {
        self.blocks.push(Block::Prose(text.into()));
        self
    }

    pub(crate) fn json(mut self, value: Value) -> Self {
        self.blocks.push(Block::Json(value));
        self
    }

    pub(crate) fn jsonl(mut self, values: Vec<Value>) -> Self {
        self.blocks.push(Block::Jsonl(values));
        self
    }

    /// The section as Markdown.
    pub(crate) fn render(&self) -> String {
        let mut out = match title(self.kind) {
            Some(title) => format!("## {title}\n\n"),
            None => String::new(),
        };
        for block in &self.blocks {
            match block {
                Block::Prose(text) => out.push_str(text),
                Block::Json(value) => out.push_str(&fence(
                    "json",
                    &serde_json::to_string_pretty(value).expect("JSON values serialise"),
                )),
                Block::Jsonl(values) => {
                    let body: Vec<String> = values.iter().map(Value::to_string).collect();
                    out.push_str(&fence("jsonl", &body.join("\n")));
                }
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }
}

fn title(kind: ExportSection) -> Option<&'static str> {
    match kind {
        ExportSection::Header => None,
        ExportSection::Summary => Some("Summary"),
        ExportSection::Status => Some("Status"),
        ExportSection::Settings => Some("Settings shape"),
        ExportSection::Jobs => Some("Jobs"),
        ExportSection::Crash => Some("Crash record"),
        ExportSection::Runs => Some("Runs"),
        ExportSection::DaemonLog => Some("Daemon log"),
        ExportSection::Unknown => Some("Other"),
    }
}

/// A fenced block one backtick longer than the longest backtick run in `body`, and at least
/// three, so no line can close it early (#28 D6).
pub(crate) fn fence(info: &str, body: &str) -> String {
    let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat((longest + 1).max(3));
    if body.is_empty() {
        format!("{ticks}{info}\n{ticks}\n")
    } else {
        format!("{ticks}{info}\n{body}\n{ticks}\n")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_fence_outgrows_every_backtick_run_in_its_body() {
        assert_eq!(fence("json", "{}"), "```json\n{}\n```\n");
        assert_eq!(
            fence("jsonl", "{\"m\":\"a ``` b\"}"),
            "````jsonl\n{\"m\":\"a ``` b\"}\n````\n"
        );
        assert_eq!(
            fence("jsonl", "{\"m\":\"``````\"}"),
            "```````jsonl\n{\"m\":\"``````\"}\n```````\n"
        );
        assert_eq!(fence("jsonl", ""), "```jsonl\n```\n");
    }

    #[test]
    fn sections_render_as_headed_blocks() {
        let section = Section::new(ExportSection::Runs)
            .prose("2 runs.")
            .jsonl(vec![json!({"a": 1}), json!({"b": 2})]);
        assert_eq!(
            section.render(),
            "## Runs\n\n2 runs.\n\n```jsonl\n{\"a\":1}\n{\"b\":2}\n```\n\n"
        );
    }
}

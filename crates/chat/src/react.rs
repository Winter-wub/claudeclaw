//! The `[react:x]` reply directive (architecture §4 `claudeclaw-chat`, §6; TS parity, #18).
//!
//! A reply may carry `[react:<emoji>]` anywhere in its text. The adapter removes every
//! directive from the text it sends and reacts to the user's message with the first one that
//! isn't blank (`skills/telegram-react/SKILL.md`).
//!
//! The grammar is the TS regex `/\[react:([^\]\r\n]+)\]/gi` (`src/commands/discord.ts:383`,
//! the same in `telegram.ts:590` and `slack.ts:375`): `[`, `react:` in any ASCII case, one or
//! more characters other than `]`, CR and LF, then `]`. A `[react:` that hits a line break or
//! the end of the text before its `]` is not a directive and stays in the text.

use std::fmt;

use crate::js;

/// A reply with its `[react:x]` directives taken out, from [`extract_react_directive`].
#[derive(Clone, PartialEq, Eq)]
pub struct ReactDirective {
    /// The reply text to send: directives removed, spaces and tabs before a line break
    /// dropped, three or more line breaks folded into two, then trimmed.
    pub text: String,
    /// The first directive's value, trimmed; `None` when the reply has no directive whose
    /// value is non-blank.
    pub emoji: Option<String>,
}

impl fmt::Debug for ReactDirective {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReactDirective")
            .field("text_bytes", &self.text.len())
            .field("emoji_bytes", &self.emoji.as_ref().map(String::len))
            .finish()
    }
}

/// Takes every `[react:x]` directive out of `text` and returns the cleaned text plus the
/// first non-blank directive value (TS `extractReactionDirective`, `src/commands/discord.ts:380`).
pub fn extract_react_directive(text: &str) -> ReactDirective {
    let mut emoji = None;
    let stripped = replace_directives(text, |value| {
        let candidate = js::trim(value);
        if emoji.is_none() && !candidate.is_empty() {
            emoji = Some(candidate.to_owned());
        }
    });
    let text = js::trim(&fold_blank_lines(&drop_trailing_blanks(&stripped))).to_owned();
    ReactDirective { text, emoji }
}

/// Removes every `[react:x]` directive from `text` and changes nothing else (the TS
/// `text.replace(/\[react:[^\]\r\n]+\]/gi, "")` in front of every send,
/// `src/commands/telegram.ts:401`).
pub fn strip_react_directives(text: &str) -> String {
    replace_directives(text, |_| {})
}

/// Copies `text` without its directives, calling `on_value` with each directive's raw value in
/// order.
fn replace_directives(text: &str, mut on_value: impl FnMut(&str)) -> String {
    const OPEN: &str = "[react:";
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut from = 0;
    while let Some(offset) = text[from..].find('[') {
        let start = from + offset;
        let after_open = start + OPEN.len();
        let opens = text
            .get(start..after_open)
            .is_some_and(|head| head.eq_ignore_ascii_case(OPEN));
        if opens {
            let rest = &text[after_open..];
            let value_len = rest.find([']', '\r', '\n']).unwrap_or(rest.len());
            if value_len > 0 && rest[value_len..].starts_with(']') {
                on_value(&rest[..value_len]);
                out.push_str(&text[copied..start]);
                copied = after_open + value_len + 1;
                from = copied;
                continue;
            }
        }
        from = start + 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// TS `.replace(/[ \t]+\n/g, "\n")`.
fn drop_trailing_blanks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut line_start = 0;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            out.push_str(text[line_start..i].trim_end_matches([' ', '\t']));
            out.push('\n');
            line_start = i + 1;
        }
    }
    out.push_str(&text[line_start..]);
    out
}

/// TS `.replace(/\n{3,}/g, "\n\n")`.
fn fold_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    for c in text.chars() {
        if c == '\n' {
            run += 1;
            if run > 2 {
                continue;
            }
        } else {
            run = 0;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values come from evaluating the TS `extractReactionDirective`
    /// (`src/commands/discord.ts:380-392`) on each input.
    #[test]
    fn extract_matches_ts() {
        let cases: &[(&str, &str, Option<&str>)] = &[
            // `src/__tests__/slack.test.ts:67`.
            (
                "Nice! [react:thumbsup] done",
                "Nice!  done",
                Some("thumbsup"),
            ),
            ("plain text", "plain text", None),
            // The example in `skills/telegram-react/SKILL.md`.
            ("Nice work [react:🔥]", "Nice work", Some("🔥")),
            // Several directives: all removed, the first wins.
            ("[react:👍]a[react:🎉]b[react:x]", "ab", Some("👍")),
            // A blank first directive is removed but doesn't win.
            ("[react:  ]hi [react: 🎉 ]", "hi", Some("🎉")),
            ("[react: ]only blank", "only blank", None),
            // `react:` is matched in any ASCII case.
            ("[REACT:ok] [React:no]", "", Some("ok")),
            // Malformed: empty value, no `]` before a line break, no `]` at all.
            ("[react:] x", "[react:] x", None),
            ("[react:ok\n] x", "[react:ok\n] x", None),
            ("[react:ok\r\n] x", "[react:ok\r\n] x", None),
            ("tail [react:ok", "tail [react:ok", None),
            ("[ react:x] [react :x]", "[ react:x] [react :x]", None),
            // The value runs to the first `]` and may hold `[`.
            ("a[react:[react:x]]b", "a]b", Some("[react:x")),
            // A failed `[react:` doesn't hide a later directive.
            ("[react:\n[react:🔥]", "[react:", Some("🔥")),
            // Spaces and tabs before a line break go; three or more breaks fold into two.
            ("line [react:x] \t\nnext", "line\nnext", Some("x")),
            ("a\n\n\n\nb [react:x]\n\n\n", "a\n\nb", Some("x")),
            ("a \r\nb", "a \r\nb", None),
            // `trim` is JavaScript's: U+FEFF goes, U+0085 stays.
            ("\u{FEFF}[react:x]hi\u{3000}", "hi", Some("x")),
            ("\u{0085}hi\u{0085}", "\u{0085}hi\u{0085}", None),
            ("[react:\u{FEFF}👍\u{00A0}]", "", Some("👍")),
        ];
        for &(input, text, emoji) in cases {
            let got = extract_react_directive(input);
            assert_eq!(got.text, text, "text of {input:?}");
            assert_eq!(got.emoji.as_deref(), emoji, "emoji of {input:?}");
        }
    }

    /// Expected values come from the TS `.replace(/\[react:[^\]\r\n]+\]/gi, "")`
    /// (`src/commands/telegram.ts:401`), which neither trims nor folds.
    #[test]
    fn strip_matches_ts() {
        let cases: &[(&str, &str)] = &[
            ("  a [react:x] b  ", "  a  b  "),
            ("[react:x]\n\n\n[react:y]", "\n\n\n"),
            ("[react:]", "[react:]"),
            ("ไทย[react:🙏]ไทย", "ไทยไทย"),
            ("[[react:x]]", "[]"),
        ];
        for &(input, want) in cases {
            assert_eq!(strip_react_directives(input), want, "input {input:?}");
        }
    }

    #[test]
    fn debug_is_content_free() {
        let got = format!("{:?}", extract_react_directive("secret reply [react:🔥]"));
        assert!(!got.contains("secret"), "{got}");
        assert!(!got.contains('🔥'), "{got}");
    }
}

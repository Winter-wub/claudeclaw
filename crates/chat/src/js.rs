//! The two pieces of JavaScript string semantics the TS helpers rely on (TS parity, #18).

/// Whether `c` is whitespace to JavaScript's `String.prototype.trim`: ECMAScript's
/// *WhiteSpace* (TAB, VT, FF, ZWNBSP, every `Zs` character) plus *LineTerminator* (LF, CR,
/// LS, PS).
///
/// This differs from [`char::is_whitespace`] in two places: U+FEFF counts here and not in Rust,
/// and U+0085 counts in Rust and not here.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// `s.trim()` as JavaScript computes it.
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

#[cfg(test)]
mod tests {
    use super::trim;

    #[test]
    fn trims_what_javascript_trims() {
        assert_eq!(trim(" \t\r\n\u{000B}\u{000C}x\u{00A0}\u{3000}"), "x");
        assert_eq!(trim("\u{FEFF}x\u{2028}\u{2029}"), "x");
        assert_eq!(trim("\u{2000}\u{200A}x\u{202F}\u{205F}\u{1680}"), "x");
    }

    #[test]
    fn keeps_what_javascript_keeps() {
        // NEL is whitespace to Rust but not to JavaScript; ZWSP is neither.
        assert_eq!(trim("\u{0085}x\u{0085}"), "\u{0085}x\u{0085}");
        assert_eq!(trim("\u{200B}x\u{200B}"), "\u{200B}x\u{200B}");
    }
}

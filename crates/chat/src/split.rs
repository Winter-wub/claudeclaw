//! Message splitting by a size limit (architecture §4 `claudeclaw-chat`; TS parity, #18).
//!
//! Every TS transport cuts an outgoing reply into fixed-size slices with
//! `for (i = 0; i < s.length; i += MAX) s.slice(i, i + MAX)`: Discord at 2000
//! (`src/commands/discord.ts:261-271`), Slack at 3800 (`src/commands/slack.ts:274-276`) and
//! Telegram at 4096 after its HTML conversion (`src/commands/telegram.ts:403-407`). The limit
//! is the transport's, so each adapter passes its own.
//!
//! JavaScript lengths count UTF-16 code units, so a limit here does too: a Thai or accented
//! letter counts 1, an emoji outside the BMP counts 2. The slices ignore Markdown, so a code
//! fence or a combining mark can be cut in two, as in TS. The one divergence: TS can cut an
//! emoji's surrogate pair in half and send two broken halves; here the chunk ends before the
//! pair instead, one unit short, and the next chunk starts with it.

use crate::{js, react};

/// Cuts `text` into chunks of at most `limit` UTF-16 code units each, as the TS slicing loop
/// does (module docs). Empty text gives no chunks.
///
/// A single character wider than `limit` (an emoji when `limit` is 1) still gets a chunk of
/// its own, so every character is sent.
///
/// # Panics
///
/// If `limit` is 0.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    assert!(limit > 0, "split_message: limit must be at least 1");
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut units = 0;
    for (i, c) in text.char_indices() {
        let width = c.len_utf16();
        if units + width > limit && i > start {
            chunks.push(text[start..i].to_owned());
            start = i;
            units = 0;
        }
        units += width;
    }
    if start < text.len() {
        chunks.push(text[start..].to_owned());
    }
    chunks
}

/// The chunks to send for a reply: `[react:x]` directives removed, the text trimmed, then
/// [`split_message`]. Blank text gives no chunks, so nothing is sent (TS
/// `discordMessageChunks`, `src/commands/discord.ts:263-271`, and Slack's `sendMessage`,
/// `src/commands/slack.ts:270-276`).
///
/// # Panics
///
/// If `limit` is 0.
pub fn reply_chunks(text: &str, limit: usize) -> Vec<String> {
    split_message(js::trim(&react::strip_react_directives(text)), limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discord's limit (`src/commands/discord.ts:261`).
    const DISCORD: usize = 2000;

    fn lens(chunks: &[String]) -> Vec<usize> {
        chunks.iter().map(|c| c.encode_utf16().count()).collect()
    }

    /// Expected values come from running the TS loop
    /// (`src/commands/discord.ts:266-269`) on each input.
    #[test]
    fn split_matches_ts() {
        let cases: &[(&str, usize, &[&str])] = &[
            ("", 3, &[]),
            ("abc", 3, &["abc"]),
            ("abcd", 3, &["abc", "d"]),
            ("abcdefg", 3, &["abc", "def", "g"]),
            ("abc", 1, &["a", "b", "c"]),
            // Whitespace is kept as is; only `reply_chunks` trims.
            ("  ab  ", 3, &["  a", "b  "]),
            // A code fence is cut where the limit falls, opening and closing halves alike.
            (
                "x```rust\nfn f() {}\n```",
                8,
                &["x```rust", "\nfn f() ", "{}\n```"],
            ),
            ("```ab```", 4, &["```a", "b```"]),
            // BMP letters count one unit whatever their UTF-8 width.
            ("ไทยไทย", 3, &["ไทย", "ไทย"]),
            ("éàüö", 3, &["éàü", "ö"]),
            // Combining marks are cut from their base, as in TS: `ที่` is ท, sara ii, mai ek.
            (
                "\u{0E17}\u{0E35}\u{0E48}\u{0E17}\u{0E35}\u{0E48}",
                2,
                &["\u{0E17}\u{0E35}", "\u{0E48}\u{0E17}", "\u{0E35}\u{0E48}"],
            ),
            // Two-unit emoji that fit the boundary exactly.
            ("🔥🔥🔥", 4, &["🔥🔥", "🔥"]),
            ("a🔥b", 3, &["a🔥", "b"]),
        ];
        for &(input, limit, want) in cases {
            assert_eq!(split_message(input, limit), want, "{input:?} at {limit}");
        }
    }

    /// The divergence in the module docs: where TS would cut a surrogate pair, the chunk ends
    /// one unit early instead.
    #[test]
    fn never_cuts_a_surrogate_pair() {
        // TS: ["ab\uD83D", "\uDD25c"].
        assert_eq!(split_message("ab🔥c", 3), ["ab", "🔥c"]);
        // TS: ["\uD83D", "\uDD25"] for each emoji.
        assert_eq!(split_message("🔥🔥", 1), ["🔥", "🔥"]);
        // An emoji right at Discord's boundary.
        let text = format!("{}🔥{}", "a".repeat(DISCORD - 1), "b".repeat(5));
        let chunks = split_message(&text, DISCORD);
        assert_eq!(lens(&chunks), [DISCORD - 1, 2 + 5]);
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn discord_sized_replies() {
        let text = "a".repeat(DISCORD * 2 + 1);
        assert_eq!(lens(&split_message(&text, DISCORD)), [DISCORD, DISCORD, 1]);
        // 2000 Thai letters are 6000 UTF-8 bytes but one Discord message.
        let thai = "ก".repeat(DISCORD);
        assert_eq!(split_message(&thai, DISCORD), std::slice::from_ref(&thai));
        let thai_plus = format!("{thai}ข");
        assert_eq!(split_message(&thai_plus, DISCORD), [thai, "ข".to_owned()]);
    }

    #[test]
    #[should_panic(expected = "limit must be at least 1")]
    fn zero_limit_panics() {
        split_message("a", 0);
    }

    /// Expected values come from TS `discordMessageChunks` (`src/commands/discord.ts:263-271`):
    /// strip directives, `trim`, return `[]` when empty, then slice.
    #[test]
    fn reply_chunks_match_ts() {
        let cases: &[(&str, usize, &[&str])] = &[
            ("", 3, &[]),
            ("  \n\t ", 3, &[]),
            ("[react:🔥]", 3, &[]),
            (" [react:🔥] ", 3, &[]),
            ("  hello  ", 3, &["hel", "lo"]),
            ("ab[react:x]cd", 3, &["abc", "d"]),
            // Several directives, and a malformed one that stays in the text.
            ("[react:a]x[react:b]y[react:]", 4, &["xy[r", "eact", ":]"]),
            // Only stripped and trimmed, never folded: blank lines stay.
            ("a\n\n\n\nb", 10, &["a\n\n\n\nb"]),
            ("\u{FEFF}hi\u{FEFF}", 10, &["hi"]),
        ];
        for &(input, limit, want) in cases {
            assert_eq!(reply_chunks(input, limit), want, "{input:?} at {limit}");
        }
    }
}

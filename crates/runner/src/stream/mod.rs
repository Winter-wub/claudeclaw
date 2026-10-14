//! The pure stream-json parser and result judging (architecture §4 runner `stream`; #5, #8,
//! #18, #39, #42, #47).
//!
//! No tokio and no IO: spawn hands over stdout lines (`LinesCodec::new_with_max_length`) and,
//! at the end, an [`ExitReport`]. The flow is:
//!
//! 1. [`parse_line`] turns each line into a [`StreamItem`]: a [`Event`] tagged by `type` with a
//!    catch-all, or [`StreamItem::Noise`] for non-JSON lines, which are kept for the run log.
//! 2. [`StreamState::observe`] folds it in and returns [`StreamEvent`]s for `RunEvents`: text
//!    deltas, tool use and results, agent spawn/done, the session ID, the Claude Code version
//!    (#42 D2) and a [`TranscriptHint`] (#39 D1).
//! 3. [`judge`] gives the [`RunOutcome`] from the state, the [`ExitReport`] and the run's
//!    [`crate::argv::ClaudeArgs`].
//!
//! Wiring spawn → stream → `RunEvents` is the integration PR's job.

mod hint;
mod item;
mod judge;
mod state;

pub use hint::{TranscriptHint, TranscriptHintError};
pub use item::{
    AssistantEvent, AssistantMessage, ContentBlock, CostBasis, Event, MemoryPaths, ModelUsage,
    ResultEvent, StreamItem, SystemEvent, Usage, UserEvent, UserMessage,
};
pub use judge::{
    ExitReport, Failure, FlagSource, RateLimitReset, Refusal, RunClock, RunOutcome, Verdict, judge,
};
pub use state::{StreamEvent, StreamState};

use serde::Deserialize;

/// Parses one stdout line (architecture §4 runner `stream`). Never fails: a line that isn't a
/// JSON object is [`StreamItem::Noise`], and an object without a usable `type` tag is
/// [`StreamItem::Malformed`]. Surrounding whitespace (a CR before LF) is ignored.
pub fn parse_line(line: &str) -> StreamItem {
    let trimmed = line.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) if value.is_object() => match Event::deserialize(value) {
            Ok(event) => StreamItem::Event(event),
            Err(_) => StreamItem::Malformed(line.to_owned()),
        },
        _ => StreamItem::Noise(line.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::Version;

    fn kind(item: &StreamItem) -> &'static str {
        match item {
            StreamItem::Event(Event::System(_)) => "system",
            StreamItem::Event(Event::Assistant(_)) => "assistant",
            StreamItem::Event(Event::User(_)) => "user",
            StreamItem::Event(Event::Result(_)) => "result",
            StreamItem::Event(Event::Other) => "other",
            StreamItem::Malformed(_) => "malformed",
            StreamItem::Noise(_) => "noise",
        }
    }

    #[test]
    fn line_kinds() {
        let cases: &[(&str, &str)] = &[
            (r#"{"type":"system","subtype":"init"}"#, "system"),
            (
                r#"{"type":"system","subtype":"api_retry","attempt":1}"#,
                "system",
            ),
            (
                r#"{"type":"assistant","message":{"content":[]}}"#,
                "assistant",
            ),
            (r#"{"type":"user","message":{"content":"hi"}}"#, "user"),
            (
                r#"{"type":"result","subtype":"success","is_error":false}"#,
                "result",
            ),
            ("{\"type\":\"result\"}\r", "result"),
            // Catch-all for types this build doesn't know.
            (r#"{"type":"stream_event","event":{}}"#, "other"),
            (r#"{"type":"rate_limit_event"}"#, "other"),
            // JSON objects without a usable tag.
            (r#"{"no_type":true}"#, "malformed"),
            (r#"{"type":7}"#, "malformed"),
            // Not JSON objects: wrapper noise (#5).
            ("[OK] Synchronized marketplace registry paths", "noise"),
            ("[CCR remote] event", "noise"),
            ("╭──────────╮", "noise"),
            ("", "noise"),
            ("42", "noise"),
            (r#"["type","result"]"#, "noise"),
            (r#"{"type":"result""#, "noise"),
        ];
        for (line, want) in cases {
            assert_eq!(kind(&parse_line(line)), *want, "line {line:?}");
        }
    }

    #[test]
    fn noise_keeps_the_line() {
        let StreamItem::Noise(kept) = parse_line("[ccs] hello") else {
            panic!("not noise");
        };
        assert_eq!(kept, "[ccs] hello");
        assert_eq!(
            format!("{:?}", parse_line("[ccs] hello")),
            "Noise(11 bytes)"
        );
    }

    #[test]
    fn wrong_field_types_read_as_absent() {
        let line = r#"{"type":"result","subtype":5,"is_error":"yes","result":["x"],"api_error_status":"429","num_turns":-1,"usage":{"input_tokens":"many","output_tokens":3},"modelUsage":[],"errors":"nope"}"#;
        let StreamItem::Event(Event::Result(r)) = parse_line(line) else {
            panic!("not a result");
        };
        assert_eq!(r.subtype, None);
        assert!(!r.is_error);
        assert_eq!(r.result, None);
        assert_eq!(r.api_error_status, None);
        assert_eq!(r.num_turns, None);
        assert_eq!(
            r.usage,
            Some(Usage {
                output_tokens: 3,
                ..Usage::default()
            })
        );
        assert!(r.model_usage.is_empty() && r.errors.is_empty());
    }

    #[test]
    fn result_fields() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"s-1","num_turns":2,"total_cost_usd":0.012,"usage":{"input_tokens":1000,"output_tokens":1234,"cache_read_input_tokens":5,"cache_creation_input_tokens":6},"modelUsage":{"claude-x":{"inputTokens":10,"outputTokens":20,"cacheReadInputTokens":30,"cacheCreationInputTokens":40,"costUSD":0.5,"contextWindow":1000000,"costBasis":"managed","provider":"firstParty"},"glm-4.6":{"costUSD":0.1,"costBasis":"unknown"},"other":{"costBasis":"surprise"},"bare":{}}}"#;
        let StreamItem::Event(Event::Result(r)) = parse_line(line) else {
            panic!("not a result");
        };
        assert_eq!(r.total_cost_usd, Some(0.012));
        let u = r.usage.unwrap();
        assert_eq!((u.input_tokens, u.output_tokens), (1000, 1234));
        assert_eq!(u.context_tokens(), 1011);
        let m = &r.model_usage["claude-x"];
        assert_eq!(
            (
                m.input_tokens,
                m.output_tokens,
                m.cache_read_input_tokens,
                m.cache_creation_input_tokens
            ),
            (10, 20, 30, 40)
        );
        assert_eq!(m.cost_usd, Some(0.5));
        assert_eq!(m.context_window, Some(1_000_000));
        assert_eq!(m.provider.as_deref(), Some("firstParty"));
        // #39 D9: missing means list; anything unrecognised is unknown.
        assert_eq!(m.cost_basis, CostBasis::Managed);
        assert_eq!(r.model_usage["glm-4.6"].cost_basis, CostBasis::Unknown);
        assert_eq!(r.model_usage["other"].cost_basis, CostBasis::Unknown);
        assert_eq!(r.model_usage["bare"].cost_basis, CostBasis::List);
    }

    #[test]
    fn unknown_content_blocks_are_kept_as_other() {
        let line = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"a"},{"type":"text","text":5},{"type":"server_tool_use"}]}}"#;
        let StreamItem::Event(Event::Assistant(a)) = parse_line(line) else {
            panic!("not assistant");
        };
        let content = a.message.unwrap().content;
        assert_eq!(content.len(), 4);
        assert!(matches!(content[0], ContentBlock::Other));
        assert!(matches!(&content[1], ContentBlock::Text { text } if text == "a"));
        assert!(matches!(content[2], ContentBlock::Other));
        assert!(matches!(content[3], ContentBlock::Other));
    }

    fn events(lines: &[&str]) -> Vec<StreamEvent> {
        let mut s = StreamState::new();
        lines.iter().flat_map(|l| s.push_line(l)).collect()
    }

    fn texts(evs: &[StreamEvent]) -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn init_gives_session_version_and_hint() {
        let mem = std::env::temp_dir()
            .join("cfg")
            .join("projects")
            .join("-p")
            .join("memory");
        let init = serde_json::json!({
            "type": "system", "subtype": "init", "session_id": "s-1",
            "claude_code_version": "2.1.296", "apiKeySource": "none",
            "memory_paths": {"auto": mem},
        })
        .to_string();
        let mut s = StreamState::new();
        let evs = s.push_line(&init);
        assert_eq!(evs.len(), 3, "{evs:?}");
        assert_eq!(evs[0], StreamEvent::SessionId("s-1".into()));
        let StreamEvent::ClaudeCodeVersion(v) = &evs[1] else {
            panic!("{evs:?}")
        };
        assert_eq!(v.parsed, Some(Version::new(2, 1, 296)));
        let StreamEvent::TranscriptHint(h) = &evs[2] else {
            panic!("{evs:?}")
        };
        assert_eq!(h.dir(), mem.parent().unwrap());
        // The same session ID again is not a new event.
        assert!(
            s.push_line(r#"{"type":"system","subtype":"status","session_id":"s-1"}"#)
                .is_empty()
        );
    }

    #[test]
    fn init_with_bad_memory_path_gives_no_hint() {
        let evs = events(&[
            r#"{"type":"system","subtype":"init","claude_code_version":"x","memory_paths":{"auto":"relative/memory/"}}"#,
        ]);
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], StreamEvent::ClaudeCodeVersion(v) if v.parsed.is_none()));
    }

    #[test]
    fn text_deltas_per_message() {
        // TS `src/runner.ts:520-541`: per message ID, only new text; "\n" between messages.
        let evs = events(&[
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hel"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hello"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}]}}"#,
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"Bye"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m3","content":[{"type":"text","text":"A"},{"type":"text","text":"B"}]}}"#,
        ]);
        assert_eq!(texts(&evs), ["Hel", "lo", "\n", "Bye", "\n", "AB"]);
    }

    fn thread_texts(evs: &[StreamEvent], want_subagent: bool) -> String {
        evs.iter()
            .filter_map(|e| match e {
                StreamEvent::TextDelta { text, subagent } if *subagent == want_subagent => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn subagent_messages_keep_their_own_separator_state() {
        // A subagent message between two main messages doesn't eat the main "\n" (TS
        // `src/runner.ts:523-526` gives "Hi\nBye").
        let evs = events(&[
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hi"},{"type":"tool_use","id":"a1","name":"Agent","input":{"description":"d"}}]},"parent_tool_use_id":null}"#,
            r#"{"type":"assistant","message":{"id":"s1","content":[{"type":"tool_use","id":"g1","name":"Glob","input":{"pattern":"*"}}]},"parent_tool_use_id":"a1"}"#,
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"Bye"}]},"parent_tool_use_id":null}"#,
        ]);
        assert_eq!(thread_texts(&evs, false), "Hi\nBye");
        assert_eq!(thread_texts(&evs, true), "");

        // The reverse: subagent text first doesn't start the main reply with "\n".
        let evs = events(&[
            r#"{"type":"assistant","message":{"id":"s1","content":[{"type":"text","text":"sub"}]},"parent_tool_use_id":"a1"}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Hello"}]},"parent_tool_use_id":null}"#,
            r#"{"type":"assistant","message":{"id":"s2","content":[{"type":"text","text":"more"}]},"parent_tool_use_id":"a1"}"#,
        ]);
        assert_eq!(thread_texts(&evs, false), "Hello");
        assert_eq!(thread_texts(&evs, true), "sub\nmore");
    }

    #[test]
    fn a_later_block_is_not_cut_by_an_earlier_one() {
        // One message's blocks arriving one per event: text, thinking, text.
        let evs = events(&[
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Done"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"thinking","thinking":"hm"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Done. Next"}]}}"#,
        ]);
        assert_eq!(texts(&evs), ["Done", "Done. Next"]);
    }

    #[test]
    fn a_snapshot_repeats_nothing() {
        // The same message re-sent with all its blocks so far: only new text and new tools.
        let evs = events(&[
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Look"},{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"Look"},{"type":"tool_use","id":"t1","name":"Read","input":{}},{"type":"text","text":"ed"},{"type":"tool_use","id":"t2","name":"Bash","input":{}}]}}"#,
        ]);
        assert_eq!(texts(&evs), ["Look", "ed"]);
        let tools: Vec<&str> = evs
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tools, ["t1", "t2"]);
    }

    #[test]
    fn outcome_model_is_the_main_threads_full_id() {
        let usage = r#""modelUsage":{"claude-main-1":{"contextWindow":1000000},"claude-sub-1":{"contextWindow":200000}}"#;
        let result = format!(r#"{{"type":"result","subtype":"success","is_error":false,{usage}}}"#);
        let judged = |lines: &[&str]| {
            let mut s = StreamState::new();
            for l in lines {
                s.push_line(l);
            }
            judge(&s, &ExitReport::default(), &crate::argv::ClaudeArgs::new())
        };
        let init = r#"{"type":"system","subtype":"init","model":"claude-main-1"}"#;

        // The last main-thread message's model; a subagent's or a synthetic one doesn't count.
        let out = judged(&[
            init,
            r#"{"type":"assistant","message":{"id":"m1","model":"claude-main-1","content":[]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":"a1","message":{"id":"s1","model":"claude-sub-1","content":[]}}"#,
            r#"{"type":"assistant","message":{"id":"m2","model":"<synthetic>","content":[]}}"#,
            &result,
        ]);
        assert_eq!(out.model.as_deref(), Some("claude-main-1"));
        assert_eq!(out.context_window(), Some(1_000_000));

        // Falls back to init.model when no main-thread message named one.
        let out = judged(&[init, &result]);
        assert_eq!(out.model.as_deref(), Some("claude-main-1"));
        assert_eq!(out.context_window(), Some(1_000_000));

        // A main-thread model newer than init wins.
        let out = judged(&[
            init,
            r#"{"type":"assistant","message":{"id":"m1","model":"claude-sub-1","content":[]}}"#,
            &result,
        ]);
        assert_eq!(out.context_window(), Some(200_000));

        // No model anywhere: no window.
        let out = judged(&[&result]);
        assert_eq!((out.model.as_deref(), out.context_window()), (None, None));
    }

    #[test]
    fn tool_use_and_results() {
        let evs = events(&[
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"src/main.rs"}},{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"line one\nline two"}]}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"boom\nmore","is_error":true}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t9","content":""}]}}"#,
        ]);
        let summaries: Vec<String> = evs
            .iter()
            .map(|e| match e {
                StreamEvent::ToolUse { name, summary, .. } => format!("use {name} {summary}"),
                StreamEvent::ToolResult {
                    name,
                    summary,
                    is_error,
                    ..
                } => format!("result {name:?} {summary} {is_error}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            summaries,
            [
                "use Read Read(src/main.rs)",
                "use Bash Bash(ls)",
                r#"result Some("Read") line one false"#,
                r#"result Some("Bash") Error: boom true"#,
                "result None done false",
            ]
        );
    }

    #[test]
    fn tool_call_summaries_match_ts() {
        // TS `formatToolCallSummary`, `src/runner.ts:588-601`.
        use serde_json::json;
        let long = "x".repeat(70);
        let cases = [
            (
                "Write",
                json!({"file_path": "a.txt"}),
                "Write(a.txt)".to_owned(),
            ),
            ("Edit", json!({}), "Edit()".to_owned()),
            (
                "Bash",
                json!({"command": long}),
                format!("Bash({})", "x".repeat(60)),
            ),
            (
                "Read",
                json!({"file_path": long}),
                format!("Read({})", "x".repeat(50)),
            ),
            ("Grep", json!({"pattern": "fn"}), "Grep(fn in .)".to_owned()),
            (
                "Grep",
                json!({"pattern": "fn", "path": "src"}),
                "Grep(fn in src)".to_owned(),
            ),
            ("Glob", json!({"pattern": "*.rs"}), "Glob(*.rs)".to_owned()),
            (
                "WebSearch",
                json!({"query": "q"}),
                "WebSearch(q)".to_owned(),
            ),
            (
                "WebFetch",
                json!({"url": "https://example.com"}),
                "WebFetch(https://example.com)".to_owned(),
            ),
            (
                "Agent",
                json!({"description": "d"}),
                "Agent(...)".to_owned(),
            ),
            ("mcp__x__y", json!({}), "mcp__x__y(...)".to_owned()),
        ];
        for (name, input, want) in cases {
            assert_eq!(state::tool_call_summary(name, &input), want, "{name}");
        }
    }

    #[test]
    fn agent_spawn_and_done() {
        let evs = events(&[
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"a1","name":"Agent","input":{"description":"look around","prompt":"p"}},{"type":"tool_use","id":"a2","name":"Agent","input":{"prompt":"only prompt"}},{"type":"tool_use","id":"a3","name":"Agent","input":{}}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":"a1","message":{"id":"m9","content":[{"type":"text","text":"sub"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a1","content":"found it"},{"type":"tool_result","tool_use_id":"a2","content":[{"type":"text","text":"x"}]}]}}"#,
        ]);
        let agents: Vec<String> = evs
            .iter()
            .filter_map(|e| match e {
                StreamEvent::AgentSpawn { id, description } => {
                    Some(format!("spawn {id} {description}"))
                }
                StreamEvent::AgentDone {
                    id,
                    description,
                    result,
                } => Some(format!("done {id} {description} {result}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            agents,
            [
                "spawn a1 look around",
                "spawn a2 only prompt",
                "spawn a3 Running background task...",
                "done a1 look around found it",
                r#"done a2 only prompt [{"type":"text","text":"x"}]"#,
            ]
        );
        assert!(evs.iter().any(|e| matches!(
            e,
            StreamEvent::TextDelta { text, subagent: true } if text == "sub"
        )));
    }

    #[test]
    fn last_main_thread_usage_ignores_subagents() {
        let mut s = StreamState::new();
        for l in [
            r#"{"type":"assistant","message":{"id":"m1","content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":100}}}"#,
            r#"{"type":"assistant","parent_tool_use_id":"a1","message":{"id":"m2","content":[],"usage":{"input_tokens":999}}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"id":"m3","content":[],"usage":{"input_tokens":20,"cache_creation_input_tokens":3}}}"#,
            r#"{"type":"assistant","message":{"id":"m4","content":[]}}"#,
        ] {
            s.push_line(l);
        }
        let out = judge(&s, &ExitReport::default(), &crate::argv::ClaudeArgs::new());
        let u = out.last_main_usage.unwrap();
        assert_eq!(u.context_tokens(), 23);
    }

    #[test]
    fn debug_never_prints_text() {
        let evs = events(&[
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"private words"}]}}"#,
        ]);
        let dbg = format!("{evs:?}");
        assert!(!dbg.contains("private"), "{dbg}");
        let item = parse_line(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"private words"}"#,
        );
        assert!(!format!("{item:?}").contains("private"));
    }
}

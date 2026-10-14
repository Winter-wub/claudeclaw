//! The hand-made stream-json fixtures, run through the public `stream` API end to end:
//! `parse_line` → `StreamState` → `judge` (architecture §4 runner `stream`).

use std::path::PathBuf;

use claudeclaw_runner::argv::ClaudeArgs;
use claudeclaw_runner::stream::{
    CostBasis, ExitReport, Failure, RateLimitReset, StreamEvent, StreamItem, StreamState, Verdict,
    judge, parse_line,
};
use claudeclaw_runner::version::VersionCheck;

/// An absolute config dir on every OS, standing in for `@CONFIG@`.
fn config_dir() -> PathBuf {
    std::env::temp_dir().join("claudeclaw-fixture-config")
}

/// Replays a fixture, returning the state, every event and the noise lines kept.
fn replay(fixture: &str) -> (StreamState, Vec<StreamEvent>, Vec<String>) {
    let escaped = serde_json::to_string(config_dir().to_str().unwrap()).unwrap();
    let config = escaped.trim_matches('"');
    let mut state = StreamState::new();
    let mut events = Vec::new();
    let mut noise = Vec::new();
    for line in fixture.replace("@CONFIG@", config).lines() {
        let item = parse_line(line);
        if let StreamItem::Noise(kept) = &item {
            noise.push(kept.clone());
        }
        events.extend(state.observe(&item));
    }
    (state, events, noise)
}

fn exit(code: i32) -> ExitReport {
    ExitReport {
        exit_code: Some(code),
        ..ExitReport::default()
    }
}

fn reply_text(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta {
                text,
                subagent: false,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn plain_reply() {
    let (state, events, noise) = replay(include_str!("fixtures/stream/plain_reply.ndjson"));
    assert!(noise.is_empty());
    let out = judge(&state, &exit(0), &ClaudeArgs::new());
    assert_eq!(out.verdict, Verdict::Ok);
    assert_eq!(out.reply.as_deref(), Some("สวัสดี! Hello."));
    assert_eq!(reply_text(&events), "สวัสดี! Hello.");
    assert_eq!(
        out.session_id.as_deref(),
        Some("00000000-0000-4000-8000-0000000000a1")
    );
    assert_eq!(out.api_key_source.as_deref(), Some("none"));
    assert_eq!(out.version_check(), Some(VersionCheck::Supported));

    // #39 D1: the hint names the project dir, and the transcript is <dir>/<session>.jsonl.
    let hint = out.transcript_hint.as_ref().expect("hint");
    let project = config_dir().join("projects").join("-work");
    assert_eq!(hint.dir(), project);
    assert_eq!(
        hint.transcript_path("00000000-0000-4000-8000-0000000000a1"),
        Some(project.join("00000000-0000-4000-8000-0000000000a1.jsonl"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::TranscriptHint(_)))
    );

    // #39 D4, D9, E10.
    assert_eq!(out.last_main_usage.unwrap().context_tokens(), 119);
    assert_eq!(out.context_window("claude-test"), Some(1_000_000));
    let m = &out.model_usage["claude-test"];
    assert_eq!(m.cost_basis, CostBasis::List);
    assert_eq!(m.provider.as_deref(), Some("firstParty"));
    assert_eq!(out.total_cost_usd, Some(0.003));
    assert_eq!(out.usage.unwrap().output_tokens, 5);
}

#[test]
fn tool_use() {
    let (state, events, _) = replay(include_str!("fixtures/stream/tool_use.ndjson"));
    let out = judge(&state, &exit(0), &ClaudeArgs::new());
    assert_eq!(out.verdict, Verdict::Ok);
    assert_eq!(reply_text(&events), "Let me look.\nIt is a title.");
    let tools: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ToolUse { summary, .. } => Some(summary.clone()),
            StreamEvent::ToolResult { name, summary, .. } => {
                Some(format!("[{}] {summary}", name.as_deref().unwrap_or("?")))
            }
            _ => None,
        })
        .collect();
    assert_eq!(tools, ["Read(README.md)", "[Read] # Title"]);
    // The last main-thread usage, not the run total (#39 D4).
    assert_eq!(out.last_main_usage.unwrap().context_tokens(), 130);
    assert_eq!(out.api_key_source.as_deref(), Some("ANTHROPIC_API_KEY"));
    // Exactly the minimum is supported.
    assert_eq!(out.version_check(), Some(VersionCheck::Supported));
    // No memory_paths (auto memory off, #39 E2): no hint.
    assert!(out.transcript_hint.is_none());
}

#[test]
fn agent() {
    let (state, events, _) = replay(include_str!("fixtures/stream/agent.ndjson"));
    let out = judge(&state, &exit(0), &ClaudeArgs::new());
    assert_eq!(out.verdict, Verdict::Ok);
    let agent: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::AgentSpawn { id, description } => {
                Some(format!("spawn {id} {description}"))
            }
            StreamEvent::AgentDone { id, result, .. } => Some(format!("done {id} {result}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        agent,
        [
            "spawn toolu_a1 Survey the repo",
            r#"done toolu_a1 [{"type":"text","text":"One file: src/lib.rs"}]"#,
        ]
    );
    assert!(events.iter().any(|e| matches!(
        e,
        StreamEvent::ToolUse { name, subagent: true, .. } if name == "Glob"
    )));
    // The subagent's 9,000-token request doesn't move the context meter.
    assert_eq!(out.last_main_usage.unwrap().context_tokens(), 60);
    assert_eq!(
        out.model_usage["claude-small"].cost_basis,
        CostBasis::Unknown
    );
}

#[test]
fn api_error_429() {
    let (state, _, _) = replay(include_str!("fixtures/stream/api_error_429.ndjson"));
    let out = judge(&state, &exit(1), &ClaudeArgs::new());
    assert_eq!(
        out.verdict,
        Verdict::Failed(Failure::ResultError {
            subtype: Some("success".into()),
            api_error_status: Some(429),
        })
    );
    assert_eq!(out.api_error_status, Some(429));
    assert_eq!(out.is_error, Some(true));
}

#[test]
fn session_not_found() {
    let (state, events, _) = replay(include_str!("fixtures/stream/session_not_found.ndjson"));
    let mut ex = exit(1);
    ex.stderr =
        "No conversation found with session ID: 00000000-0000-4000-8000-0000000000ff\n".into();
    let out = judge(&state, &ex, &ClaudeArgs::new());
    assert_eq!(out.verdict, Verdict::Failed(Failure::SessionNotFound));
    // #39 E3: no init, so no version and no hint.
    assert!(out.claude_code_version.is_none() && out.transcript_hint.is_none());
    assert_eq!(events.len(), 1, "only the session ID: {events:?}");
}

#[test]
fn wrapper_noise_is_kept_and_ignored() {
    let (state, events, noise) = replay(include_str!("fixtures/stream/wrapper_noise.ndjson"));
    assert_eq!(noise.len(), 6, "{noise:?}");
    assert_eq!(noise[3], "[OK] Synchronized marketplace registry paths");
    assert_eq!(noise[4], "[CCR remote] inbound sync event");
    let out = judge(&state, &exit(0), &ClaudeArgs::new());
    assert_eq!(out.verdict, Verdict::Ok);
    assert_eq!(reply_text(&events), "ok");
}

#[test]
fn rate_limit() {
    let (state, _, _) = replay(include_str!("fixtures/stream/rate_limit.ndjson"));
    let out = judge(&state, &exit(1), &ClaudeArgs::new());
    assert_eq!(
        out.verdict,
        Verdict::RateLimited {
            reset: Some(RateLimitReset {
                hour: 15,
                minute: 0
            })
        }
    );
}

#[test]
fn no_result() {
    let (state, events, _) = replay(include_str!("fixtures/stream/no_result.ndjson"));
    // #8: no result is a failure even on exit 0.
    let out = judge(&state, &exit(0), &ClaudeArgs::new());
    assert_eq!(out.verdict, Verdict::Failed(Failure::NoResult));
    // Below the minimum: recorded, judged as usual (#42 D4).
    assert_eq!(out.version_check(), Some(VersionCheck::Outdated));
    assert_eq!(reply_text(&events), "Starting");

    // #47 D3's other side: no result before the timeout is a timeout.
    let timed_out = ExitReport {
        timed_out: true,
        ..ExitReport::default()
    };
    let out = judge(&state, &timed_out, &ClaudeArgs::new());
    assert_eq!(out.verdict, Verdict::Timeout);
    assert!(!out.lingered);
}

#[test]
fn plain_reply_lingering_past_the_timeout_is_ok() {
    let (state, _, _) = replay(include_str!("fixtures/stream/plain_reply.ndjson"));
    let timed_out = ExitReport {
        timed_out: true,
        signal: Some(15),
        ..ExitReport::default()
    };
    let out = judge(&state, &timed_out, &ClaudeArgs::new());
    assert_eq!(out.verdict, Verdict::Ok);
    assert!(out.lingered);
}

//! End-to-end export tests over a synthetic project dir (#28 D8).

use std::fs;

use claudeclaw_api::export::ExportSection;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;

/// A fake snowflake of `len` repeated digits, built at runtime so no long digit run sits in
/// the source (gitleaks).
fn flake(d: char) -> String {
    std::iter::repeat_n(d, 18).collect()
}

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

const SESSION: &str = "5E55-AB12-CD34";
const DEVICE: &str = "dev-7f3a9c";
const TOKEN: &str = "tok-91b2e0";

struct Fixture {
    _dir: TempDir,
    layout: Layout,
    input: ExportInput,
}

fn write(path: &std::path::Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn log_line(seq: u64, ts: &str, level: &str, event: &str, message: &str, extra: Value) -> String {
    let mut v = json!({
        "seq": seq, "timestamp": ts, "level": level, "message": message, "event": event,
        "target": "claudeclaw_core::conversations",
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v.to_string()
}

fn run_record(
    run: u32,
    started: &str,
    name: &str,
    scope: &str,
    launcher: &str,
    job: Option<&str>,
) -> Value {
    json!({
        "run_id": format!("0199e0c2-0000-7000-8000-00000000000{run}"),
        "turn_id": null,
        "name": name,
        "scope": scope,
        "origin": "scheduler",
        "launcher": launcher,
        "job": job,
        "agent": null,
        "session_id": SESSION,
        "session": "resumed",
        "model": "claude-sonnet-5-5",
        "started_at": started,
        "ended_at": null,
        "duration_ms": 1000,
        "outcome": "ok",
        "exit_code": 0,
        "is_error": false,
        "api_error_status": null,
        "attempt": 1,
        "tokens_in": null,
        "tokens_out": null,
        "cache_read_tokens": null,
        "cache_write_tokens": null,
        "cost_usd": 0.30000000000000004,
        "cost_status": "priced",
        "cost_basis": null,
        "provider": null,
        "api_key_source": null,
        "models": [],
        "run_log": format!("logs/{name}-2026-10-13T08-30-27-512Z.log"),
        "launcher_resolved": "~/.local/bin/claude",
        "purpose": "turn",
        "settle": null,
        "awake_ms": null,
        "suspended_ms": null,
        "wake_grace": false,
        "claude_code_version": "2.1.296",
        "lingered": false,
    })
}

/// A project dir with an identifier of every kind in settings, job dirs, agents, sessions,
/// Devices, tokens, the Run index, the Daemon log and the Crash record, plus `CANARY-` content
/// that must never reach the export.
fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let layout = Layout::new(dir.path().join("proj"));
    let home = dir.path().to_string_lossy().into_owned();
    let channel = flake('1');
    let user = flake('2');

    write(
        &layout.settings(),
        &json!({
            "model": "opus",
            "api": "CANARY-API-KEY-1",
            "timezone": "UTC",
            "heartbeat": {"enabled": true, "interval": 15, "prompt": "CANARY-PROMPT-1"},
            "security": {"level": "moderate"},
            "discord": {
                "token": "CANARY-TOKEN-1",
                "listenChannels": [channel],
                "allowedUserIds": [serde_json::from_str::<Value>(&user).unwrap()],
            },
            "logging": {"level": "info"},
            "remote": {"enabled": false, "listen": ["CANARY-LISTEN"]},
        })
        .to_string(),
    );
    write(
        &layout.jobs_dir().join("daily-report.md"),
        "CANARY-JOB-BODY",
    );
    write(
        &layout.agent_jobs_dir("researcher").join("digest.md"),
        "CANARY-JOB-BODY",
    );
    write(
        &layout.session_state(),
        &json!({"sessions": [{"scope": "global", "session_id": SESSION}]}).to_string(),
    );
    write(
        &layout.devices(),
        &json!([{"id": DEVICE, "label": "kitchen-laptop", "public_key": "CANARY-KEY"}]).to_string(),
    );
    write(
        &layout.api_tokens(),
        &json!([{"id": TOKEN, "name": "ci-bot", "hash": "CANARY-HASH"}]).to_string(),
    );
    write(
        &layout
            .run_logs_dir()
            .join("daily-report-2026-10-13T08-30-27-512Z.log"),
        "CANARY-RUN-LOG",
    );
    write(&layout.audit_log("2026-10"), "CANARY-AUDIT");
    write(
        &layout.run_index("2026-10"),
        &[
            run_record(
                1,
                "2026-10-13T08:30:27Z",
                "daily-report",
                "job:daily-report",
                "direct",
                Some("daily-report"),
            ),
            run_record(
                2,
                "2026-10-13T09:00:00Z",
                "heartbeat",
                "global",
                "corp-proxy.sh",
                None,
            ),
            run_record(
                3,
                "2026-10-10T09:00:00Z",
                "old-job",
                "job:old-job",
                "direct",
                Some("old-job"),
            ),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n"),
    );
    let lines = [
        log_line(
            1,
            "2026-10-13T08:30:27Z",
            "INFO",
            "run.start",
            "run started",
            json!({"session_id": SESSION, "span": {"scope": format!("discord:{channel}"),
                "launcher": "direct", "name": "daily-report"}}),
        ),
        log_line(
            2,
            "2026-10-13T08:31:00Z",
            "WARN",
            "discord.send_failed",
            &format!("POST channels/{channel}/messages: 403 for daily-report"),
            json!({"author_id": user, "err": format!("open {home}/notes/x.txt: denied")}),
        ),
        log_line(
            3,
            "2026-10-13T08:32:00Z",
            "WARN",
            "api.auth_failed",
            "auth failed",
            json!({"peer": "192.0.2.7:4443", "listener": "remote",
                "span": {"principal": format!("device:{DEVICE}")}}),
        ),
        log_line(
            4,
            "2026-10-13T08:33:00Z",
            "INFO",
            "pairing.completed",
            "paired kitchen-laptop",
            json!({"label": "kitchen-laptop", "token_name": "ci-bot",
                "span": {"principal": format!("token:{TOKEN}")}, "tool": "mcp__corp__lookup"}),
        ),
        log_line(
            7,
            "2026-10-13T08:33:30Z",
            "WARN",
            "api.auth_failed",
            "auth failed",
            json!({"peer": "198.51.100.9", "listener": "remote"}),
        ),
        "thread 'main' panicked at CANARY-TOKEN-1 on box-host".to_owned(),
        log_line(
            5,
            "2026-10-13T08:34:00Z",
            "DEBUG",
            "api.request",
            "request",
            json!({"method": "GET", "route": "/v1/status", "status": 200}),
        ),
    ];
    write(
        &layout.daemon_log("worker", "2026-10-13"),
        &lines.join("\n"),
    );
    write(
        &layout.daemon_log("watchdog", "2026-10-13"),
        &log_line(
            1,
            "2026-10-13T08:29:00Z",
            "INFO",
            "watchdog.start",
            "started",
            json!({"pid": 77}),
        ),
    );
    write(
        &layout.supervisor(),
        &json!({
            "schema": 1,
            "state": "backoff",
            "updated_at": "2026-10-13T08:35:00Z",
            "watchdog": {
                "pid": 4242, "started_at": "2026-10-13T08:00:00Z", "version": "2.0.0",
                "stamp": {"macos": {"sec": 1000, "usec": 1}},
                "service_manager": {"kind": "launchd", "name": "com.claudeclaw.daily-report"},
            },
            "worker": null,
            "next_restart_at": "2026-10-13T08:36:00Z",
            "stop": null,
            "gave_up": null,
            "crashes": [{
                "at": "2026-10-13T08:34:30Z",
                "boot_id": "0199e0c2-0000-7000-8000-00000000000b",
                "uptime_ms": 51000,
                "exit": {"code": 101},
                "fatal": format!("lost channel {channel} for researcher"),
                "cause": "exit",
                "counted": true,
                "reaped": {"killed": 0, "already_gone": 0, "not_ours": 0,
                    "unverified": 0, "unknown_pid": 0, "survived": 0},
                "stderr_tail": [
                    log_line(9, "2026-10-13T08:34:29Z", "WARN", "run.finish", "daily-report failed",
                        json!({"job": "daily-report"})),
                    format!("panicked: session {SESSION}"),
                ],
            }],
        })
        .to_string(),
    );

    let job: JobSummary = serde_json::from_value(json!({
        "name": "daily-report",
        "agent": null,
        "path": ".claude/claudeclaw/jobs/daily-report.md",
        "status": "valid",
        "schedule": "0 9 * * *",
        "recurring": true,
        "enabled": true,
        "notify": "error",
        "launcher": format!("{home}/bin/corp-proxy.sh"),
        "model": null,
        "next_at": "2026-10-14T09:00:00Z",
        "last_run": {"at": "2026-10-13T08:30:27Z", "outcome": "ok"},
        "in_flight": false,
        "retry_at": null,
        "revision": "0123456789abcdef",
    }))
    .unwrap();
    let diagnostic: JobDiagnostic = serde_json::from_value(json!({
        "severity": "warning", "kind": "key.near_miss", "line": 3, "column": 1,
        "message": "CANARY-DIAGNOSTIC quoting the job file",
    }))
    .unwrap();
    let status: DaemonStatus = serde_json::from_value(json!({
        "dir": format!("{home}/proj"),
        "state": "running",
        "health": "degraded",
        "problems": [{"kind": "job_invalid", "scope": "job:daily-report", "job": "daily-report",
            "since": "2026-10-13T08:00:06Z"}],
        "lock_holder": null, "watchdog_pid": 4242, "worker_pid": 4243,
        "version": {"daemon": "2.0.0"},
        "started_at": "2026-10-13T08:00:00Z",
        "boot_id": "0199e0c2-0000-7000-8000-00000000000c",
        "socket": "CANARY-SOCKET",
        "remote": {"enabled": true, "listen": ["CANARY-LISTEN"], "fingerprint": "CANARY-FP",
            "fingerprint_curl": "CANARY-FP"},
        "adapters": [{"transport": "discord", "state": "connected", "since": "2026-10-13T08:00:03Z"}],
        "heartbeat": {"enabled": true, "next_at": "2026-10-13T09:00:00Z"},
        "jobs": {"total": 2, "invalid": 1, "warnings": 0,
            "next": [{"job": "daily-report", "at": "2026-10-14T09:00:00Z"}]},
        "active_runs": 0,
        "launchers": {"default": "direct", "refused_scopes": ["agent:researcher"],
            "direct_resolved": "CANARY-RESOLVED"},
        "update": {"available": false, "latest": null},
        "run_path": null, "run_locale": null, "run_index": null, "beacons": [],
        "config_dir_mismatch": false, "supervisor": null, "source": "api",
    }))
    .unwrap();

    let input = ExportInput {
        now: at("2026-10-13T12:00:00Z"),
        versions: ExportVersions {
            claudeclaw: "2.0.0".into(),
            commit: "88ec5c0".into(),
            target: "aarch64-apple-darwin".into(),
            daemon: Some("2.0.0".into()),
            app: None,
        },
        install_method: Some("cargo".into()),
        os: OsInfo {
            family: "macos".into(),
            version: Some("26.0".into()),
            arch: "aarch64".into(),
        },
        status: Some(status),
        jobs: vec![ExportJob {
            summary: job,
            diagnostics: vec![diagnostic],
        }],
        claude_code: Vec::new(),
        host: HostFacts {
            hostname: Some("box-host".into()),
            os_user: Some("someone".into()),
            home_dir: Some(dir.path().to_path_buf()),
        },
        secrets: vec![(
            SecretString::from("CANARY-PAIRING"),
            SecretLabel::PairingCode,
        )],
    };
    Fixture {
        _dir: dir,
        layout,
        input,
    }
}

fn run(f: &Fixture, request: &ExportRequest) -> LogExport {
    export(&f.layout, &f.input, request).unwrap()
}

/// The `jsonl` lines of a section, parsed.
fn section_lines(text: &str, title: &str) -> Vec<Value> {
    let start = text.find(&format!("## {title}\n")).unwrap();
    let body = &text[start..];
    let body = &body[body.find("```jsonl\n").unwrap() + 9..];
    let body = &body[..body.find("\n```").unwrap_or(0)];
    body.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn every_identifier_kind_is_pseudonymised_and_no_content_appears() {
    let f = fixture();
    let out = run(&f, &ExportRequest::default());
    let text = &out.text;
    let home = f._dir.path().to_string_lossy().into_owned();
    for real in [
        flake('1').as_str(),
        flake('2').as_str(),
        "daily-report",
        "researcher",
        SESSION,
        &SESSION.to_ascii_lowercase(),
        DEVICE,
        TOKEN,
        "kitchen-laptop",
        "ci-bot",
        "corp-proxy.sh",
        "mcp__corp__lookup",
        "192.0.2.7",
        "box-host",
        home.as_str(),
        "CANARY-",
    ] {
        assert!(!text.contains(real), "{real} leaked:\n{text}");
    }
    for pseudonym in [
        "discord:#1",
        "discord:#2",
        "job:#1",
        "agent:#1",
        "session:#1",
        "launcher:#1",
        "device:#1",
        "label:#1",
        "token:#1",
        "token-name:#1",
        "mcp:#1",
        "ip:#1:4443",
        "host:#1",
        "path:#1",
    ] {
        assert!(text.contains(pseudonym), "{pseudonym} missing:\n{text}");
    }
    assert!(text.contains("<redacted:discord-token>"), "{text}");
    for kind in [
        "discord", "job", "agent", "session", "launcher", "device", "token",
    ] {
        assert!(out.manifest.pseudonyms[kind] >= 1, "{kind}");
    }
    assert!(text.starts_with("# claudeclaw-export/1\n\n> Read this file before posting it."));
    assert!(out.map.is_none() && !out.manifest.map_written);
}

#[test]
fn pseudonyms_are_stable_across_sections_fields_and_free_text() {
    let f = fixture();
    let text = run(&f, &ExportRequest::default()).text;
    let channel = "discord:#1";
    let log = section_lines(&text, "Daemon log");
    let start = log.iter().find(|l| l["event"] == "run.start").unwrap();
    assert_eq!(start["span"]["scope"], channel);
    let failed = log
        .iter()
        .find(|l| l["event"] == "discord.send_failed")
        .unwrap();
    // The job was first seen in the Summary's Status problems, so it is job:#1 everywhere.
    assert_eq!(
        failed["message"],
        format!("POST channels/{channel}/messages: 403 for job:#1")
    );
    assert_eq!(start["span"]["name"], "job:#1");
    let jobs = section_lines(&text, "Jobs");
    assert_eq!(jobs[0]["job"], "job:#1");
    let runs = section_lines(&text, "Runs");
    assert_eq!(runs[0]["scope"], "job:#1");
    assert_eq!(runs[0]["session_id"], start["fields"]["session_id"]);
    assert!(
        text.contains(&format!("lost channel {channel} for agent:#1")),
        "{text}"
    );
}

#[test]
fn sections_follow_the_spec_and_never_include_dropped_fields() {
    let f = fixture();
    let out = run(&f, &ExportRequest::default());
    let text = &out.text;
    let order: Vec<usize> = [
        "## Summary",
        "## Status",
        "## Settings shape",
        "## Jobs",
        "## Crash record",
        "## Runs",
        "## Daemon log",
    ]
    .iter()
    .map(|h| text.find(h).unwrap_or_else(|| panic!("{h} missing")))
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    // Status and Crash record pids are dropped; a Daemon log `pid` field is Structural.
    assert!(
        !text.contains("4242") && !text.contains("4243"),
        "pids are dropped"
    );
    assert!(!text.contains("\"run_log\""));
    let runs = section_lines(text, "Runs");
    assert_eq!(runs.len(), 2, "the run outside the window is left out");
    assert_eq!(runs[0]["has_run_log"], true);
    assert_eq!(runs[1]["name"], "heartbeat", "fixed run-log names are kept");
    assert_eq!(runs[0]["cost_usd"], json!(0.30000000000000004));
    assert_eq!(runs[0]["launcher_resolved"], "path:#1");
    let jobs = section_lines(text, "Jobs");
    assert_eq!(
        jobs[0]["diagnostics"],
        json!([{"severity": "warning", "line": 3, "column": 1, "kind": "key.near_miss"}])
    );
    assert_eq!(jobs[0]["schedule"], "0 9 * * *");
    assert_eq!(out.manifest.runs, 2);
    assert_eq!(out.manifest.lines, 8);
    assert!(
        text.contains("Claude Code 2.1.296 via launcher:#1, seen 2026-10-13"),
        "{text}"
    );
}

#[test]
fn settings_values_never_appear() {
    let f = fixture();
    let text = run(&f, &ExportRequest::default()).text;
    let start = text.find("## Settings shape").unwrap();
    let shape = &text[start..text.find("## Jobs").unwrap()];
    let json_start = shape.find("```json\n").unwrap() + 8;
    let shape: Value =
        serde_json::from_str(&shape[json_start..shape.rfind("```").unwrap()]).unwrap();
    assert_eq!(
        shape,
        json!({
            "model": "opus",
            "api": {"set": true},
            "timezone": "UTC",
            "heartbeat": {"enabled": true, "interval": 15, "prompt": "<set>"},
            "security": {"level": "moderate"},
            "discord": {
                "token": {"set": true},
                "listenChannels": {"len": 1},
                "allowedUserIds": {"len": 1},
            },
            "logging": {"level": "info"},
            "remote": {"enabled": false, "listen": {"len": 1}},
        })
    );
}

#[test]
fn the_same_input_gives_the_same_file_and_another_window_numbers_differently() {
    let f = fixture();
    let a = run(&f, &ExportRequest::default());
    let b = run(&f, &ExportRequest::default());
    assert_eq!(a.text, b.text);
    let map = |since: &str| {
        run(
            &f,
            &ExportRequest {
                since: Some(at(since)),
                map: true,
                ..ExportRequest::default()
            },
        )
        .map
        .unwrap()
    };
    let wide = map("2026-10-13T00:00:00Z");
    let narrow = map("2026-10-13T08:32:30Z");
    // The wide window meets the first peer before the second, the narrow one only the second.
    assert_eq!(wide["ip:#1"], "192.0.2.7");
    assert_eq!(wide["ip:#2"], "198.51.100.9");
    assert_eq!(narrow["ip:#1"], "198.51.100.9");
    assert!(!narrow.contains_key("ip:#2"));
    assert_ne!(wide, narrow);
    assert_eq!(wide["job:#1"], "daily-report");
}

#[test]
fn the_map_is_the_reverse_of_the_text() {
    let f = fixture();
    let out = run(
        &f,
        &ExportRequest {
            map: true,
            ..ExportRequest::default()
        },
    );
    let map = out.map.unwrap();
    assert!(out.manifest.map_written);
    assert_eq!(map["device:#1"], DEVICE);
    assert_eq!(map["session:#1"], SESSION.to_ascii_lowercase());
    for pseudonym in map.keys() {
        assert!(out.text.contains(pseudonym.as_str()), "{pseudonym}");
    }
    let total: u32 = out.manifest.pseudonyms.values().sum();
    assert_eq!(total as usize, map.len());
}

#[test]
fn a_planted_surviving_identifier_fails_with_export_leak() {
    let f = fixture();
    // `boot_id` is Structural, so a Device ID planted there is kept, and the verify pass must
    // catch it.
    let planted = log_line(
        6,
        "2026-10-13T08:40:00Z",
        "INFO",
        "daemon.start",
        "started",
        json!({"boot_id": DEVICE}),
    );
    let path = f.layout.daemon_log("worker", "2026-10-13");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push('\n');
    text.push_str(&planted);
    fs::write(&path, text).unwrap();
    let err = export(&f.layout, &f.input, &ExportRequest::default()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ExportLeak);
    let ExportError::Leak {
        section,
        field,
        kind,
    } = &err
    else {
        panic!("expected a leak, got {err}")
    };
    assert_eq!(
        (*section, field.as_str(), *kind),
        (ExportSection::DaemonLog, "boot_id", "device")
    );
    assert!(!err.to_string().contains(DEVICE), "{err}");
}

#[test]
fn a_planted_snowflake_in_a_structural_string_fails_closed() {
    let f = fixture();
    let planted = log_line(
        6,
        "2026-10-13T08:40:00Z",
        "INFO",
        "daemon.start",
        "started",
        json!({"version": format!("v{}", flake('9'))}),
    );
    let path = f.layout.daemon_log("watchdog", "2026-10-13");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push('\n');
    text.push_str(&planted);
    fs::write(&path, text).unwrap();
    let err = export(&f.layout, &f.input, &ExportRequest::default()).unwrap_err();
    assert!(
        matches!(&err, ExportError::Leak { field, kind: "discord", .. } if field == "version"),
        "{err}"
    );
}

#[test]
fn the_size_cap_trims_debug_then_oldest_info_and_reports_it() {
    let f = fixture();
    let full = run(&f, &ExportRequest::default());
    let request = |max: usize| ExportRequest {
        max_bytes: Some(max as u64),
        ..ExportRequest::default()
    };
    let small = run(&f, &request(full.text.len() - 50));
    assert!(small.text.len() <= full.text.len() - 50);
    let t = &small.manifest.trimmed;
    assert_eq!((t.trace, t.debug), (0, 1), "{t:?}");
    assert_eq!(small.manifest.lines, full.manifest.lines - 1);
    assert!(
        small
            .text
            .contains("Trimmed to fit the size cap: 0 TRACE, 1 DEBUG")
    );
    assert!(!small.text.contains("api.request"));

    let tiny = run(&f, &request(full.text.len() - 400));
    let t = &tiny.manifest.trimmed;
    assert_eq!(t.debug, 1);
    assert!(t.info >= 1, "{t:?}");
    assert!(
        t.oldest_included > full.manifest.trimmed.oldest_included,
        "{t:?} {:?}",
        full.manifest.trimmed
    );
    // Status, Jobs and the Crash record are never trimmed.
    for section in ["## Status", "## Jobs", "## Crash record"] {
        assert!(tiny.text.contains(section));
    }
    assert!(full.manifest.trimmed.oldest_included.is_some());
    assert_eq!(full.manifest.trimmed.debug, 0);
}

#[test]
fn requests_out_of_range_are_refused() {
    let f = fixture();
    let err = export(
        &f.layout,
        &f.input,
        &ExportRequest {
            max_bytes: Some(MAX_BYTES + 1),
            ..ExportRequest::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, ExportError::TooLarge { .. }));
    assert_eq!(err.code(), ErrorCode::BadRequest);
    let err = export(
        &f.layout,
        &f.input,
        &ExportRequest {
            since: Some(f.input.now),
            ..ExportRequest::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, ExportError::EmptyWindow));
}

#[test]
fn an_empty_project_dir_still_exports() {
    let dir = TempDir::new().unwrap();
    let layout = Layout::new(dir.path());
    let f = fixture();
    let input = ExportInput {
        status: None,
        jobs: Vec::new(),
        ..f.input.clone()
    };
    let out = export(&layout, &input, &ExportRequest::default()).unwrap();
    assert!(out.text.contains("There is no `settings.json`."));
    assert!(out.text.contains("There is no `supervisor.json`."));
    assert!(
        out.text
            .contains("Claude Code version: (run claude --version)")
    );
    assert_eq!((out.manifest.lines, out.manifest.runs), (0, 0));
    assert_eq!(out.manifest.trimmed.oldest_included, None);
}

/// Appends Daemon log lines to the fixture's Worker file.
fn append_worker(f: &Fixture, lines: &[String]) {
    let path = f.layout.daemon_log("worker", "2026-10-13");
    let mut text = fs::read_to_string(&path).unwrap();
    for line in lines {
        text.push('\n');
        text.push_str(line);
    }
    fs::write(&path, text).unwrap();
}

/// Every pseudonym in `map` appears in `text`, and each kind is numbered 1..=n with no gap.
fn assert_map_matches_text(out: &LogExport) {
    let map = out.map.as_ref().unwrap();
    for pseudonym in map.keys() {
        assert!(
            out.text.contains(pseudonym.as_str()),
            "{pseudonym} not in the file"
        );
    }
    for (kind, n) in &out.manifest.pseudonyms {
        for i in 1..=*n {
            assert!(
                map.contains_key(&format!("{kind}:#{i}")),
                "{kind}:#{i} missing"
            );
        }
    }
    let total: u32 = out.manifest.pseudonyms.values().sum();
    assert_eq!(total as usize, map.len());
}

#[test]
fn trimmed_lines_take_no_pseudonym_numbers() {
    let f = fixture();
    // The oldest line, and a TRACE one, so it is the first to be trimmed. It is the only place
    // these two identifiers appear.
    let path = f.layout.daemon_log("watchdog", "2026-10-13");
    let mut text = log_line(
        0,
        "2026-10-13T00:00:01Z",
        "TRACE",
        "gateway.event",
        "early",
        json!({"session_id": "trimmed-only-session", "job": "early-job"}),
    );
    text.push('\n');
    text.push_str(&fs::read_to_string(&path).unwrap());
    fs::write(&path, text).unwrap();
    let request = |max: Option<u64>| ExportRequest {
        max_bytes: max,
        map: true,
        ..ExportRequest::default()
    };
    let full = run(&f, &request(None));
    assert!(
        full.map
            .as_ref()
            .unwrap()
            .values()
            .any(|v| v == "early-job")
    );
    let trimmed = run(&f, &request(Some(full.text.len() as u64 - 50)));
    assert_eq!(trimmed.manifest.trimmed.trace, 1);
    let map = trimmed.map.as_ref().unwrap();
    assert!(
        !map.values()
            .any(|v| v == "early-job" || v == "trimmed-only-session"),
        "{map:?}"
    );
    assert_map_matches_text(&trimmed);
    assert_map_matches_text(&full);
}

#[test]
fn a_long_fatal_is_scrubbed_before_it_is_cut() {
    let f = fixture();
    let id = flake('4');
    let mut status: Value =
        serde_json::from_str(&fs::read_to_string(f.layout.supervisor()).unwrap()).unwrap();
    // A raw cut at 300 bytes would fall inside the snowflake and keep its first digits.
    let fatal = format!("{} {id} after-tail-job", "x".repeat(290));
    status["crashes"][0]["fatal"] = json!(fatal);
    fs::write(f.layout.supervisor(), status.to_string()).unwrap();
    let out = run(
        &f,
        &ExportRequest {
            map: true,
            ..ExportRequest::default()
        },
    );
    let summary = &out.text[..out.text.find("## Status").unwrap()];
    let crash_line = summary
        .lines()
        .find(|l| l.starts_with("- Newest crash:"))
        .unwrap();
    assert!(crash_line.ends_with("x […]"), "{crash_line}");
    assert!(!summary.contains("4444"), "{summary}");
    // The snowflake is numbered where it first shows: the Crash record, after discord:#1-#2.
    assert_map_matches_text(&out);
}

#[test]
fn identifiers_in_keys_glued_ids_and_full_stops_are_scrubbed() {
    let f = fixture();
    let unknown = flake('5');
    append_worker(
        &f,
        &[log_line(
            8,
            "2026-10-13T08:36:00Z",
            "WARN",
            "discord.send_failed",
            &format!(
                "Unknown user {unknown}. resume_{} failed",
                SESSION.to_ascii_lowercase()
            ),
            json!({"per_job": {"daily-report": 2, "researcher/digest": 1}}),
        )],
    );
    let out = run(&f, &ExportRequest::default());
    for leaked in [
        unknown.as_str(),
        "daily-report",
        "researcher",
        &SESSION.to_ascii_lowercase(),
    ] {
        assert!(!out.text.contains(leaked), "{leaked} leaked");
    }
    let log = section_lines(&out.text, "Daemon log");
    let line = log.iter().find(|l| l["seq"] == 8).unwrap();
    assert_eq!(
        line["message"],
        "Unknown user discord:#3. resume_session:#1 failed"
    );
    let keys: Vec<&String> = line["fields"]["per_job"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(keys, ["job:#1", "job:#2"]);
}

#[test]
fn planted_glued_or_dotted_ids_fail_closed() {
    for (field, value, kind) in [
        ("boot_id", format!("resume_{SESSION}"), "session"),
        ("version", format!("v{}.", flake('9')), "discord"),
    ] {
        let f = fixture();
        append_worker(
            &f,
            &[log_line(
                8,
                "2026-10-13T08:40:00Z",
                "INFO",
                "daemon.start",
                "started",
                json!({ field: value }),
            )],
        );
        let err = export(&f.layout, &f.input, &ExportRequest::default()).unwrap_err();
        assert!(
            matches!(&err, ExportError::Leak { field: f, kind: k, .. } if f == field && *k == kind),
            "{field}: {err}"
        );
    }
}

#[test]
fn a_job_name_as_a_key_inside_a_structural_field_is_scrubbed() {
    // `deleted` is Structural, so its value is kept as is; its keys still get the free-text
    // pass, and the verify pass checks them for short names (see `verify::tests`).
    let f = fixture();
    append_worker(
        &f,
        &[log_line(
            8,
            "2026-10-13T08:40:00Z",
            "INFO",
            "housekeeping.done",
            "done",
            json!({"deleted": {"daily-report": 1, "inbox": 2}}),
        )],
    );
    let out = run(&f, &ExportRequest::default());
    assert!(
        out.text.contains("\"deleted\":{\"job:#1\":1,\"inbox\":2}"),
        "{}",
        out.text
    );
}

#[test]
fn the_summary_stays_under_two_kib() {
    let mut f = fixture();
    let long = "v".repeat(500);
    f.input.versions.claudeclaw = long.clone();
    f.input.versions.commit = long.clone();
    f.input.versions.target = long.clone();
    f.input.versions.daemon = Some(long.clone());
    f.input.versions.app = Some(long.clone());
    f.input.install_method = Some(long.clone());
    f.input.os.version = Some(long.clone());
    f.input.claude_code = (0..10)
        .map(|i| ClaudeCodeRow {
            launcher: format!("/opt/l{i}.sh"),
            seen: serde_json::from_value(json!({
                "version": long, "seen_at": "2026-10-13T00:00:00Z", "outdated": false,
            }))
            .unwrap(),
        })
        .collect();
    let out = run(&f, &ExportRequest::default());
    let start = out.text.find("## Summary\n\n").unwrap() + 12;
    let summary = &out.text[start..out.text.find("\n\n## Status").unwrap()];
    assert!(summary.len() <= SUMMARY_MAX_BYTES, "{}", summary.len());
    assert!(summary.contains("via launcher:#1"), "{summary}");
}

#[test]
fn a_home_path_with_spaces_is_scrubbed_whole_and_project_paths_are_kept() {
    let f = fixture();
    let home = f._dir.path().to_string_lossy().into_owned();
    // The project dir as the OS spells it (`<home>\proj` on Windows).
    let project = f.layout.project_dir().to_string_lossy().into_owned();
    append_worker(
        &f,
        &[log_line(
            8,
            "2026-10-13T08:40:00Z",
            "WARN",
            "job.invalid",
            &format!(
                "open {home}/Mobile Documents/AcmeCorp/plan.md was denied; \
                 see {project}/agents/zed/jobs/y.md"
            ),
            json!({}),
        )],
    );
    let out = run(&f, &ExportRequest::default());
    let log = section_lines(&out.text, "Daemon log");
    let line = log.iter().find(|l| l["seq"] == 8).unwrap();
    let message = line["message"].as_str().unwrap();
    assert!(!out.text.contains("AcmeCorp"), "{message}");
    assert!(
        message.ends_with("was denied; see <project-dir>/agents/zed/jobs/y.md"),
        "{message}"
    );
}

#[test]
fn snowflakes_stored_as_numbers_never_appear() {
    let f = fixture();
    let id = flake('6');
    let n: Value = serde_json::from_str(&id).unwrap();
    append_worker(
        &f,
        &[log_line(
            8,
            "2026-10-13T08:40:00Z",
            "INFO",
            "discord.thread",
            "thread",
            json!({"thread_id": n, "ids": [n]}),
        )],
    );
    let out = run(&f, &ExportRequest::default());
    assert!(!out.text.contains(&id), "{}", out.text);
    let log = section_lines(&out.text, "Daemon log");
    let line = log.iter().find(|l| l["seq"] == 8).unwrap();
    assert_eq!(line["fields"]["thread_id"], line["fields"]["ids"][0]);
    assert!(
        line["fields"]["thread_id"]
            .as_str()
            .unwrap()
            .starts_with("discord:#")
    );
}

#[test]
fn crash_record_numbers_follow_field_classes() {
    // `uptime_ms` is Structural, so even a snowflake-sized value there is kept and passes
    // verify; an unclassified number in a parsed `stderr_tail` line is pseudonymised.
    let f = fixture();
    let id = flake('6');
    let mut status: Value =
        serde_json::from_str(&fs::read_to_string(f.layout.supervisor()).unwrap()).unwrap();
    status["crashes"][0]["uptime_ms"] = serde_json::from_str(&id).unwrap();
    status["crashes"][0]["stderr_tail"] = json!([log_line(
        9,
        "2026-10-13T08:34:29Z",
        "WARN",
        "x",
        "y",
        json!({"thread_id": serde_json::from_str::<Value>(&id).unwrap()}),
    )]);
    fs::write(f.layout.supervisor(), status.to_string()).unwrap();
    let out = run(&f, &ExportRequest::default());
    let tail_start = out.text.find("stderr_tail").unwrap();
    assert!(out.text[..tail_start].contains(&format!("\"uptime_ms\": {id}")));
    assert!(!out.text[tail_start..].contains(&id));
}

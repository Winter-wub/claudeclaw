//! In-process fakes for the scopes and sessions seams (management-api §2.5), and the samples they
//! serve. The sessions fake starts its compact Turns on the shared [`FakeTurns`], so a test sees
//! the same Turn bus `turns.create` writes to.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Shared};

use claudeclaw_api::error::{ApiError, ErrorCode};
use claudeclaw_api::runs::RunOutcome;
use claudeclaw_api::scope::Scope;
use claudeclaw_api::scopes::{
    ClaudeCodeSeen, InheritedFrom, LauncherCheck, LauncherCheckId, LauncherInfo, LauncherOverview,
    LauncherSetResult, LauncherSource, LauncherTestRequest, LauncherTestResult, LauncherUnset,
    NotRunnable, Runnable, ScopeInfo, ScopeKind, ScopeLauncher, ScopeList, ScopeQueue,
    SessionStart,
};
use claudeclaw_api::sessions::{
    AutoCompactLast, AutoCompactState, ScopeSession, SessionContext, SessionDeleted, SessionDetail,
    SessionList, SessionRecord, SessionReset, SessionResetRequest, SessionStatus,
    TimeoutBreakerState, TranscriptMessage, TranscriptPage, TranscriptRole,
};
use claudeclaw_api::turns::{SessionCompacted, TurnPurpose};

use super::{FakeTurns, at};
use crate::backend::{
    BackendFuture, ScopeListQuery, ScopesBackend, SessionListQuery, SessionsBackend,
    TranscriptQuery, TurnCreate, TurnHandle, TurnsBackend,
};

/// A Launcher path the scopes fake knows how to run.
pub const WORK_LAUNCHER: &str = "/opt/launchers/work";
/// A Launcher path the scopes fake refuses to set (`422 launcher_not_runnable`).
pub const BROKEN_LAUNCHER: &str = "/opt/launchers/broken";

fn launcher_row(scope: Scope, set_here: Option<&str>, inherited: InheritedFrom) -> ScopeLauncher {
    ScopeLauncher {
        scope,
        set_here: set_here.map(str::to_owned),
        effective: set_here.unwrap_or("direct").to_owned(),
        inherited_from: inherited,
        source: LauncherSource::Settings,
    }
}

/// `scopes.list`'s sample: the `global` Scope, a channel and one of its threads.
pub fn sample_scope_list() -> ScopeList {
    let channel = Scope::Discord("1234".into());
    ScopeList {
        scopes: vec![
            ScopeInfo {
                scope: Scope::Global,
                kind: ScopeKind::Global,
                label: "global".into(),
                parent: None,
                launcher: launcher_row(Scope::Global, Some("direct"), InheritedFrom::Default),
                session: Some(ScopeSession {
                    launcher: "direct".into(),
                    session_id: "s-1".into(),
                    status: SessionStatus::InUse,
                }),
                queue: ScopeQueue {
                    running: false,
                    depth: 0,
                },
            },
            ScopeInfo {
                scope: channel.clone(),
                kind: ScopeKind::Channel,
                label: "general".into(),
                parent: None,
                launcher: launcher_row(channel.clone(), None, InheritedFrom::Scope(Scope::Global)),
                session: None,
                queue: ScopeQueue {
                    running: true,
                    depth: 2,
                },
            },
            ScopeInfo {
                scope: Scope::Discord("5678".into()),
                kind: ScopeKind::Thread,
                label: "research".into(),
                parent: Some(channel.clone()),
                launcher: launcher_row(
                    Scope::Discord("5678".into()),
                    None,
                    InheritedFrom::Scope(Scope::Global),
                ),
                session: None,
                queue: ScopeQueue {
                    running: false,
                    depth: 0,
                },
            },
        ],
    }
}

/// `launchers.list`'s sample: `direct` plus one Launcher path in use.
pub fn sample_launcher_overview() -> LauncherOverview {
    LauncherOverview {
        default: "direct".into(),
        picker: vec!["direct".into(), WORK_LAUNCHER.into()],
        launchers: vec![
            LauncherInfo {
                launcher: "direct".into(),
                builtin: true,
                runnable: Runnable {
                    ok: true,
                    reason: None,
                },
                last_test: None,
                used_by: vec![],
                claude_code: None,
            },
            LauncherInfo {
                launcher: WORK_LAUNCHER.into(),
                builtin: false,
                runnable: Runnable {
                    ok: false,
                    reason: Some(NotRunnable::Missing),
                },
                last_test: None,
                used_by: vec![],
                claude_code: None,
            },
        ],
        scopes: vec![launcher_row(
            Scope::Global,
            Some("direct"),
            InheritedFrom::Default,
        )],
    }
}

/// A passing Test launcher result for `launcher`.
pub fn sample_test_result(launcher: &str, scope: Scope) -> LauncherTestResult {
    LauncherTestResult {
        launcher: launcher.to_owned(),
        scope,
        ok: true,
        checks: vec![LauncherCheck {
            id: LauncherCheckId::Runnable,
            ok: true,
            advisory: false,
            detail: "found on the Run PATH".into(),
        }],
        claude_code: Some(ClaudeCodeSeen {
            version: "2.1.296".into(),
            seen_at: at("2026-10-14T09:12:00Z"),
            outdated: false,
        }),
        duration_ms: 8123,
        tested_at: at("2026-10-14T09:12:00Z"),
    }
}

/// What `sessions.delete` was given: the Scope and the optional `?launcher=`.
type DeleteCall = (Scope, Option<String>);

type SharedTest = Shared<BoxFuture<'static, Result<LauncherTestResult, ApiError>>>;

/// The scopes seam: it serves the samples, records what the routes asked for, refuses a Launcher
/// path it cannot run, and joins a test request onto the test already running for its Launcher
/// (§2.5, #40) the way the real seam must.
#[derive(Clone, Default)]
pub struct FakeScopes {
    list_calls: Arc<Mutex<Vec<ScopeListQuery>>>,
    set_calls: Arc<Mutex<Vec<(Scope, String)>>>,
    unset_calls: Arc<Mutex<Vec<Scope>>>,
    test_calls: Arc<Mutex<Vec<LauncherTestRequest>>>,
    test_runs: Arc<Mutex<u32>>,
    running: Arc<Mutex<BTreeMap<String, SharedTest>>>,
    test_delay: Arc<Mutex<Option<Duration>>>,
}

impl FakeScopes {
    /// The queries `scopes.list` served.
    pub fn list_calls(&self) -> Vec<ScopeListQuery> {
        self.list_calls.lock().unwrap().clone()
    }

    /// The `(scope, launcher)` pairs `launchers.set` was given.
    pub fn set_calls(&self) -> Vec<(Scope, String)> {
        self.set_calls.lock().unwrap().clone()
    }

    /// The Scopes `launchers.unset` was given.
    pub fn unset_calls(&self) -> Vec<Scope> {
        self.unset_calls.lock().unwrap().clone()
    }

    /// Every request `launchers.test` reached the seam with, joined or not.
    pub fn test_calls(&self) -> Vec<LauncherTestRequest> {
        self.test_calls.lock().unwrap().clone()
    }

    /// How many tests actually ran: a request that joined a running test does not count.
    pub fn test_runs(&self) -> u32 {
        *self.test_runs.lock().unwrap()
    }

    /// How long a test takes to run.
    pub fn set_test_delay(&self, delay: Duration) {
        *self.test_delay.lock().unwrap() = Some(delay);
    }
}

impl ScopesBackend for FakeScopes {
    fn list(&self, query: ScopeListQuery) -> BackendFuture<'_, ScopeList> {
        self.list_calls.lock().unwrap().push(query);
        let mut list = sample_scope_list();
        if let Some(kind) = query.kind {
            list.scopes.retain(|info| info.kind == kind);
        }
        Box::pin(async move { Ok(list) })
    }

    fn launchers(&self) -> BackendFuture<'_, LauncherOverview> {
        Box::pin(async { Ok(sample_launcher_overview()) })
    }

    fn set_launcher(&self, scope: Scope, launcher: String) -> BackendFuture<'_, LauncherSetResult> {
        self.set_calls
            .lock()
            .unwrap()
            .push((scope.clone(), launcher.clone()));
        Box::pin(async move {
            if launcher == BROKEN_LAUNCHER {
                return Err(ApiError::new(
                    ErrorCode::LauncherNotRunnable,
                    "the Launcher cannot be run",
                )
                .with_detail("reason", "not_executable"));
            }
            Ok(LauncherSetResult {
                scope,
                effective: launcher.clone(),
                launcher,
                session: SessionStart::New,
            })
        })
    }

    fn unset_launcher(&self, scope: Scope) -> BackendFuture<'_, LauncherUnset> {
        self.unset_calls.lock().unwrap().push(scope.clone());
        Box::pin(async move {
            Ok(LauncherUnset {
                scope,
                effective: "direct".into(),
                inherited_from: InheritedFrom::Default,
            })
        })
    }

    fn test_launcher(&self, request: LauncherTestRequest) -> BackendFuture<'_, LauncherTestResult> {
        self.test_calls.lock().unwrap().push(request.clone());
        let launcher = request.launcher.clone().unwrap_or_else(|| "direct".into());
        let delay = *self.test_delay.lock().unwrap();
        let mut running = self.running.lock().unwrap();
        // A second request for a Launcher under test joins the running test (#40).
        let shared = match running.get(&launcher) {
            Some(shared) => shared.clone(),
            None => {
                *self.test_runs.lock().unwrap() += 1;
                let table = self.running.clone();
                let key = launcher.clone();
                let scope = request.scope.clone();
                let fresh: BoxFuture<'static, Result<LauncherTestResult, ApiError>> =
                    Box::pin(async move {
                        if let Some(delay) = delay {
                            tokio::time::sleep(delay).await;
                        }
                        table.lock().unwrap().remove(&key);
                        Ok(sample_test_result(&key, scope))
                    });
                let shared = fresh.shared();
                running.insert(launcher, shared.clone());
                shared
            }
        };
        drop(running);
        Box::pin(shared)
    }
}

/// The sessions the sessions fake holds: the `global` Scope's `in_use` one and a `parked` one.
pub fn sample_session(session_id: &str, status: SessionStatus) -> SessionRecord {
    SessionRecord {
        scope: Scope::Global,
        scope_label: "global".into(),
        launcher: if status == SessionStatus::InUse {
            "direct".into()
        } else {
            WORK_LAUNCHER.into()
        },
        session_id: session_id.to_owned(),
        status,
        created_at: at("2026-10-14T08:00:00Z"),
        last_used_at: at("2026-10-14T09:00:00Z"),
        turns: 12,
        first_message: Some("first words".into()),
        prune_at: (status != SessionStatus::InUse).then(|| at("2026-11-13T09:00:00Z")),
        timeout_breaker: TimeoutBreakerState {
            consecutive_timeouts: 0,
            failing_since: None,
            open: false,
            opened_at: None,
        },
        auto_compact: AutoCompactState {
            spent_at: None,
            last: Some(AutoCompactLast {
                at: at("2026-10-14T08:30:00Z"),
                outcome: RunOutcome::Ok,
                duration_ms: 4000,
            }),
        },
    }
}

/// The session whose Transcript the sessions fake can read.
pub const READABLE: &str = "s-1";
/// The session whose Transcript the sessions fake cannot locate.
pub const UNREADABLE: &str = "s-parked";
/// How many messages the readable Transcript has.
pub const TRANSCRIPT_LEN: usize = 5;

fn transcript_message(index: usize) -> TranscriptMessage {
    TranscriptMessage {
        uuid: format!("m-{index}"),
        role: if index.is_multiple_of(2) {
            TranscriptRole::User
        } else {
            TranscriptRole::Assistant
        },
        text: format!("message {index}"),
        timestamp: at("2026-10-14T09:00:00Z") + chrono::Duration::minutes(index as i64),
        tools: vec![],
    }
}

/// The sessions seam: it serves a `global` session in use and a parked one, records what the
/// routes asked for, pages the Transcript newest first with a numeric cursor, and refuses to
/// delete the one in use (§2.5).
#[derive(Clone)]
pub struct FakeSessions {
    list_calls: Arc<Mutex<Vec<SessionListQuery>>>,
    get_calls: Arc<Mutex<Vec<String>>>,
    message_calls: Arc<Mutex<Vec<(String, TranscriptQuery)>>>,
    reset_calls: Arc<Mutex<Vec<(Scope, SessionResetRequest)>>>,
    compact_calls: Arc<Mutex<Vec<(Scope, String)>>>,
    delete_calls: Arc<Mutex<Vec<DeleteCall>>>,
    sessions: Arc<Mutex<Vec<SessionRecord>>>,
    pub turns: FakeTurns,
}

impl FakeSessions {
    /// A sessions fake whose `compact` starts its Turns on the given turns fake (§2.5).
    pub fn new(turns: FakeTurns) -> Self {
        FakeSessions {
            list_calls: Arc::default(),
            get_calls: Arc::default(),
            message_calls: Arc::default(),
            reset_calls: Arc::default(),
            compact_calls: Arc::default(),
            delete_calls: Arc::default(),
            sessions: Arc::new(Mutex::new(vec![
                sample_session(READABLE, SessionStatus::InUse),
                sample_session(UNREADABLE, SessionStatus::Parked),
            ])),
            turns,
        }
    }

    /// The queries `sessions.list` served.
    pub fn list_calls(&self) -> Vec<SessionListQuery> {
        self.list_calls.lock().unwrap().clone()
    }

    /// The ids `sessions.get` served.
    pub fn get_calls(&self) -> Vec<String> {
        self.get_calls.lock().unwrap().clone()
    }

    /// The `(session, query)` pairs `sessions.messages` served.
    pub fn message_calls(&self) -> Vec<(String, TranscriptQuery)> {
        self.message_calls.lock().unwrap().clone()
    }

    /// The `(scope, request)` pairs `sessions.reset` was given.
    pub fn reset_calls(&self) -> Vec<(Scope, SessionResetRequest)> {
        self.reset_calls.lock().unwrap().clone()
    }

    /// The `(scope, principal)` pairs `sessions.compact` started Turns for.
    pub fn compact_calls(&self) -> Vec<(Scope, String)> {
        self.compact_calls.lock().unwrap().clone()
    }

    /// The `(scope, launcher)` pairs `sessions.delete` was given.
    pub fn delete_calls(&self) -> Vec<DeleteCall> {
        self.delete_calls.lock().unwrap().clone()
    }

    fn find(&self, session_id: &str) -> Option<SessionRecord> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|record| record.session_id == session_id)
            .cloned()
    }
}

impl SessionsBackend for FakeSessions {
    fn list(&self, query: SessionListQuery) -> BackendFuture<'_, SessionList> {
        self.list_calls.lock().unwrap().push(query.clone());
        let sessions = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|record| query.scope.as_ref().is_none_or(|s| *s == record.scope))
            .filter(|record| query.status.is_none_or(|status| status == record.status))
            .cloned()
            .collect();
        Box::pin(async move { Ok(SessionList { sessions }) })
    }

    fn get(&self, session_id: &str) -> BackendFuture<'_, SessionDetail> {
        self.get_calls.lock().unwrap().push(session_id.to_owned());
        let found = self.find(session_id);
        Box::pin(async move {
            let record =
                found.ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such session"))?;
            let context = (record.status == SessionStatus::InUse).then_some(SessionContext {
                used_tokens: 42_000,
                window: 200_000,
            });
            Ok(SessionDetail { record, context })
        })
    }

    fn messages(
        &self,
        session_id: &str,
        query: TranscriptQuery,
    ) -> BackendFuture<'_, TranscriptPage> {
        self.message_calls
            .lock()
            .unwrap()
            .push((session_id.to_owned(), query.clone()));
        let known = self.find(session_id).is_some();
        let readable = session_id == READABLE;
        Box::pin(async move {
            if !known {
                return Err(ApiError::new(ErrorCode::NotFound, "no such session"));
            }
            if !readable {
                // Never an empty list for a Transcript that cannot be read (#39).
                return Err(ApiError::new(
                    ErrorCode::TranscriptNotFound,
                    "the Transcript was not found",
                )
                .with_hint("check the transcript_roots setting")
                .with_detail("reason", "not_located")
                .with_detail("searched_roots", Vec::<String>::new())
                .with_detail("retention_days", 30));
            }
            // Newest first: the cursor is the index of the next older message.
            let start = match &query.before {
                None => TRANSCRIPT_LEN,
                Some(cursor) => cursor
                    .strip_prefix("c")
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|n| *n <= TRANSCRIPT_LEN)
                    .ok_or_else(|| {
                        ApiError::new(
                            ErrorCode::InvalidCursor,
                            "not a cursor this route handed out",
                        )
                    })?,
            };
            let end = start.saturating_sub(query.limit as usize);
            let messages = (end..start).rev().map(transcript_message).collect();
            Ok(TranscriptPage {
                messages,
                next: (end > 0).then(|| format!("c{end}")),
            })
        })
    }

    fn reset(&self, scope: Scope, request: SessionResetRequest) -> BackendFuture<'_, SessionReset> {
        self.reset_calls
            .lock()
            .unwrap()
            .push((scope.clone(), request.clone()));
        Box::pin(async move {
            Ok(SessionReset {
                scope,
                launcher: request.launcher.unwrap_or_else(|| "direct".into()),
                backup: Some("sessions/backups/s-1".into()),
            })
        })
    }

    fn compact(&self, scope: Scope, principal: String) -> BackendFuture<'_, TurnHandle> {
        self.compact_calls
            .lock()
            .unwrap()
            .push((scope.clone(), principal.clone()));
        let turns = self.turns.clone();
        Box::pin(async move {
            let handle = turns
                .create(TurnCreate {
                    scope: scope.clone(),
                    text: "/compact".into(),
                    attachments: Vec::new(),
                    notify: false,
                    principal,
                })
                .await?;
            // The Turn is a compact: its record says so, and finishing keeps that (§2.3).
            if let Some(live) = turns.turns.lock().unwrap().get(&handle.turn_id) {
                live.record.send_modify(|record| {
                    record.purpose = TurnPurpose::Compact;
                    record.compact = Some(SessionCompacted {
                        scope: scope.clone(),
                        launcher: "direct".into(),
                        ok: true,
                        context_before: Some(180_000),
                        context_after: Some(20_000),
                    });
                });
            }
            Ok(handle)
        })
    }

    fn delete(&self, scope: Scope, launcher: Option<String>) -> BackendFuture<'_, SessionDeleted> {
        self.delete_calls
            .lock()
            .unwrap()
            .push((scope.clone(), launcher.clone()));
        let in_use = self.sessions.lock().unwrap().iter().any(|record| {
            record.scope == scope
                && record.status == SessionStatus::InUse
                && launcher.as_deref().is_none_or(|l| l == record.launcher)
        });
        Box::pin(async move {
            if in_use {
                return Err(ApiError::new(
                    ErrorCode::SessionInUse,
                    "the session is in use; reset it instead",
                ));
            }
            Ok(SessionDeleted { deleted: true })
        })
    }
}

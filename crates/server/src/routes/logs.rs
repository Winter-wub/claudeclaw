//! The Daemon-log handlers (management-api §2.9): `logs.tail`, `logs.stream`, `logs.summary`
//! and the Bug-report `logs.export`.

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::{Stream, StreamExt};
use tokio::sync::broadcast;

use claudeclaw_api::events::{GapReason, LogStreamHello, StreamGap};
use claudeclaw_api::export::LogExport;
use claudeclaw_api::logs::{LogPage, LogSummary};

use crate::backend::logs::{
    LogCursor, LogExportQuery, LogSource, LogStream, LogStreamQuery, LogTailQuery, SequencedLog,
};
use crate::error::ApiFailure;
use crate::routes::Params;
use crate::state::AppState;

/// `GET /v1/logs/{source}`: one page, newest first (§2.9). The merged `daemon` source is
/// resolved server-side, so no client has to merge two cursor-based feeds (#23 D13).
pub async fn tail(
    State(state): State<AppState>,
    Path(source): Path<String>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<LogPage>, ApiFailure> {
    let source = source_of(&source)?;
    let params = Params::new(&pairs);
    let query = LogTailQuery {
        before: match params.get("before") {
            Some(raw) => Some(cursor_of(source, raw)?),
            None => None,
        },
        limit: params.page_limit()?,
        min_level: params.level("min_level")?,
        event: params.get("event").map(str::to_owned),
        scope: params.scope("scope")?,
        run_id: params.get("run_id").map(str::to_owned),
        q: params.get("q").map(str::to_owned),
    };
    state
        .logs
        .tail(source, query)
        .await
        .map(Json)
        .map_err(Into::into)
}

/// `GET /v1/logs/{source}/stream`: SSE (§2.9, §1.6). It opens with `hello{boot_id, last_seq}`,
/// carries every line as `log` with `id: <boot_id>:<seq>`, resumes from `Last-Event-ID`, and
/// reports lost lines as `gap` rather than skipping them silently.
pub async fn stream(
    State(state): State<AppState>,
    Path(source): Path<String>,
    Query(pairs): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> Result<Response, ApiFailure> {
    let source = source_of(&source)?;
    let params = Params::new(&pairs);
    let resume = resume_header(&headers);
    let query = LogStreamQuery {
        backfill: params.count("backfill")?.unwrap_or_default(),
        min_level: params.level("min_level")?,
        event: params.get("event").map(str::to_owned),
        scope: params.scope("scope")?,
        run_id: params.get("run_id").map(str::to_owned),
        resume: match &resume {
            Resume::At { seq, .. } => Some(*seq),
            Resume::None | Resume::Unusable => None,
        },
    };
    let stream = state.logs.stream(source, query).await?;
    let boot_id = stream.hello.boot_id.clone();
    let frames = frames(stream, resume)
        .map(move |frame| Ok::<Event, Infallible>(frame.into_event(&boot_id)));
    let sse = Sse::new(frames).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    );
    Ok(sse.into_response())
}

/// `GET /v1/logs/summary`: the badges behind the Logs and Status screens (§2.9).
pub async fn summary(State(state): State<AppState>) -> Result<Json<LogSummary>, ApiFailure> {
    state.logs.summary().await.map(Json).map_err(Into::into)
}

/// `GET /v1/logs/export`: the Bug-report export (§2.9, #28). Never audited: it is a `GET` that
/// mutates nothing, and the export holds strictly less than `logs:read` already returns.
pub async fn export(
    State(state): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Result<Json<LogExport>, ApiFailure> {
    let params = Params::new(&pairs);
    let query = LogExportQuery {
        since: params.timestamp("since")?,
        until: params.timestamp("until")?,
        min_level: params.level("level")?,
        max_bytes: params.bytes("max_bytes")?,
        map: params.flag("map")?,
        app_version: params.get("app_version").map(str::to_owned),
    };
    state.logs.export(query).await.map(Json).map_err(Into::into)
}

/// What `Last-Event-ID` asked for (§1.6, §2.9): which boot to resume in, and after which `seq`.
#[derive(Debug, Clone, PartialEq)]
pub enum Resume {
    /// The header was absent: subscribe live, replay only what `?backfill=` asks for.
    None,
    /// `<boot_id>:<seq>`, parseable.
    At { boot_id: String, seq: u64 },
    /// Sent, but not a `<boot_id>:<seq>` this server could use. The stream still says so with a
    /// `gap` frame (§1.6: never skip silently).
    Unusable,
}

/// The header an SSE client sends to resume, spelled out: `http` has no constant for it.
const LAST_EVENT_ID: &str = "last-event-id";

/// The `Last-Event-ID` header of a request.
pub fn resume_header(headers: &HeaderMap) -> Resume {
    let Some(value) = headers.get(LAST_EVENT_ID).and_then(|v| v.to_str().ok()) else {
        return Resume::None;
    };
    if value.is_empty() {
        return Resume::None;
    }
    match value.split_once(':') {
        Some((boot_id, seq)) => match seq.parse() {
            Ok(seq) => Resume::At {
                boot_id: boot_id.to_owned(),
                seq,
            },
            Err(_) => Resume::Unusable,
        },
        None => Resume::Unusable,
    }
}

/// One thing the stream sends. Modelling the frames apart from axum's [`Event`] keeps the id,
/// ordering and gap behaviour testable: `Event` gives no way to read its bytes back.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// Always first: `event: hello` with the `boot_id` and the ring's newest `seq` (§1.6).
    Hello(LogStreamHello),
    /// `event: log` with one line, `id: <boot_id>:<seq>`.
    Log(SequencedLog),
    /// `event: gap`, sent instead of skipping lines silently (§1.6).
    Gap(StreamGap),
}

impl Frame {
    /// The SSE event name.
    pub fn name(&self) -> &'static str {
        match self {
            Frame::Hello(_) => "hello",
            Frame::Log(_) => "log",
            Frame::Gap(_) => "gap",
        }
    }

    /// Renders the frame as an axum SSE [`Event`]. Only `log` frames carry an id: it is the
    /// `<boot_id>:<seq>` a client resumes with.
    pub fn into_event(self, boot_id: &str) -> Event {
        match self {
            Frame::Hello(hello) => json_event("hello", &hello, None),
            Frame::Log(SequencedLog { seq, entry }) => {
                json_event("log", &entry, Some(format!("{boot_id}:{seq}")))
            }
            Frame::Gap(gap) => json_event("gap", &gap, None),
        }
    }
}

fn json_event<T: serde::Serialize>(name: &'static str, data: &T, id: Option<String>) -> Event {
    let mut event = Event::default()
        .event(name)
        .json_data(data)
        .unwrap_or_else(|_| Event::default().event(name));
    if let Some(id) = id {
        event = event.id(id);
    }
    event
}

/// The frame stream: `hello`, the resume `gap` when the resume point is gone, the replayed
/// lines, then the live feed — with a `gap` frame whenever a slow client falls behind the ring
/// (§1.6, §2.9).
pub fn frames(stream: LogStream, resume: Resume) -> impl Stream<Item = Frame> {
    let boot_id = stream.hello.boot_id.clone();
    let mut leading = vec![Frame::Hello(stream.hello)];
    match resume {
        Resume::None => {}
        Resume::Unusable => leading.push(Frame::Gap(StreamGap {
            reason: GapReason::Evicted,
            missed: None,
        })),
        Resume::At {
            boot_id: resumed, ..
        } => {
            if resumed != boot_id {
                // Another boot's `Last-Event-ID`: the client re-fetches and starts over (§1.6).
                leading.push(Frame::Gap(StreamGap {
                    reason: GapReason::Restart,
                    missed: None,
                }));
            } else if let Some(gap) = stream.gap {
                leading.push(Frame::Gap(gap));
            }
        }
    }
    leading.extend(stream.backfill.into_iter().map(Frame::Log));
    futures_util::stream::iter(leading).chain(live(stream.receiver))
}

/// The live tail: one `log` frame per line, and a `gap` frame whenever the receiver lagged
/// behind the ring, which it reports rather than silently dropping.
fn live(receiver: tokio::sync::broadcast::Receiver<SequencedLog>) -> impl Stream<Item = Frame> {
    futures_util::stream::unfold(receiver, |mut receiver| async move {
        match receiver.recv().await {
            Ok(entry) => Some((Frame::Log(entry), receiver)),
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                let frame = Frame::Gap(StreamGap {
                    reason: GapReason::Lagged,
                    missed: Some(missed.min(u64::from(u32::MAX)) as u32),
                });
                Some((frame, receiver))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    })
}

/// The `{source}` path segment (§2.9): anything but `worker`, `watchdog` or `daemon` is
/// `400 bad_request`, never a scope or a run (§1.4).
fn source_of(raw: &str) -> Result<LogSource, ApiFailure> {
    LogSource::from_wire(raw).ok_or_else(|| {
        crate::routes::bad_request("unknown log source; expected worker, watchdog or daemon")
    })
}

/// The `?before=` cursor, checked against the source that would have handed it out (§1.6).
fn cursor_of(source: LogSource, raw: &str) -> Result<LogCursor, ApiFailure> {
    LogCursor::parse(source, raw).ok_or_else(crate::routes::invalid_cursor)
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::error::ErrorCode;

    use futures_util::FutureExt;

    use super::*;

    fn line(seq: u64) -> SequencedLog {
        let text = format!(
            r#"{{"seq":{seq},"timestamp":"2026-10-13T08:30:27Z","level":"INFO","target":"t","message":"m"}}"#
        );
        SequencedLog {
            seq,
            entry: serde_json::from_str(&text).unwrap(),
        }
    }

    fn stream_with(
        backfill: Vec<SequencedLog>,
        gap: Option<StreamGap>,
    ) -> (LogStream, broadcast::Sender<SequencedLog>) {
        let (sender, receiver) = broadcast::channel(4);
        (
            LogStream {
                hello: LogStreamHello {
                    boot_id: "boot-1".into(),
                    last_seq: 7,
                },
                backfill,
                receiver,
                gap,
            },
            sender,
        )
    }

    /// The frames a stream produces until it pends.
    fn drain(stream: LogStream, resume: Resume) -> Vec<Frame> {
        let mut stream = Box::pin(frames(stream, resume));
        let mut out = Vec::new();
        while let Some(Some(frame)) = stream.next().now_or_never() {
            out.push(frame);
        }
        out
    }

    /// The frames a stream produces until it pends, by name.
    fn names(stream: LogStream, resume: Resume) -> Vec<&'static str> {
        drain(stream, resume).iter().map(Frame::name).collect()
    }

    #[test]
    fn a_stream_opens_with_hello_then_the_replay_then_the_live_feed() {
        let (stream, _sender) = stream_with(vec![line(5), line(6)], None);
        assert_eq!(
            names(stream, Resume::None),
            ["hello", "log", "log"],
            "no resume, no gap"
        );
    }

    #[test]
    fn a_resume_from_another_boot_is_a_restart_gap() {
        let (stream, _sender) = stream_with(vec![], None);
        let frames = drain(
            stream,
            Resume::At {
                boot_id: "boot-0".into(),
                seq: 3,
            },
        );
        assert!(matches!(frames[0], Frame::Hello(_)));
        assert_eq!(
            frames[1],
            Frame::Gap(StreamGap {
                reason: GapReason::Restart,
                missed: None
            })
        );
        assert_eq!(frames.len(), 2, "nothing is replayed across a restart");
    }

    #[test]
    fn an_evicted_resume_point_is_a_gap_before_the_replay() {
        let (stream, _sender) = stream_with(
            vec![line(6)],
            Some(StreamGap {
                reason: GapReason::Evicted,
                missed: None,
            }),
        );
        let frames = drain(
            stream,
            Resume::At {
                boot_id: "boot-1".into(),
                seq: 2,
            },
        );
        assert!(matches!(frames[1], Frame::Gap(_)), "{frames:?}");
        assert!(matches!(frames[2], Frame::Log(_)));
    }

    #[test]
    fn a_same_boot_resume_without_a_gap_is_pure_replay() {
        let (stream, _sender) = stream_with(vec![line(4)], None);
        assert_eq!(
            names(
                stream,
                Resume::At {
                    boot_id: "boot-1".into(),
                    seq: 3
                }
            ),
            ["hello", "log"]
        );
    }

    #[tokio::test]
    async fn a_lagging_client_gets_a_gap_frame() {
        let (sender, receiver) = broadcast::channel(2);
        let stream = LogStream {
            hello: LogStreamHello {
                boot_id: "boot-1".into(),
                last_seq: 6,
            },
            backfill: vec![],
            receiver,
            gap: None,
        };
        let mut frames = Box::pin(frames(stream, Resume::None));
        assert!(matches!(frames.next().await, Some(Frame::Hello(_))));
        for seq in 0..6 {
            sender.send(line(seq)).unwrap();
        }
        assert_eq!(
            frames.next().await,
            Some(Frame::Gap(StreamGap {
                reason: GapReason::Lagged,
                missed: Some(4)
            }))
        );
    }

    #[tokio::test]
    async fn the_live_feed_follows_the_replay() {
        let (stream, sender) = stream_with(vec![line(1)], None);
        let mut frames = Box::pin(frames(stream, Resume::None));
        assert!(matches!(frames.next().await, Some(Frame::Hello(_))));
        assert!(matches!(frames.next().await, Some(Frame::Log(_))));
        sender.send(line(2)).unwrap();
        assert!(
            matches!(frames.next().await, Some(Frame::Log(entry)) if entry.seq == 2),
            "the live line arrives as a log frame"
        );
    }

    #[test]
    fn last_event_id_is_read_or_refused() {
        let header = |value: &str| {
            let mut headers = HeaderMap::new();
            if !value.is_empty() {
                headers.insert(LAST_EVENT_ID, value.parse().unwrap());
            }
            headers
        };
        assert_eq!(resume_header(&header("")), Resume::None);
        assert_eq!(resume_header(&HeaderMap::new()), Resume::None);
        assert_eq!(
            resume_header(&header("boot-1:42")),
            Resume::At {
                boot_id: "boot-1".into(),
                seq: 42
            }
        );
        assert_eq!(resume_header(&header("boot-1")), Resume::Unusable);
        assert_eq!(resume_header(&header("boot-1:x")), Resume::Unusable);
    }

    #[test]
    fn a_source_and_a_cursor_are_checked() {
        assert_eq!(source_of("daemon").unwrap(), LogSource::Daemon);
        assert_eq!(source_of("runs").unwrap_err().code(), ErrorCode::BadRequest);
        assert_eq!(
            cursor_of(LogSource::Daemon, "2026-10-13:1|2026-10-13:2")
                .unwrap()
                .encode(),
            "2026-10-13:1|2026-10-13:2"
        );
        assert_eq!(
            cursor_of(LogSource::Worker, "junk").unwrap_err().code(),
            ErrorCode::InvalidCursor
        );
    }
}

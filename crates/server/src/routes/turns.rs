//! The Turns handlers (management-api §2.3): `turns.create`, `turns.get` and the
//! `turns.events` stream, plus the `202` / `200 ?wait=` exchange `jobs.run` shares (§1.6).
//!
//! A Turn is started, never awaited: [`start`] hands the work to [`deadline::detach`] before it
//! waits for anything (§1.9), so a client that goes away leaves the Turn running, and `?wait=`
//! bounds only this request. The stream replays the Turn's events from its start, follows them
//! live until `outcome`, and ends there (§2.3).

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::{Stream, StreamExt};
use tokio::sync::broadcast;

use claudeclaw_api::error::ErrorCode;
use claudeclaw_api::events::StreamHello;
use claudeclaw_api::log_event::daemon;
use claudeclaw_api::permission::TokenPermission;
use claudeclaw_api::turns::{
    AttachmentUpload, TurnAccepted, TurnEvent, TurnRecord, TurnRequest, TurnState,
};

use crate::audit::AuditTarget;
use crate::auth::Principal;
use crate::backend::{BackendFuture, TurnAttachment, TurnCreate, TurnFeed, TurnHandle};
use crate::deadline;
use crate::error::ApiFailure;
use crate::routes::{Params, bad_request};
use crate::state::AppState;

/// §1.6: `turns.create` takes a 64 MiB body (the attachments), everything else 1 MiB.
const MAX_BODY: usize = 64 * 1024 * 1024;
/// §2.3: at most 5 attachments, 10 MB each (the TS Web UI's limits).
const MAX_ATTACHMENTS: usize = 5;
const MAX_ATTACHMENT: usize = 10 * 1024 * 1024;
/// The same 10 MB as the shortest base64 that could carry it, so an oversized upload is refused
/// before it is decoded.
const MAX_ATTACHMENT_B64: usize = MAX_ATTACHMENT.div_ceil(3) * 4;
/// §1.6: `?wait=` is whole seconds and never more than 300.
const MAX_WAIT_S: u32 = 300;

/// `POST /v1/turns?wait=<s>`: queues a Turn and answers at once (§2.3, §1.9).
///
/// `send` reaches this route only for the `global` Scope (§1.3), so a token asking for any other
/// one is `403 missing_permission` naming `send` — before the backend sees the request.
pub async fn create(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(pairs): Query<Vec<(String, String)>>,
    request: Request,
) -> Result<Response, ApiFailure> {
    // The body is read by hand so an oversized one is §1.4's envelope with `payload_too_large`,
    // which axum's `Json` extractor would replace with a plain-text rejection.
    let body = axum::body::to_bytes(request.into_body(), MAX_BODY)
        .await
        .map_err(|_| too_large("the turn request is larger than 64 MiB"))?;
    let request: TurnRequest =
        serde_json::from_slice(&body).map_err(|_| bad_request("the body is not a TurnRequest"))?;
    if !principal.is_admin() && request.scope != claudeclaw_api::Scope::Global {
        return Err(ApiFailure::missing_permission(&TokenPermission::Send));
    }
    let create = TurnCreate {
        scope: request.scope,
        text: request.text,
        attachments: attachments(request.attachments)?,
        notify: request.notify,
        principal: principal.audit_name(),
    };
    let turns = state.turns.clone();
    let work: BackendFuture<'static, TurnHandle> =
        Box::pin(async move { turns.create(create).await });
    start(work, wait_seconds(&pairs), None).await
}

/// `GET /v1/turns/{turn_id}`: the Turn's record as it stands (§2.3).
///
/// Admin reads any Turn; a token reads only the ones it created, and a Turn that did not come
/// through the API belongs to nobody. Anything else is `404 not_found`, so a token cannot even
/// learn that another token's Turn exists (§1.2, §1.3).
pub async fn get(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(turn_id): Path<String>,
) -> Result<Json<TurnRecord>, ApiFailure> {
    let feed = state.turns.get(&turn_id).await?;
    own(&feed, &principal)?;
    Ok(Json(feed.record.borrow().clone()))
}

/// `GET /v1/turns/{turn_id}/events` **[SSE]**: `hello`, the Turn's events replayed from its
/// start, then the live ones until `outcome`, and there the stream ends (§2.3, §1.6).
pub async fn events(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(turn_id): Path<String>,
) -> Result<Response, ApiFailure> {
    let feed = state.turns.get(&turn_id).await?;
    own(&feed, &principal)?;
    let hello = StreamHello {
        boot_id: state.events.boot_id(),
    };
    let stream = frames(hello, feed).map(|frame| Ok::<Event, Infallible>(frame.into_event()));
    let sse = Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    );
    Ok(sse.into_response())
}

/// Starts a Turn through [`deadline::detach`] (§1.9) and answers `202 TurnAccepted`, or
/// `200 TurnRecord` when `?wait=` held the request until the Turn finished (§1.6).
///
/// The work is handed off before anything is awaited, so a client that disconnects mid-wait
/// leaves the Turn running; `wait` bounds this request alone. `target` names the audit entry,
/// which defaults to the Turn's id.
pub(super) async fn start(
    work: BackendFuture<'static, TurnHandle>,
    wait: Option<Duration>,
    target: Option<String>,
) -> Result<Response, ApiFailure> {
    let (task, mut guard) = deadline::detach(work);
    let handle = match task.await {
        Ok(Ok(handle)) => handle,
        Ok(Err(failure)) => {
            guard.disarm();
            return Err(failure.into());
        }
        Err(_stopped) => {
            guard.disarm();
            return Err(ApiFailure::internal("the turn task stopped"));
        }
    };
    // Only the fields `turn.accepted` declares (#69): the id is the audit entry's `target`.
    tracing::info!(
        event = daemon::TURN_ACCEPTED,
        queue_depth = handle.queue_position,
        "turn accepted",
    );
    let mut response = match wait {
        Some(limit) => {
            let mut record = handle.feed.record.clone();
            let done = tokio::time::timeout(
                limit,
                record.wait_for(|record| record.state == TurnState::Done),
            )
            .await;
            match done {
                Ok(Ok(record)) => (StatusCode::OK, Json((*record).clone())).into_response(),
                // Timed out, or the Turn's feed closed without finishing: still accepted (§1.6).
                Ok(Err(_)) | Err(_) => accepted(&handle).into_response(),
            }
        }
        None => accepted(&handle).into_response(),
    };
    response
        .extensions_mut()
        .insert(AuditTarget(target.or_else(|| Some(handle.turn_id.clone()))));
    guard.disarm();
    Ok(response)
}

/// `?wait=` in whole seconds, capped at §1.6's 300. Absent, empty or `0` means "do not wait".
pub(super) fn wait_seconds(pairs: &[(String, String)]) -> Option<Duration> {
    let raw = Params::new(pairs).get("wait")?;
    let seconds: u32 = raw.parse().ok()?;
    if seconds == 0 {
        return None;
    }
    Some(Duration::from_secs(u64::from(seconds.min(MAX_WAIT_S))))
}

/// §1.3: a token may send into `global` only, so the Turn it asked for is not somebody else's
/// Scope.
fn own(feed: &TurnFeed, principal: &Principal) -> Result<(), ApiFailure> {
    if principal.is_admin() {
        return Ok(());
    }
    let owner = principal.audit_name();
    if feed.principal.as_deref() == Some(owner.as_str()) {
        Ok(())
    } else {
        Err(ApiFailure::new(ErrorCode::NotFound, "no such turn"))
    }
}

/// The body's attachments, decoded and within §2.3's count and size limits. Each refusal happens
/// here, before the backend is asked to start anything.
fn attachments(uploads: Option<Vec<AttachmentUpload>>) -> Result<Vec<TurnAttachment>, ApiFailure> {
    let Some(uploads) = uploads else {
        return Ok(Vec::new());
    };
    if uploads.len() > MAX_ATTACHMENTS {
        return Err(too_large(format!(
            "a turn takes at most {MAX_ATTACHMENTS} attachments"
        )));
    }
    uploads
        .into_iter()
        .map(|upload| {
            if upload.data_base64.len() > MAX_ATTACHMENT_B64 {
                return Err(too_large("an attachment is larger than 10 MB"));
            }
            let data = decode_base64(&upload.data_base64)
                .ok_or_else(|| bad_request("`data_base64` is not standard base64"))?;
            if data.len() > MAX_ATTACHMENT {
                return Err(too_large("an attachment is larger than 10 MB"));
            }
            Ok(TurnAttachment {
                name: upload.name,
                content_type: upload.content_type,
                data,
            })
        })
        .collect()
}

/// Strict standard base64 (§2.3): whole groups, `=` only as the last group's padding, and the
/// trailing bits of a short final group must be zero, so a decoded file is the file that was
/// encoded rather than a near miss.
fn decode_base64(raw: &str) -> Option<Vec<u8>> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let groups = bytes.len() / 4;
    for (index, group) in bytes.chunks(4).enumerate() {
        let last = index + 1 == groups;
        let a = sextet(group[0])?;
        let b = sextet(group[1])?;
        out.push((a << 2) | (b >> 4));
        if group[2] == b'=' {
            if !last || group[3] != b'=' || b & 0x0f != 0 {
                return None;
            }
            continue;
        }
        let c = sextet(group[2])?;
        out.push((b << 4) | (c >> 2));
        if group[3] == b'=' {
            if !last || c & 0x03 != 0 {
                return None;
            }
            continue;
        }
        out.push((c << 6) | sextet(group[3])?);
    }
    Some(out)
}

fn sextet(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn too_large(message: impl Into<String>) -> ApiFailure {
    ApiFailure::new(ErrorCode::PayloadTooLarge, message)
}

/// `202 TurnAccepted` (§2.3).
fn accepted(handle: &TurnHandle) -> (StatusCode, Json<TurnAccepted>) {
    (
        StatusCode::ACCEPTED,
        Json(TurnAccepted {
            turn_id: handle.turn_id.clone(),
            scope: handle.scope.clone(),
            queue_position: handle.queue_position,
        }),
    )
}

/// One thing the stream sends. Modelling the frames apart from axum's [`Event`] keeps the
/// ordering and the end of the stream testable: `Event` gives no way to read its bytes back.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// Always first: `event: hello` with the `boot_id` (§1.6).
    Hello(StreamHello),
    /// `event: turn` with one [`TurnEvent`], whose own `kind` tag says which one it is (§2.3).
    Turn(TurnEvent),
}

impl Frame {
    /// The SSE event name.
    pub fn name(&self) -> &'static str {
        match self {
            Frame::Hello(_) => "hello",
            Frame::Turn(_) => "turn",
        }
    }

    /// Renders the frame as an axum SSE [`Event`].
    pub fn into_event(self) -> Event {
        match self {
            Frame::Hello(hello) => json_event("hello", &hello),
            Frame::Turn(event) => json_event("turn", &event),
        }
    }
}

fn json_event<T: serde::Serialize>(name: &'static str, data: &T) -> Event {
    Event::default()
        .event(name)
        .json_data(data)
        .unwrap_or_else(|_| Event::default().event(name))
}

/// The frame stream: `hello`, the Turn's events replayed from its start, then the live ones —
/// and after `outcome` it ends, because there is nothing more to say (§2.3).
///
/// The feed subscribes live before it snapshots the replay, so an event is in exactly one of the
/// two. If the live feed falls behind the ring the stream ends instead of skipping events
/// (§1.6): reconnecting replays the whole Turn.
pub fn frames(hello: StreamHello, feed: TurnFeed) -> impl Stream<Item = Frame> {
    let finished = feed
        .replay
        .iter()
        .any(|event| matches!(event, TurnEvent::Outcome { .. }));
    let replayed =
        std::iter::once(Frame::Hello(hello)).chain(feed.replay.into_iter().map(Frame::Turn));
    let replayed = futures_util::stream::iter(replayed);
    if finished {
        replayed.left_stream()
    } else {
        replayed.chain(live(feed.live)).right_stream()
    }
}

/// The live events after the replay, stopping after `outcome`.
fn live(receiver: broadcast::Receiver<TurnEvent>) -> impl Stream<Item = Frame> {
    futures_util::stream::unfold((receiver, false), |(mut receiver, ended)| async move {
        if ended {
            return None;
        }
        match receiver.recv().await {
            Ok(event) => {
                let ends = matches!(event, TurnEvent::Outcome { .. });
                Some((Frame::Turn(event), (receiver, ends)))
            }
            // The receiver lagged or closed: the replay covers it, and a reconnect gets all of it.
            Err(_lost) => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use claudeclaw_api::scope::Scope;
    use claudeclaw_api::turns::{TurnOutcome, TurnPurpose};
    use futures_util::FutureExt;

    use super::*;

    fn record() -> TurnRecord {
        TurnRecord {
            turn_id: "turn-1".into(),
            scope: Scope::Global,
            origin: "api".into(),
            principal: Some("token:1".into()),
            purpose: TurnPurpose::Prompt,
            state: TurnState::Queued,
            outcome: None,
            text: None,
            compact: None,
            session_id: None,
            launcher: None,
            run_ids: vec![],
            created_at: "2026-10-14T09:00:00Z".parse().unwrap(),
            finished_at: None,
            duration_ms: None,
        }
    }

    fn feed(replay: Vec<TurnEvent>) -> (TurnFeed, broadcast::Sender<TurnEvent>) {
        let (live, receiver) = broadcast::channel(8);
        let (_record, record_receiver) = tokio::sync::watch::channel(record());
        let feed = TurnFeed {
            principal: Some("token:1".into()),
            record: record_receiver,
            replay,
            live: receiver,
        };
        (feed, live)
    }

    /// The frames a stream produces until it pends.
    fn drain(feed: TurnFeed) -> Vec<Frame> {
        let hello = StreamHello {
            boot_id: "boot-1".into(),
        };
        let mut stream = Box::pin(frames(hello, feed));
        let mut out = Vec::new();
        while let Some(Some(frame)) = stream.next().now_or_never() {
            out.push(frame);
        }
        out
    }

    #[test]
    fn base64_takes_only_canonical_standard_input() {
        assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_base64("aGk=").unwrap(), b"hi");
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert!(decode_base64("a===").is_none());
        for bad in ["aGk", "aGk==", "aGk=!", "=Gk=", "a Gk=", "aG\nk=", "AA=A"] {
            assert!(decode_base64(bad).is_none(), "{bad:?}");
        }
        // The trailing bits of a short final group must be zero: `AB` carries one bit past what
        // `AA==` encodes, so it is not the same two bytes back.
        assert!(decode_base64("AB==").is_none());
    }

    #[test]
    fn a_wait_is_whole_seconds_capped_and_absent_means_now() {
        let pairs = |raw: &str| vec![("wait".to_owned(), raw.to_owned())];
        assert_eq!(wait_seconds(&[]), None);
        assert_eq!(wait_seconds(&pairs("")), None);
        assert_eq!(wait_seconds(&pairs("0")), None);
        assert_eq!(wait_seconds(&pairs("soon")), None, "unparsable is ignored");
        assert_eq!(wait_seconds(&pairs("30")), Some(Duration::from_secs(30)));
        assert_eq!(
            wait_seconds(&pairs("1000")),
            Some(Duration::from_secs(300)),
            "§1.6 caps the wait at 300 s"
        );
    }

    #[test]
    fn the_stream_replays_then_ends_after_outcome() {
        let (feed, live) = feed(vec![
            TurnEvent::Queued { position: 1 },
            TurnEvent::Text { delta: "he".into() },
        ]);
        // The feed subscribed before these, so they arrive live and are not replayed twice.
        live.send(TurnEvent::Tool {
            name: "Read".into(),
            category: "read".into(),
            summary: "notes.md".into(),
        })
        .unwrap();
        live.send(TurnEvent::Outcome {
            record: Box::new(TurnRecord {
                state: TurnState::Done,
                outcome: Some(TurnOutcome::Ok),
                ..record()
            }),
        })
        .unwrap();
        let names: Vec<&str> = drain(feed).iter().map(Frame::name).collect();
        assert_eq!(names, ["hello", "turn", "turn", "turn", "turn"]);
    }

    #[test]
    fn a_turn_that_already_finished_never_waits_for_the_live_feed() {
        let (feed, live) = feed(vec![TurnEvent::Outcome {
            record: Box::new(TurnRecord {
                state: TurnState::Done,
                outcome: Some(TurnOutcome::Ok),
                ..record()
            }),
        }]);
        drop(live);
        let names: Vec<&str> = drain(feed).iter().map(Frame::name).collect();
        assert_eq!(names, ["hello", "turn"], "the replay alone ends the stream");
    }

    #[test]
    fn an_admin_reads_any_turn_and_a_token_reads_its_own() {
        let (feed, _live) = feed(vec![]);
        assert!(own(&feed, &Principal::Local).is_ok());
        assert!(own(&feed, &Principal::Device("d1".into())).is_ok());
        assert!(own(&feed, &Principal::ApiToken("1".into(), vec![])).is_ok());
        assert_eq!(
            own(&feed, &Principal::ApiToken("2".into(), vec![]))
                .unwrap_err()
                .code(),
            ErrorCode::NotFound
        );
        let mut anonymous = feed;
        anonymous.principal = None;
        assert!(
            own(&anonymous, &Principal::ApiToken("1".into(), vec![])).is_err(),
            "a Turn that did not come through the API belongs to nobody"
        );
    }
}

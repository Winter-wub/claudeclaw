//! The Deadline backstop (management-api §1.9, #40).
//!
//! Every route carries an `api::Deadline`. This layer enforces the server half of it: if a handler
//! is still running when the backstop fires, the client gets `504 deadline_exceeded` instead of a
//! hang. Streams have no total deadline (their idle limit lives in the SSE handler), and the
//! probe has none either.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use claudeclaw_api::deadline::{Call, Deadline};
use claudeclaw_api::log_event::daemon;

use crate::error::ApiFailure;

/// The deadline layer's per-route state.
#[derive(Clone, Copy)]
pub struct DeadlineCtx {
    pub deadline: Deadline,
}

/// Enforces [`DeadlineCtx::deadline`]'s server bound around the rest of the stack.
pub async fn enforce(State(ctx): State<DeadlineCtx>, request: Request, next: Next) -> Response {
    let call = call_from_query(request.uri().query());
    match ctx.deadline.server(call) {
        Some(limit) => match tokio::time::timeout(limit, next.run(request)).await {
            Ok(response) => response,
            Err(_elapsed) => ApiFailure::deadline_exceeded().into_response(),
        },
        None => next.run(request).await,
    }
}

/// The `wait=` and `timeout_s=` query values a deadline depends on (§1.6). Unknown keys and
/// unparsable values are ignored, matching the "whole seconds" convention.
pub fn call_from_query(query: Option<&str>) -> Call {
    let Some(query) = query else {
        return Call::default();
    };
    let mut call = Call::default();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "wait" => call.wait_s = seconds(value).unwrap_or(0),
            "timeout_s" => call.timeout_s = seconds(value),
            _ => {}
        }
    }
    call
}

fn seconds(value: &str) -> Option<u32> {
    value.parse::<u32>().ok()
}

/// A handle that logs `api.client_gone` when it drops before being disarmed: the marker that hyper
/// dropped a run-starting handler as soon as its client disconnected (§1.9).
pub struct Detach {
    started: tokio::time::Instant,
    armed: bool,
}

impl Detach {
    /// Suppresses the drop log: the handler settled the client's wait itself.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Detach {
    fn drop(&mut self) {
        if self.armed {
            let elapsed_ms = self.started.elapsed().as_millis() as u64;
            tracing::info!(
                event = daemon::API_CLIENT_GONE,
                elapsed_ms = elapsed_ms,
                "client disconnected before the response was sent",
            );
        }
    }
}

/// Spawns `future` and returns its handle alongside a [`Detach`] guard. A run-starting handler
/// hands its work off with this before it answers, so the run survives the client going away
/// (§1.9).
pub fn detach<F>(future: F) -> (tokio::task::JoinHandle<F::Output>, Detach)
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let guard = Detach {
        started: tokio::time::Instant::now(),
        armed: true,
    };
    (tokio::spawn(future), guard)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn query_waits_are_read_from_the_url() {
        let call = call_from_query(Some("wait=60&timeout_s=600"));
        assert_eq!(call.wait_s, 60);
        assert_eq!(call.timeout_s, Some(600));
        assert_eq!(call_from_query(Some("limit=10")), Call::default());
        assert_eq!(call_from_query(Some("wait=x")).wait_s, 0);
        assert_eq!(call_from_query(None), Call::default());
    }

    #[tokio::test(start_paused = true)]
    async fn a_disarmed_detach_does_not_log_or_wait() {
        let (handle, mut guard) = detach(async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            7
        });
        guard.disarm();
        assert_eq!(handle.await.unwrap(), 7);
    }
}

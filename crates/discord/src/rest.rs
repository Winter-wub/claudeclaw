//! The Discord REST side of the adapter: a thin wrapper over `twilight_http`'s `Client`, the
//! owner DM fan-out, and the `notify_owner` entry point for the Watchdog (architecture §4
//! `claudeclaw-discord`; TS parity, `src/commands/discord.ts:217-320`).
//!
//! `twilight_http` builds every URL as `{http|https}://{host}/api/v10/{path}`, so
//! [`DiscordEndpoints::rest`](crate::DiscordEndpoints::rest) is a **host**, not a base path: the
//! Parity suite's replay redirects the adapter by giving a `http://127.0.0.1:<port>` host, and
//! [`rest_host`] keeps only its authority. The scheme picks the transport (`http://` → plain
//! HTTP), and `None` keeps Discord's real `https://discord.com`.
//!
//! The token is a [`SecretString`], read only to hand to `twilight_http`; it is never logged.
//! Failures are returned as [`RestError`] and logged by the caller, so the level differs per call
//! site (a stream edit is a debug line, a final reply is a warning).

use std::fmt;
use std::time::Duration;

use claudeclaw_chat::{NotifyError, reply_chunks};
use secrecy::{ExposeSecret, SecretString};
use twilight_http::Client;
use twilight_http::error::ErrorType;
use twilight_http::request::channel::reaction::RequestReactionType;
use twilight_model::http::attachment::Attachment;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, MessageMarker, UserMarker};

use crate::DiscordEndpoints;

/// Discord's per-message character limit (`src/commands/discord.ts:261`).
pub const MAX_MESSAGE_LEN: usize = 2000;

/// How many times a `429` is retried before the call fails (TS `discordApi`,
/// `src/commands/discord.ts:237`: `attempt >= 3`).
const MAX_RETRIES: u32 = 3;

/// The fallback wait for a `429` with no usable `retry_after` (TS,
/// `src/commands/discord.ts:242-243`: five seconds).
const RETRY_FALLBACK: Duration = Duration::from_secs(5);

/// The `discord.send_failed` Daemon event
/// (`claudeclaw_api::log_event::daemon::DISCORD_SEND_FAILED`, #23). The constant cannot be
/// imported: this crate depends only on `chat`.
const SEND_FAILED_EVENT: &str = "discord.send_failed";

/// Runs one `twilight_http` request, retrying a `429` up to [`MAX_RETRIES`] times with the wait
/// the response asks for. `$call` is re-evaluated on each attempt, so it must be a builder
/// expression, not a value already moved into a future.
macro_rules! discord_call {
    ($call:expr) => {{
        let mut attempt: u32 = 0;
        loop {
            match ($call).await {
                Ok(value) => break Ok(value),
                Err(err) => match rate_limit_delay(&err) {
                    Some(delay) if attempt < MAX_RETRIES => {
                        tracing::debug!(
                            event = SEND_FAILED_EVENT,
                            status = 429u16,
                            "a Discord call was rate limited; retrying"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    _ => break Err(RestError::from(err)),
                },
            }
        }
    }};
}

/// Where a [`Rest`] client points: a host and whether to speak plain HTTP. From
/// [`rest_host`], which reads [`DiscordEndpoints::rest`](crate::DiscordEndpoints::rest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestHost {
    /// The authority only, host and optional port; any path is ignored (`twilight_http`
    /// appends `/api/v10/…` itself).
    pub host: String,
    /// `true` for an `http://` endpoint (the Parity replay's local server), `false` for HTTPS.
    pub use_http: bool,
}

/// The host to point `twilight_http` at, from `endpoints.rest` (module docs). `None`, an empty
/// authority and a value without a scheme all fall back to `discord.com` over HTTPS.
pub fn rest_host(endpoints: &DiscordEndpoints) -> RestHost {
    /// Discord's real API host.
    const DEFAULT: &str = "discord.com";
    let Some(raw) = endpoints.rest.as_deref() else {
        return RestHost {
            host: DEFAULT.to_owned(),
            use_http: false,
        };
    };
    let (use_http, rest) = match raw.strip_prefix("http://") {
        Some(rest) => (true, rest),
        None => (false, raw.strip_prefix("https://").unwrap_or(raw)),
    };
    // `twilight_http` appends the path, so keep only the authority up to the first `/`.
    let authority = rest.split('/').next().unwrap_or("");
    RestHost {
        host: if authority.is_empty() {
            DEFAULT.to_owned()
        } else {
            authority.to_owned()
        },
        use_http,
    }
}

/// Why a REST call failed. `status` is the HTTP status when Discord answered (`0` otherwise),
/// which is what the `discord.send_failed` log event reports.
#[derive(Debug, thiserror::Error)]
pub enum RestError {
    /// An ID in a [`MessageRef`](claudeclaw_chat::MessageRef) was not a Discord snowflake.
    #[error("a Discord ID was not a number")]
    Id,
    /// Discord's reply could not be read (a body without the expected `id`).
    #[error("a Discord reply could not be parsed")]
    Malformed,
    /// The call itself failed: a transport error, a rejected token, or a non-success status.
    #[error("the Discord call failed")]
    Http(#[source] Box<twilight_http::Error>),
}

impl From<twilight_http::Error> for RestError {
    fn from(err: twilight_http::Error) -> Self {
        RestError::Http(Box::new(err))
    }
}

impl RestError {
    /// The HTTP status Discord answered with, or `0` when the failure never got that far.
    pub fn status(&self) -> u16 {
        match self {
            RestError::Http(err) => match err.kind() {
                ErrorType::Response { status, .. } => status.get(),
                _ => 0,
            },
            RestError::Id | RestError::Malformed => 0,
        }
    }

    /// Whether Discord answered `404`: the channel, message or user is gone, which callers
    /// treat as "nothing to do" rather than a failure.
    pub fn is_not_found(&self) -> bool {
        self.status() == 404
    }
}

/// A file to upload with a message: its name and its bytes. `Debug` prints the name's and body's
/// sizes, never the bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct UploadFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for UploadFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadFile")
            .field("name_bytes", &self.name.len())
            .field("size_bytes", &self.bytes.len())
            .finish()
    }
}

/// The Discord REST client, shared through an `Arc` by the notifier and the reply sink
/// (architecture §4 `claudeclaw-discord`). `Debug` is content-free; the token never appears.
pub struct Rest {
    client: Client,
}

impl fmt::Debug for Rest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rest").finish_non_exhaustive()
    }
}

impl Rest {
    /// A client for `token` pointed at `endpoints`, with `twilight_http`'s own rate limiter.
    pub fn connect(token: &SecretString, endpoints: &DiscordEndpoints) -> Self {
        Self::build(token, endpoints, true)
    }

    /// A client with no rate limiter, for tests: the local replay server must answer
    /// immediately, with no bucket waits.
    #[cfg(test)]
    pub(crate) fn for_test(token: &SecretString, endpoints: &DiscordEndpoints) -> Self {
        Self::build(token, endpoints, false)
    }

    fn build(token: &SecretString, endpoints: &DiscordEndpoints, ratelimiter: bool) -> Self {
        let host = rest_host(endpoints);
        let mut builder = Client::builder()
            .token(token.expose_secret().to_owned())
            .proxy(host.host, host.use_http);
        if !ratelimiter {
            builder = builder.ratelimiter(None);
        }
        Rest {
            client: builder.build(),
        }
    }

    /// The underlying client, for the gateway side of the adapter to reuse (slash commands,
    /// thread Hire/Fire, interactions).
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Opens (or fetches) the DM channel with `user` (TS `sendMessageToUser`,
    /// `src/commands/discord.ts:291-304`).
    pub async fn create_dm(&self, user: Id<UserMarker>) -> Result<Id<ChannelMarker>, RestError> {
        let response = discord_call!(self.client.create_private_channel(user))?;
        model_id(response).await
    }

    /// Sends `content` to `channel`, optionally as a reply to `reply_to`, and returns the new
    /// message's ID (TS `sendMessage`/`postPlaceholder`, `src/commands/discord.ts:273-289`,
    /// `861-877`). `content` is already split to at most [`MAX_MESSAGE_LEN`].
    pub async fn send(
        &self,
        channel: Id<ChannelMarker>,
        content: &str,
        reply_to: Option<Id<MessageMarker>>,
    ) -> Result<Id<MessageMarker>, RestError> {
        let response = match reply_to {
            Some(reply) => discord_call!(
                self.client
                    .create_message(channel)
                    .content(content)
                    .reply(reply)
            )?,
            None => discord_call!(self.client.create_message(channel).content(content))?,
        };
        model_id(response).await
    }

    /// Sends `content` with `files` attached as a multipart message (TS `uploadImageMessage`,
    /// `src/commands/discord.ts:448-481`). `content` is not empty: the caller substitutes
    /// `\u{200b}` when the text is blank (TS `sendMessageWithImages`,
    /// `src/commands/discord.ts:439`).
    pub async fn send_files(
        &self,
        channel: Id<ChannelMarker>,
        content: &str,
        files: &[UploadFile],
    ) -> Result<Id<MessageMarker>, RestError> {
        let attachments: Vec<Attachment> = files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                Attachment::from_bytes(file.name.clone(), file.bytes.clone(), i as u64)
            })
            .collect();
        let response = discord_call!(
            self.client
                .create_message(channel)
                .content(content)
                .attachments(&attachments)
        )?;
        model_id(response).await
    }

    /// Edits a message's content (TS stream edits, `src/commands/discord.ts:888-893`).
    pub async fn edit(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
        content: &str,
    ) -> Result<(), RestError> {
        discord_call!(
            self.client
                .update_message(channel, message)
                .content(Some(content))
        )?;
        Ok(())
    }

    /// Deletes a message (TS `finalize`, `src/commands/discord.ts:936`).
    pub async fn delete(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
    ) -> Result<(), RestError> {
        discord_call!(self.client.delete_message(channel, message))?;
        Ok(())
    }

    /// Adds the bot's own reaction to a message (TS `sendReaction`,
    /// `src/commands/discord.ts:306-320`).
    pub async fn add_reaction(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
        emoji: &str,
    ) -> Result<(), RestError> {
        let reaction = RequestReactionType::Unicode { name: emoji };
        discord_call!(self.client.create_reaction(channel, message, &reaction))?;
        Ok(())
    }

    /// Removes the bot's own reaction from a message (TS `removeReaction`,
    /// `src/commands/discord.ts:322-336`).
    pub async fn remove_reaction(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
        emoji: &str,
    ) -> Result<(), RestError> {
        let reaction = RequestReactionType::Unicode { name: emoji };
        discord_call!(
            self.client
                .delete_current_user_reaction(channel, message, &reaction)
        )?;
        Ok(())
    }
}

/// The REST-only owner notification for the Watchdog's Give-up and Held-start DMs
/// (architecture §4 `claudeclaw-discord`; TS `sendMessageToUser`, `src/commands/discord.ts:291`).
///
/// Each owner gets a DM; a failure for one owner is logged and does not stop the others. Only if
/// **every** DM failed is the error returned, so a caller can tell "nobody was reached"
/// (TS itself never throws: the round-trip is best-effort). No owner at all is
/// [`NotifyError::NoOwners`].
pub async fn notify_owner(
    token: &SecretString,
    endpoints: &DiscordEndpoints,
    user_ids: &[String],
    text: &str,
) -> Result<(), NotifyError> {
    let owners = parse_owner_ids(user_ids);
    if owners.is_empty() {
        return Err(NotifyError::NoOwners);
    }
    let rest = Rest::connect(token, endpoints);
    send_owner_text(&rest, &owners, text).await
}

/// Parses the allow-list's user IDs, skipping (and warning about) any that is not a snowflake.
pub(crate) fn parse_owner_ids(user_ids: &[String]) -> Vec<Id<UserMarker>> {
    user_ids
        .iter()
        .filter_map(|raw| match raw.parse::<Id<UserMarker>>() {
            Ok(id) => Some(id),
            Err(_) => {
                tracing::warn!(
                    "a Discord owner ID in the allow-list is not a valid ID; skipping it"
                );
                None
            }
        })
        .collect()
}

/// DMs `text` to every owner (TS `sendMessageToUser` + `sendMessage`,
/// `src/commands/discord.ts:291-304`). `text` is split at [`MAX_MESSAGE_LEN`] with the `[react:x]`
/// directives stripped, as `sendMessage` does.
pub(crate) async fn send_owner_text(
    rest: &Rest,
    owners: &[Id<UserMarker>],
    text: &str,
) -> Result<(), NotifyError> {
    let chunks = reply_chunks(text, MAX_MESSAGE_LEN);
    let mut delivered = false;
    let mut last: Option<RestError> = None;
    for owner in owners {
        match send_dm(rest, *owner, &chunks).await {
            Ok(()) => delivered = true,
            Err(err) => {
                warn_send_failed(&err);
                last = Some(err);
            }
        }
    }
    if delivered {
        Ok(())
    } else {
        Err(NotifyError::Transport(Box::new(
            last.unwrap_or(RestError::Id),
        )))
    }
}

/// Creates the DM channel and sends `chunks` in order.
async fn send_dm(rest: &Rest, owner: Id<UserMarker>, chunks: &[String]) -> Result<(), RestError> {
    let channel = rest.create_dm(owner).await?;
    for chunk in chunks {
        rest.send(channel, chunk, None).await?;
    }
    Ok(())
}

/// Logs a failed Discord call as `discord.send_failed {status}` (see [`SEND_FAILED_EVENT`]).
pub(crate) fn warn_send_failed(err: &RestError) {
    tracing::warn!(
        event = SEND_FAILED_EVENT,
        status = err.status(),
        "a Discord call failed: {err}"
    );
}

/// Logs a failed Discord call at debug level: a stream edit that fails is cosmetic and must not
/// raise a warning on every 1500 ms tick (TS `debugLog`, `src/commands/discord.ts:894`).
pub(crate) fn debug_send_failed(err: &RestError) {
    tracing::debug!(
        event = SEND_FAILED_EVENT,
        status = err.status(),
        "a Discord stream edit failed: {err}"
    );
}

/// The `{ "id": … }` shape every create call answers with, parsed into a typed [`Id`].
#[derive(serde::Deserialize)]
struct IdBody<T> {
    id: T,
}

/// Reads `response`'s body and parses its `id` (the only field any create call needs).
async fn model_id<B, T>(response: twilight_http::Response<B>) -> Result<T, RestError>
where
    T: serde::de::DeserializeOwned,
{
    let bytes = response.bytes().await.map_err(|_| RestError::Malformed)?;
    let body: IdBody<T> = serde_json::from_slice(&bytes).map_err(|_| RestError::Malformed)?;
    Ok(body.id)
}

/// The wait a `429` asks for, or `None` when `err` is not a rate limit (TS `discordApi`,
/// `src/commands/discord.ts:236-247`). `retry_after` is seconds; a missing or non-finite value
/// becomes [`RETRY_FALLBACK`].
fn rate_limit_delay(err: &twilight_http::Error) -> Option<Duration> {
    let ErrorType::Response { body, status, .. } = err.kind() else {
        return None;
    };
    if status.get() != 429 {
        return None;
    }
    let retry_after = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("retry_after").and_then(serde_json::Value::as_f64))
        .filter(|seconds| seconds.is_finite());
    Some(match retry_after {
        Some(seconds) => Duration::from_millis((seconds * 1000.0).ceil() as u64),
        None => RETRY_FALLBACK,
    })
}

#[cfg(test)]
pub(crate) mod test_server {
    //! A tiny HTTP/1.1 server on `127.0.0.1` for the module tests: no live Discord, and a
    //! [`DiscordEndpoints`] host pointed at it (architecture §4; #18). It records the requests it
    //! saw and answers canned JSON, so the real `twilight_http` path is exercised end to end.

    use std::io;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use secrecy::SecretString;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;

    use super::Rest;
    use crate::DiscordEndpoints;

    /// One request the server answered.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Request {
        pub method: String,
        pub path: String,
        pub body: Vec<u8>,
    }

    impl Request {
        /// The body as text, for assertions; non-UTF-8 bytes are replaced.
        pub(crate) fn text(&self) -> String {
            String::from_utf8_lossy(&self.body).into_owned()
        }
    }

    /// A canned answer that takes precedence over the defaults.
    #[derive(Clone)]
    struct Canned {
        method: String,
        path_contains: String,
        status: u16,
        body: String,
        /// Removed after its first use, so a retry sees the default.
        once: bool,
    }

    #[derive(Default)]
    struct State {
        requests: Mutex<Vec<Request>>,
        canned: Mutex<Vec<Canned>>,
    }

    /// A running replay server. Drop it to stop the accept loop.
    pub(crate) struct Server {
        addr: SocketAddr,
        state: Arc<State>,
        task: JoinHandle<()>,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl Server {
        /// Binds `127.0.0.1:0` and starts serving.
        pub(crate) async fn start() -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let state = Arc::new(State::default());
            let task = tokio::spawn(accept_loop(listener, state.clone()));
            Server { addr, state, task }
        }

        /// The endpoints a [`Rest`] should connect with.
        pub(crate) fn endpoints(&self) -> DiscordEndpoints {
            DiscordEndpoints {
                gateway: None,
                rest: Some(format!("http://{}", self.addr)),
            }
        }

        /// A rate-limiter-free client pointed at this server.
        pub(crate) fn connect(&self) -> Rest {
            Rest::for_test(&SecretString::from("test-token"), &self.endpoints())
        }

        /// Every request served so far, in order.
        pub(crate) fn requests(&self) -> Vec<Request> {
            self.state.requests.lock().unwrap().clone()
        }

        /// Makes every `method` request whose path contains `path_contains` answer `status` with
        /// `body`, instead of the default.
        pub(crate) fn canned(&self, method: &str, path_contains: &str, status: u16, body: &str) {
            self.state.canned.lock().unwrap().push(Canned {
                method: method.to_owned(),
                path_contains: path_contains.to_owned(),
                status,
                body: body.to_owned(),
                once: false,
            });
        }

        /// Like [`Server::canned`], but the answer is used once: the next matching request falls
        /// through to the default, so a `429` retry can succeed.
        pub(crate) fn canned_once(
            &self,
            method: &str,
            path_contains: &str,
            status: u16,
            body: &str,
        ) {
            self.state.canned.lock().unwrap().push(Canned {
                method: method.to_owned(),
                path_contains: path_contains.to_owned(),
                status,
                body: body.to_owned(),
                once: true,
            });
        }
    }

    async fn accept_loop(listener: TcpListener, state: Arc<State>) {
        while let Ok((stream, _)) = listener.accept().await {
            let state = state.clone();
            tokio::spawn(async move {
                let _ = serve(stream, state).await;
            });
        }
    }

    async fn serve(mut stream: TcpStream, state: Arc<State>) -> io::Result<()> {
        let raw = read_request(&mut stream).await?;
        let Some(head_end) = find_head_end(&raw) else {
            return Ok(());
        };
        let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();
        let body = raw[head_end..].to_vec();
        state.requests.lock().unwrap().push(Request {
            method: method.clone(),
            path: path.clone(),
            body,
        });

        let (status, body) = answer(&state, &method, &path);
        write_response(&mut stream, status, &body).await
    }

    /// The canned answer if one matches, else the route default.
    fn answer(state: &State, method: &str, path: &str) -> (u16, String) {
        let mut canned = state.canned.lock().unwrap();
        if let Some(index) = canned
            .iter()
            .position(|c| c.method == method && path.contains(&c.path_contains))
        {
            if canned[index].once {
                let c = canned.remove(index);
                return (c.status, c.body);
            }
            return (canned[index].status, canned[index].body.clone());
        }
        drop(canned);
        default_answer(method, path)
    }

    fn default_answer(method: &str, path: &str) -> (u16, String) {
        if method == "POST" && path.ends_with("/users/@me/channels") {
            return (200, r#"{"id":"123"}"#.to_owned());
        }
        if path.contains("/reactions/") && (method == "PUT" || method == "DELETE") {
            return (204, String::new());
        }
        if method == "POST" && path.ends_with("/messages") {
            return (200, r#"{"id":"456"}"#.to_owned());
        }
        if method == "PATCH" && path.contains("/messages/") {
            return (204, String::new());
        }
        if method == "DELETE" && path.contains("/messages/") {
            return (204, String::new());
        }
        (404, r#"{"message":"Not Found","code":10003}"#.to_owned())
    }

    /// Reads one request: the head, then `Content-Length` bytes of body.
    async fn read_request(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(head_end) = find_head_end(&buf) {
                let length = content_length(&buf[..head_end]);
                if buf.len() >= head_end + length {
                    return Ok(buf);
                }
            }
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(buf);
            }
            buf.extend_from_slice(&chunk[..read]);
        }
    }

    fn find_head_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
    }

    fn content_length(head: &[u8]) -> usize {
        let text = String::from_utf8_lossy(head);
        for line in text.split("\r\n") {
            let lower = line.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("content-length:") {
                return value.trim().parse().unwrap_or(0);
            }
        }
        0
    }

    async fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            404 => "Not Found",
            413 => "Payload Too Large",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            _ => "Status",
        };
        let mut head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        if !body.is_empty() {
            head.push_str("Content-Type: application/json\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).await?;
        if !body.is_empty() {
            stream.write_all(body.as_bytes()).await?;
        }
        stream.flush().await
    }
}

#[cfg(test)]
mod tests {
    use super::test_server::Server;
    use super::*;

    fn endpoints(rest: Option<&str>) -> DiscordEndpoints {
        DiscordEndpoints {
            gateway: None,
            rest: rest.map(str::to_owned),
        }
    }

    #[test]
    fn rest_host_reads_the_scheme_and_authority() {
        assert_eq!(
            rest_host(&endpoints(None)),
            RestHost {
                host: "discord.com".into(),
                use_http: false
            }
        );
        assert_eq!(
            rest_host(&endpoints(Some("https://discord.com"))),
            RestHost {
                host: "discord.com".into(),
                use_http: false
            }
        );
        assert_eq!(
            rest_host(&endpoints(Some("http://127.0.0.1:8080"))),
            RestHost {
                host: "127.0.0.1:8080".into(),
                use_http: true
            }
        );
        // A path after the authority is ignored: twilight appends `/api/v10/…` itself.
        assert_eq!(
            rest_host(&endpoints(Some("http://127.0.0.1:8080/api/v10"))),
            RestHost {
                host: "127.0.0.1:8080".into(),
                use_http: true
            }
        );
        assert_eq!(
            rest_host(&endpoints(Some("discord.com"))),
            RestHost {
                host: "discord.com".into(),
                use_http: false
            }
        );
        // An empty authority falls back to Discord.
        assert_eq!(
            rest_host(&endpoints(Some("http://"))),
            RestHost {
                host: "discord.com".into(),
                use_http: true
            }
        );
    }

    #[test]
    fn owner_ids_skip_garbage() {
        let ids = parse_owner_ids(&[
            "123".to_owned(),
            "not-a-snowflake".to_owned(),
            "0".to_owned(),
            "9".to_owned(),
        ]);
        assert_eq!(ids, vec![Id::new(123), Id::new(9)]);
    }

    #[tokio::test]
    async fn create_dm_sends_the_recipient() {
        let server = Server::start().await;
        let rest = server.connect();
        let channel = rest.create_dm(Id::new(42)).await.unwrap();
        assert_eq!(channel, Id::new(123));
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v10/users/@me/channels");
        assert_eq!(requests[0].text(), r#"{"recipient_id":"42"}"#);
    }

    #[tokio::test]
    async fn send_and_reply_carry_the_content() {
        let server = Server::start().await;
        let rest = server.connect();
        let channel = Id::new(123);
        assert_eq!(
            rest.send(channel, "hello", None).await.unwrap(),
            Id::new(456)
        );
        rest.send(channel, "back", Some(Id::new(456)))
            .await
            .unwrap();
        let requests = server.requests();
        assert_eq!(requests[0].text(), r#"{"content":"hello"}"#);
        // The reply reference is what `interrupted` uses.
        let body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(body["content"], "back");
        assert_eq!(body["message_reference"]["message_id"], "456");
    }

    #[tokio::test]
    async fn reactions_and_deletes_are_204() {
        let server = Server::start().await;
        let rest = server.connect();
        let channel = Id::new(123);
        let message = Id::new(456);
        rest.add_reaction(channel, message, "🔥").await.unwrap();
        rest.remove_reaction(channel, message, "🔥").await.unwrap();
        rest.delete(channel, message).await.unwrap();
        let requests = server.requests();
        // `🔥` is percent-encoded in the path.
        assert!(requests[0].path.contains("/reactions/%F0%9F%94%A5/@me"));
        assert_eq!(requests[0].method, "PUT");
        assert_eq!(requests[1].method, "DELETE");
        assert!(requests[1].path.contains("/reactions/%F0%9F%94%A5/@me"));
        assert_eq!(requests[2].method, "DELETE");
        assert_eq!(requests[2].path, "/api/v10/channels/123/messages/456");
    }

    #[tokio::test]
    async fn a_404_is_reported_with_its_status() {
        let server = Server::start().await;
        server.canned(
            "DELETE",
            "/messages/",
            404,
            r#"{"message":"gone","code":10008}"#,
        );
        let rest = server.connect();
        let err = rest.delete(Id::new(1), Id::new(2)).await.unwrap_err();
        assert_eq!(err.status(), 404);
        assert!(err.is_not_found());
    }

    #[tokio::test]
    async fn a_429_is_retried_then_succeeds() {
        let server = Server::start().await;
        server.canned_once("POST", "/messages", 429, r#"{"retry_after":0.001}"#);
        let rest = server.connect();
        assert_eq!(
            rest.send(Id::new(123), "hi", None).await.unwrap(),
            Id::new(456)
        );
        assert_eq!(server.requests().len(), 2);
    }

    #[tokio::test]
    async fn a_500_is_not_retried() {
        let server = Server::start().await;
        server.canned("POST", "/messages", 500, r#"{"message":"boom"}"#);
        let rest = server.connect();
        let err = rest.send(Id::new(1), "hi", None).await.unwrap_err();
        assert_eq!(err.status(), 500);
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn file_uploads_are_multipart() {
        let server = Server::start().await;
        let rest = server.connect();
        let files = [UploadFile {
            name: "a.png".into(),
            bytes: vec![1, 2, 3],
        }];
        rest.send_files(Id::new(123), "\u{200b}", &files)
            .await
            .unwrap();
        let request = &server.requests()[0];
        assert_eq!(request.method, "POST");
        assert!(request.text().contains("a.png"));
        assert!(request.text().contains("payload_json"));
    }

    #[tokio::test]
    async fn notify_owner_dms_each_owner() {
        let server = Server::start().await;
        let token = SecretString::from("test-token");
        notify_owner(
            &token,
            &server.endpoints(),
            &["7".to_owned(), "9".to_owned()],
            "please resend",
        )
        .await
        .unwrap();
        let requests = server.requests();
        // Two DM channels, then one message each.
        assert_eq!(requests.len(), 4);
        assert!(requests[0].text().contains(r#""recipient_id":"7""#));
        assert!(requests[2].text().contains(r#""recipient_id":"9""#));
    }

    #[tokio::test]
    async fn notify_owner_without_owners_is_no_owners() {
        let server = Server::start().await;
        let token = SecretString::from("test-token");
        let err = notify_owner(&token, &server.endpoints(), &[], "hi")
            .await
            .unwrap_err();
        assert!(matches!(err, NotifyError::NoOwners));
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn notify_owner_reports_a_total_failure() {
        let server = Server::start().await;
        server.canned("POST", "/users/@me/channels", 500, r#"{"message":"boom"}"#);
        let token = SecretString::from("test-token");
        let err = notify_owner(&token, &server.endpoints(), &["7".to_owned()], "hi")
            .await
            .unwrap_err();
        assert!(matches!(err, NotifyError::Transport(_)));
    }

    #[test]
    fn upload_file_debug_hides_the_bytes() {
        let file = UploadFile {
            name: "secret-name.png".into(),
            bytes: vec![0xde, 0xad],
        };
        let got = format!("{file:?}");
        assert!(!got.contains("secret"), "{got}");
        assert!(got.contains("size_bytes: 2"), "{got}");
    }

    #[tokio::test]
    async fn rest_debug_is_content_free() {
        let rest = Rest::for_test(&SecretString::from("top-secret"), &endpoints(None));
        let got = format!("{rest:?}");
        assert!(!got.contains("top-secret"), "{got}");
        assert!(!got.contains("secret"), "{got}");
    }

    #[test]
    fn rest_error_debug_is_content_free() {
        let got = format!("{:?}", RestError::Id);
        assert!(!got.contains("secret"), "{got}");
    }
}

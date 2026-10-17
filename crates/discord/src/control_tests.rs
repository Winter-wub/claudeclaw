//! Tests for the outbound REST side ([`super`]) against a fake Discord REST server on
//! `127.0.0.1`. No live Discord: the server is a `std::net::TcpListener` on a background
//! `std::thread` that answers each request with a canned JSON body (the strategy #18's Parity
//! suite replays with).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use claudeclaw_chat::AdapterState;
use secrecy::SecretString;
use twilight_model::channel::ChannelType;
use twilight_model::id::Id;

use super::*;
use crate::gateway::Shared;
use crate::triggers::DiscordSettings;

/// twilight's HTTP client builds a rustls connector eagerly, so a process crypto provider must
/// exist even though every request here is plain HTTP. Idempotent.
fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[derive(Debug, Clone)]
struct Request {
    method: String,
    path: String,
    body: String,
}

#[derive(Debug, Clone)]
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_owned(),
        }
    }

    fn no_content() -> Self {
        Self {
            status: 204,
            body: String::new(),
        }
    }
}

type Handler = Box<dyn Fn(&Request) -> Reply + Send + Sync>;

/// A fake Discord REST server. Each accepted connection is served once and closed.
struct FakeServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl FakeServer {
    fn start(handler: Handler) -> Self {
        install_crypto();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let reply = handler(&request);
                recorded.lock().unwrap().push(request);
                write_reply(&mut stream, &reply);
            }
        });
        Self { addr, requests }
    }

    fn endpoints(&self) -> DiscordEndpoints {
        DiscordEndpoints {
            gateway: None,
            rest: Some(format!("http://{}", self.addr)),
        }
    }

    /// A [`Shared`] whose cached REST client points at this server.
    fn shared(&self) -> Arc<Shared> {
        let shared = Arc::new(Shared::new());
        shared.set_rest(Some(Arc::new(TwilightRest::new(
            "test-token",
            &self.endpoints(),
        ))));
        shared
    }

    fn count(&self, method: &str, suffix: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == method && request.path.ends_with(suffix))
            .count()
    }

    fn find(&self, method: &str, suffix: &str) -> Option<Request> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.method == method && request.path.ends_with(suffix))
            .cloned()
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = buffer[header_end..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Some(Request {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) {
    let reason = match reply.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        404 => "Not Found",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body,
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn settings_with_owners(owners: &[&str]) -> DiscordSettings {
    DiscordSettings {
        allowed_user_ids: owners.iter().map(|owner| (*owner).to_owned()).collect(),
        ..DiscordSettings::default()
    }
}

#[test]
fn proxy_for_strips_the_scheme_and_keeps_only_the_authority() {
    assert_eq!(proxy_for(&None), None);
    assert_eq!(proxy_for(&Some("https://discord.com".to_owned())), None);
    assert_eq!(proxy_for(&Some(String::new())), None);
    assert_eq!(
        proxy_for(&Some("http://127.0.0.1:1234".to_owned())),
        Some(("127.0.0.1:1234".to_owned(), true))
    );
    assert_eq!(
        proxy_for(&Some("https://example.test/api/v10".to_owned())),
        Some(("example.test".to_owned(), false))
    );
    // A scheme-less host is treated as HTTPS.
    assert_eq!(
        proxy_for(&Some("example.test".to_owned())),
        Some(("example.test".to_owned(), false))
    );
}

#[test]
fn channel_kind_maps_only_the_listenable_types() {
    assert_eq!(
        channel_kind(ChannelType::GuildText),
        Some(ChannelKind::Text)
    );
    assert_eq!(
        channel_kind(ChannelType::GuildAnnouncement),
        Some(ChannelKind::Announcement)
    );
    assert_eq!(
        channel_kind(ChannelType::GuildForum),
        Some(ChannelKind::Forum)
    );
    assert_eq!(
        channel_kind(ChannelType::GuildVoice),
        Some(ChannelKind::Voice)
    );
    assert_eq!(channel_kind(ChannelType::Private), None);
    assert_eq!(channel_kind(ChannelType::Group), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hire_creates_a_thread_tracks_it_and_posts_a_confirmation() {
    let server = FakeServer::start(Box::new(|request: &Request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", path) if path.ends_with("/channels/7/threads") => Reply::json(
                201,
                r#"{"id":"77","type":11,"name":"Researcher","parent_id":"7"}"#,
            ),
            ("POST", path) if path.ends_with("/channels/77/messages") => Reply::json(200, "{}"),
            _ => Reply::json(404, "{}"),
        }
    }));
    let shared = server.shared();
    let control = DiscordControl::new(Arc::clone(&shared));

    let created = control
        .hire("7".to_owned(), vec!["Researcher".to_owned()])
        .await
        .expect("hire");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].id, "77");
    assert_eq!(created[0].name, "Researcher");
    assert_eq!(created[0].parent_id, "7");

    // The thread was created once and got its confirmation post.
    assert_eq!(server.count("POST", "/channels/7/threads"), 1);
    assert_eq!(server.count("POST", "/channels/77/messages"), 1);
    let confirmation = server.find("POST", "/channels/77/messages").unwrap();
    assert!(
        confirmation.body.contains("Researcher"),
        "{}",
        confirmation.body
    );
    // And the cache learned it, with the agent dir key.
    let track = shared.thread(&Id::new(77)).expect("tracked");
    assert_eq!(track.parent, Id::new(7));
    assert_eq!(track.agent_name.as_deref(), Some("researcher-77"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hire_rejects_a_non_snowflake_channel_without_a_request() {
    let server = FakeServer::start(Box::new(|_| Reply::json(404, "{}")));
    let shared = server.shared();
    let control = DiscordControl::new(shared);
    let error = control
        .hire("not-a-channel".to_owned(), vec!["x".to_owned()])
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AdapterError::NotFound {
            what: "channel",
            ..
        }
    ));
    assert_eq!(server.count("POST", "/threads"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fire_deletes_the_thread_and_reports_already_gone() {
    let server = FakeServer::start(Box::new(|request: &Request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("DELETE", path) if path.ends_with("/channels/77") => Reply::no_content(),
            _ => Reply::json(404, "{}"),
        }
    }));
    let shared = server.shared();
    let control = DiscordControl::new(Arc::clone(&shared));

    let done = control.fire("77".to_owned()).await.expect("fire");
    assert_eq!(done.thread_id, "77");
    assert!(done.deleted);
    assert!(shared.thread(&Id::new(77)).is_none());

    // A thread Discord no longer has is a success with `deleted: false`.
    let done = control.fire("78".to_owned()).await.expect("fire");
    assert_eq!(done.thread_id, "78");
    assert!(!done.deleted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overview_reports_the_connection_state() {
    let server = FakeServer::start(Box::new(|_| Reply::json(404, "{}")));
    let shared = server.shared();
    shared.set_state(AdapterState::Connected);
    let control = DiscordControl::new(shared);
    let overview = control.overview().await.expect("overview");
    assert_eq!(overview.state, AdapterState::Connected);
    assert!(overview.bot.is_none());
    assert!(overview.guilds.is_empty());
    assert!(
        control
            .available_channels()
            .await
            .expect("channels")
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_owners_dms_every_owner() {
    let server = FakeServer::start(Box::new(|request: &Request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", path) if path.ends_with("/users/@me/channels") => {
                Reply::json(200, r#"{"id":"88","type":1}"#)
            }
            ("POST", path) if path.ends_with("/channels/88/messages") => Reply::json(200, "{}"),
            _ => Reply::json(404, "{}"),
        }
    }));
    let shared = server.shared();
    shared.set_settings(settings_with_owners(&["200", "201"]));
    let notifier = DiscordNotifier::new(shared);

    notifier
        .notify_owners(OwnerNotice {
            kind: "job_result".to_owned(),
            text: "the nightly job finished".to_owned(),
        })
        .await
        .expect("notify");

    assert_eq!(server.count("POST", "/users/@me/channels"), 2);
    assert_eq!(server.count("POST", "/channels/88/messages"), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_owners_without_owners_is_noowners() {
    let server = FakeServer::start(Box::new(|_| Reply::json(404, "{}")));
    let notifier = DiscordNotifier::new(server.shared());
    let error = notifier
        .notify_owners(OwnerNotice {
            kind: "job_result".to_owned(),
            text: "x".to_owned(),
        })
        .await
        .unwrap_err();
    assert!(matches!(error, NotifyError::NoOwners));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_owners_without_a_rest_client_is_not_connected() {
    let shared = Arc::new(Shared::new());
    shared.set_settings(settings_with_owners(&["200"]));
    let notifier = DiscordNotifier::new(shared);
    let error = notifier
        .notify_owners(OwnerNotice {
            kind: "job_result".to_owned(),
            text: "x".to_owned(),
        })
        .await
        .unwrap_err();
    assert!(matches!(error, NotifyError::NotConnected));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_replies_then_clears_every_reaction() {
    let server = FakeServer::start(Box::new(|request: &Request| {
        match (request.method.as_str(), request.path.as_str()) {
            ("POST", path) if path.ends_with("/channels/7/messages") => Reply::json(200, "{}"),
            ("DELETE", path) if path.contains("/reactions/") => Reply::no_content(),
            _ => Reply::json(404, "{}"),
        }
    }));
    let notifier = DiscordNotifier::new(server.shared());

    notifier
        .interrupted(MessageRef {
            channel: "7".to_owned(),
            message: "500".to_owned(),
        })
        .await
        .expect("interrupted");

    // One reply, then the thinking reaction plus every tool reaction (#29).
    assert_eq!(server.count("POST", "/channels/7/messages"), 1);
    assert_eq!(server.count("DELETE", "/@me"), 1 + TOOL_REACTIONS.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notify_owner_function_reports_when_nothing_parses() {
    // No network: every id is skipped before a request, and `DiscordEndpoints::default()` is
    // never reached.
    let error = notify_owner(
        &SecretString::from("test-token"),
        &["not-a-snowflake".to_owned()],
        "hi",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AdapterError::Settings(_)));
}

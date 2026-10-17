//! Attachment intake: download a Discord attachment into `inbox/discord/`, under a 25 MB cap and
//! a disk floor (architecture §4 `claudeclaw-discord`; TS `downloadDiscordAttachment`,
//! `src/commands/discord.ts:670-690`, plus the caller's cap at `src/commands/discord.ts:655-657`).
//!
//! The download streams to disk rather than buffering the whole body, so a body that lies about
//! its `size` is cut off at the same 25 MB the declared size is checked against (TS buffers with
//! `arrayBuffer()`). Rejections are logged as `discord.attachment_rejected {reason, bytes}`. Files
//! are written `0600`.
//!
//! Two divergences from TS, both because the seam is fixed and `fetch` sees only the attachment:
//!
//! - TS hands each attachment a `type` chosen by the caller, which picks the extension fallback;
//!   here the type is inferred from the attachment's media type.
//! - `IS_VOICE_MESSAGE` (flag `1 << 13`) is not visible: twilight 0.17.1's `AttachmentFlags`
//!   keeps only `IS_REMIX` and truncates the rest on deserialization, so voice detection falls
//!   back to `content_type` starting with `audio/`.
//!
//! The [`reqwest::Client`] is supplied by the caller: building one panics unless a rustls crypto
//! provider is installed (`rustls-no-provider`), so the composition root installs one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::seams::{AttachmentFetcher, FetchError};
use claudeclaw_chat::LocalFile;
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use tokio::io::AsyncWriteExt;
use twilight_model::channel::Attachment;

/// The largest attachment that is downloaded; past it, the Turn runs and Claude is told the file
/// exists instead (TS `MAX_FILE_ATTACHMENT_BYTES`, `src/commands/discord.ts:656`).
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

/// The directory attachments land in, under the project root (TS
/// `join(process.cwd(), ".claude", "claudeclaw", "inbox", "discord")`,
/// `src/commands/discord.ts:674`).
pub const INBOX_DIR: &str = "inbox/discord";

/// The `discord.attachment_rejected` Daemon event
/// (`claudeclaw_api::log_event::daemon::DISCORD_ATTACHMENT_REJECTED`, #23, #44). Not importable:
/// this crate depends only on `chat`.
const ATTACHMENT_REJECTED: &str = "discord.attachment_rejected";

/// Downloads Discord attachments into one inbox directory.
pub struct DiscordAttachmentFetcher {
    client: reqwest::Client,
    inbox: PathBuf,
    max_bytes: u64,
    /// `true` when free disk space is above the floor; from `core`'s disk guard (#44).
    disk_ok: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl DiscordAttachmentFetcher {
    /// A fetcher writing into `inbox`, with the 25 MB cap.
    pub fn new(
        client: reqwest::Client,
        inbox: PathBuf,
        disk_ok: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        DiscordAttachmentFetcher {
            client,
            inbox,
            max_bytes: MAX_ATTACHMENT_BYTES,
            disk_ok,
        }
    }

    /// The fetcher for a project rooted at `root`: writes into `<root>/inbox/discord`.
    pub fn in_project(
        client: reqwest::Client,
        root: &Path,
        disk_ok: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self::new(client, root.join(INBOX_DIR), disk_ok)
    }

    /// Overrides the size cap; tests use a small one.
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// The inbox directory.
    pub fn inbox(&self) -> &Path {
        &self.inbox
    }

    async fn fetch_one(&self, attachment: &Attachment) -> Result<LocalFile, FetchError> {
        if attachment.size > self.max_bytes {
            reject("too_large", attachment.size);
            return Err(FetchError::TooLarge);
        }
        if !(self.disk_ok)() {
            reject("disk_low", attachment.size);
            return Err(FetchError::DiskLow);
        }
        if let Err(err) = tokio::fs::create_dir_all(&self.inbox).await {
            tracing::warn!("the Discord inbox could not be created: {err}");
            reject("write_failed", 0);
            return Err(FetchError::Write);
        }

        let path = self.inbox.join(self.filename(attachment));
        let response = match self.client.get(&attachment.url).send().await {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                tracing::warn!(
                    "a Discord attachment download failed: {}",
                    response.status()
                );
                reject("download_failed", 0);
                return Err(FetchError::Download);
            }
            Err(err) => {
                tracing::warn!("a Discord attachment download failed: {err}");
                reject("download_failed", 0);
                return Err(FetchError::Download);
            }
        };

        let mut file = match open_private(&path).await {
            Ok(file) => file,
            Err(err) => {
                tracing::warn!("a Discord attachment could not be written: {err}");
                reject("write_failed", 0);
                return Err(FetchError::Write);
            }
        };

        let mut received: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    tracing::warn!("a Discord attachment download failed: {err}");
                    drop(file);
                    let _ = tokio::fs::remove_file(&path).await;
                    reject("download_failed", received);
                    return Err(FetchError::Download);
                }
            };
            received += chunk.len() as u64;
            if received > self.max_bytes {
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                reject("too_large", received);
                return Err(FetchError::TooLarge);
            }
            if let Err(err) = file.write_all(&chunk).await {
                tracing::warn!("a Discord attachment could not be written: {err}");
                drop(file);
                let _ = tokio::fs::remove_file(&path).await;
                reject("write_failed", received);
                return Err(FetchError::Write);
            }
        }
        if let Err(err) = file.flush().await {
            tracing::warn!("a Discord attachment could not be written: {err}");
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            reject("write_failed", received);
            return Err(FetchError::Write);
        }

        Ok(LocalFile {
            path,
            name: attachment.filename.clone(),
            mime: attachment.content_type.clone(),
            size_bytes: received,
        })
    }

    /// The local file name: `<attachment id>-<epoch ms><ext>` (TS
    /// `src/commands/discord.ts:681-684`).
    fn filename(&self, attachment: &Attachment) -> String {
        let raw_ext = extension_of(&attachment.filename);
        let ext = if is_plain_extension(raw_ext) {
            raw_ext.to_owned()
        } else if is_voice(attachment) {
            ".ogg".to_owned()
        } else if is_image(attachment) {
            ".jpg".to_owned()
        } else {
            String::new()
        };
        format!("{}-{}{}", attachment.id, epoch_millis(), ext)
    }
}

impl std::fmt::Debug for DiscordAttachmentFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordAttachmentFetcher")
            .field("dir_len", &self.inbox.as_os_str().len())
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

impl AttachmentFetcher for DiscordAttachmentFetcher {
    fn fetch<'a>(
        &'a self,
        attachment: &'a Attachment,
    ) -> BoxFuture<'a, Result<LocalFile, FetchError>> {
        Box::pin(self.fetch_one(attachment))
    }
}

/// Logs a refused attachment as `discord.attachment_rejected {reason, bytes}`.
fn reject(reason: &str, bytes: u64) {
    tracing::warn!(
        event = ATTACHMENT_REJECTED,
        reason = reason,
        bytes = bytes,
        "a Discord attachment was refused"
    );
}

/// The milliseconds since the Unix epoch (TS `Date.now()`).
fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0)
}

/// `path.extname` as Node computes it on a POSIX host: from the last `.` in the basename, unless
/// that `.` is the first character (a dotfile) or absent.
fn extension_of(name: &str) -> &str {
    let base = name.rsplit('/').next().unwrap_or(name);
    match base.rfind('.') {
        None | Some(0) => "",
        Some(index) => &base[index..],
    }
}

/// The TS test `/^\.[A-Za-z0-9]{1,10}$/` (`src/commands/discord.ts:682`) on an extension.
fn is_plain_extension(ext: &str) -> bool {
    match ext.strip_prefix('.') {
        Some(rest) => {
            !rest.is_empty() && rest.len() <= 10 && rest.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// Whether the attachment is an image (TS `isImageAttachment`,
/// `src/commands/discord.ts:639-641`).
fn is_image(attachment: &Attachment) -> bool {
    attachment
        .content_type
        .as_deref()
        .is_some_and(|mime| mime.starts_with("image/"))
}

/// Whether the attachment is a voice message (TS `isVoiceAttachment`,
/// `src/commands/discord.ts:643-647`; see the module note on flag `1 << 13`).
fn is_voice(attachment: &Attachment) -> bool {
    attachment
        .content_type
        .as_deref()
        .is_some_and(|mime| mime.starts_with("audio/"))
}

/// Opens `path` for writing, `0600` on Unix (the inbox is private: #1, #44).
#[cfg(unix)]
async fn open_private(path: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .await
}

/// Opens `path` for writing on Windows, where the inbox directory's own ACL already restricts it.
#[cfg(not(unix))]
async fn open_private(path: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;
    use twilight_model::channel::Attachment;

    use super::*;
    use crate::rest::test_server::Server;

    /// The TLS provider `reqwest`'s `rustls-no-provider` needs before any client is built.
    fn provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    fn client() -> reqwest::Client {
        provider();
        reqwest::Client::new()
    }

    /// A small `channel::Attachment` fixture (only the required fields).
    fn attachment(url: &str, size: u64) -> Attachment {
        let value = serde_json::json!({
            "filename": "photo.png",
            "id": "5",
            "proxy_url": "http://example.invalid/proxy",
            "size": size,
            "url": url,
            "content_type": "image/png",
        });
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn extension_of_matches_node_posix() {
        assert_eq!(extension_of("a.txt"), ".txt");
        assert_eq!(extension_of("a.tar.gz"), ".gz");
        assert_eq!(extension_of("dir/a.png"), ".png");
        assert_eq!(extension_of(".bashrc"), "");
        assert_eq!(extension_of("noext"), "");
        assert_eq!(extension_of("a."), ".");
        assert_eq!(extension_of("a\\b.txt"), ".txt");
    }

    #[test]
    fn plain_extension_matches_the_ts_test() {
        assert!(is_plain_extension(".png"));
        assert!(is_plain_extension(".a1B2c3"));
        assert!(is_plain_extension(".1234567890"));
        assert!(!is_plain_extension("."));
        assert!(!is_plain_extension(".12345678901"));
        assert!(!is_plain_extension(".a-b"));
        assert!(!is_plain_extension("txt"));
    }

    #[tokio::test]
    async fn download_writes_a_private_file() {
        let server = Server::start().await;
        server.canned("GET", "/file", 200, "hello bytes");
        let dir = TempDir::new().unwrap();
        let fetcher =
            DiscordAttachmentFetcher::new(client(), dir.path().to_path_buf(), Arc::new(|| true));
        let url = format!(
            "http://{}/file",
            server
                .endpoints()
                .rest
                .unwrap()
                .trim_start_matches("http://")
        );
        let file = fetcher.fetch(&attachment(&url, 11)).await.unwrap();
        assert_eq!(file.name, "photo.png");
        assert_eq!(file.mime.as_deref(), Some("image/png"));
        assert_eq!(file.size_bytes, 11);
        assert!(file.path.starts_with(dir.path()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file.path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        }
    }

    #[tokio::test]
    async fn declared_size_over_the_cap_is_refused_before_any_request() {
        let server = Server::start().await;
        let dir = TempDir::new().unwrap();
        let fetcher =
            DiscordAttachmentFetcher::new(client(), dir.path().to_path_buf(), Arc::new(|| true));
        let err = fetcher
            .fetch(&attachment(
                "http://127.0.0.1:1/file",
                MAX_ATTACHMENT_BYTES + 1,
            ))
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::TooLarge);
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn a_low_disk_is_refused() {
        let server = Server::start().await;
        let dir = TempDir::new().unwrap();
        let fetcher =
            DiscordAttachmentFetcher::new(client(), dir.path().to_path_buf(), Arc::new(|| false));
        let url = format!(
            "http://{}/file",
            server
                .endpoints()
                .rest
                .unwrap()
                .trim_start_matches("http://")
        );
        let err = fetcher.fetch(&attachment(&url, 3)).await.unwrap_err();
        assert_eq!(err, FetchError::DiskLow);
        assert!(server.requests().is_empty());
    }

    #[tokio::test]
    async fn a_body_over_the_cap_is_cut_off() {
        let server = Server::start().await;
        server.canned("GET", "/file", 200, "0123456789");
        let dir = TempDir::new().unwrap();
        let fetcher =
            DiscordAttachmentFetcher::new(client(), dir.path().to_path_buf(), Arc::new(|| true))
                .with_max_bytes(4);
        let url = format!(
            "http://{}/file",
            server
                .endpoints()
                .rest
                .unwrap()
                .trim_start_matches("http://")
        );
        let err = fetcher.fetch(&attachment(&url, 1)).await.unwrap_err();
        assert_eq!(err, FetchError::TooLarge);
        // The partial file was removed.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[tokio::test]
    async fn a_failed_status_is_a_download_error() {
        let server = Server::start().await;
        server.canned("GET", "/file", 500, "nope");
        let dir = TempDir::new().unwrap();
        let fetcher =
            DiscordAttachmentFetcher::new(client(), dir.path().to_path_buf(), Arc::new(|| true));
        let url = format!(
            "http://{}/file",
            server
                .endpoints()
                .rest
                .unwrap()
                .trim_start_matches("http://")
        );
        let err = fetcher.fetch(&attachment(&url, 3)).await.unwrap_err();
        assert_eq!(err, FetchError::Download);
    }

    #[test]
    fn filename_uses_the_extension_rules() {
        let dir = TempDir::new().unwrap();
        let fetcher =
            DiscordAttachmentFetcher::new(client(), dir.path().to_path_buf(), Arc::new(|| true));
        // A plain extension is kept.
        let image = attachment("http://x", 1);
        assert!(fetcher.filename(&image).ends_with(".png"));
        // A voice file with a bad extension falls back to `.ogg`.
        let mut voice = attachment("http://x", 1);
        voice.filename = "clip.bad-ext".into();
        voice.content_type = Some("audio/ogg".into());
        assert!(fetcher.filename(&voice).ends_with(".ogg"));
        // An image with no extension falls back to `.jpg`.
        let mut bare = attachment("http://x", 1);
        bare.filename = "photo".into();
        assert!(fetcher.filename(&bare).ends_with(".jpg"));
        // Any other file gets no extension.
        let mut other = attachment("http://x", 1);
        other.filename = "notes".into();
        other.content_type = Some("application/pdf".into());
        assert!(!fetcher.filename(&other).ends_with('.'));
    }

    #[test]
    fn debug_is_content_free() {
        let dir = TempDir::new().unwrap();
        let fetcher = DiscordAttachmentFetcher::new(
            client(),
            dir.path().join("secret-inbox"),
            Arc::new(|| true),
        );
        let got = format!("{fetcher:?}");
        assert!(!got.contains("secret"), "{got}");
        assert!(!got.contains("inbox"), "{got}");
    }
}

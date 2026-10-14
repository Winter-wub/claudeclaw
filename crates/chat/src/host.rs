//! What `core` offers every Chat adapter: [`ChatHost`] and the controls behind it
//! (architecture §6).

use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use futures_util::future::BoxFuture;
use secrecy::SecretString;
use tokio::sync::watch;

use crate::{ChatOutcome, ChatScope, ControlError, RunHandle, Transport, Turn, UtilityHandle};

/// Implemented by `core` and given to every Chat adapter (architecture §6).
pub trait ChatHost: Send + Sync {
    /// Queues `turn` on its Scope's serial queue and returns the handle its events and outcome
    /// arrive on.
    fn converse(&self, turn: Turn) -> RunHandle;
    /// Starts a sessionless run of `prompt`, such as the thread-intent classifier.
    fn utility(&self, prompt: String) -> UtilityHandle;
    /// Session commands: reset, compact, status, context usage.
    fn sessions(&self) -> &dyn SessionControl;
    /// Launcher commands: show, set, inherit, Test launcher.
    fn launchers(&self) -> &dyn LauncherControl;
    /// The transports' settings, with hot reload (§7).
    fn settings(&self) -> &dyn SettingsWatch;
}

/// Session commands a Chat adapter maps its slash or bot commands onto (architecture §6).
/// Each acts on the session of the Scope's effective Launcher.
pub trait SessionControl: Send + Sync {
    /// Ends the Scope's session, so its next Turn starts a new one.
    fn reset(&self, scope: ChatScope) -> BoxFuture<'_, Result<(), ControlError>>;
    /// Runs `/compact` on the Scope's session, queued like a Turn (#40 D7).
    fn compact(&self, scope: ChatScope) -> BoxFuture<'_, Result<ChatOutcome, ControlError>>;
    /// The Scope's session; `None` when it has none.
    fn status(
        &self,
        scope: ChatScope,
    ) -> BoxFuture<'_, Result<Option<SessionStatus>, ControlError>>;
    /// The context window use of the session's last run, from the Daemon's own stream (#39);
    /// `None` when there is no session or no run of it has reported usage yet.
    fn context(
        &self,
        scope: ChatScope,
    ) -> BoxFuture<'_, Result<Option<ContextUsage>, ControlError>>;
}

/// Launcher commands a Chat adapter maps its slash or bot commands onto (architecture §6).
/// A Launcher is `"direct"` or an absolute path (#8).
pub trait LauncherControl: Send + Sync {
    /// The Scope's effective Launcher and where it comes from.
    fn show(&self, scope: ChatScope) -> BoxFuture<'_, Result<LauncherView, ControlError>>;
    /// Sets the Scope's own Launcher. `core` validates it (runnable; a path not in the
    /// [`picker`](Self::picker) list needs `author_allowed`, the adapter's allow-list check on
    /// whoever typed it) and returns the new view.
    fn set(
        &self,
        scope: ChatScope,
        launcher: String,
        author_allowed: bool,
    ) -> BoxFuture<'_, Result<LauncherView, ControlError>>;
    /// Drops the Scope's own Launcher so it follows the resolution chain again.
    fn inherit(&self, scope: ChatScope) -> BoxFuture<'_, Result<LauncherView, ControlError>>;
    /// Runs Test launcher on the Scope's effective Launcher (management-api §2.5).
    fn test(&self, scope: ChatScope) -> BoxFuture<'_, Result<LauncherTest, ControlError>>;
    /// The Launcher picker list: `direct` plus every path already used by a Scope
    /// (§4 `claudeclaw-core`).
    fn picker(&self) -> BoxFuture<'_, Result<Vec<String>, ControlError>>;
}

/// Each transport's settings, republished on every reload (architecture §6, §7).
pub trait SettingsWatch: Send + Sync {
    /// The current settings of `transport`, marked changed on each reload that changes them.
    fn subscribe(&self, transport: Transport) -> watch::Receiver<TransportSettings>;
}

/// One transport's settings as an adapter sees them (architecture §7).
///
/// `core` resolves precedence (env over `settings.json`, §7) before publishing, so `token` is
/// already the effective one. `section` is the raw JSON text of the transport's own object in
/// `settings.json` (`"discord": {…}`), which the adapter parses itself: `chat` knows no
/// transport's settings shape. `Debug` prints neither.
#[derive(Clone, Default)]
pub struct TransportSettings {
    /// The bot token; `None` when the transport isn't configured.
    pub token: Option<SecretString>,
    /// The transport's `settings.json` object as JSON text; `"{}"` or empty when absent.
    pub section: Arc<str>,
}

impl fmt::Debug for TransportSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportSettings")
            .field("has_token", &self.token.is_some())
            .field("section_bytes", &self.section.len())
            .finish()
    }
}

/// A Scope's session, for a status command (architecture §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatus {
    pub session_id: String,
    /// `"direct"` or the Launcher path the session belongs to.
    pub launcher: String,
    /// The model setting in effect; `None` for Claude Code's default.
    pub model: Option<String>,
    pub turns: u32,
    pub created_at: SystemTime,
    pub last_used_at: SystemTime,
}

/// Context window use of a session's last run (architecture §6, #39).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    pub input_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    /// The model's context window.
    pub window_tokens: u64,
}

impl ContextUsage {
    /// Tokens in context: input plus cache writes plus cache reads.
    pub fn used_tokens(&self) -> u64 {
        self.input_tokens + self.cache_write_tokens + self.cache_read_tokens
    }
}

/// A Scope's effective Launcher (architecture §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherView {
    /// `"direct"` or a Launcher path.
    pub launcher: String,
    pub source: LauncherSource,
    /// The model setting in effect; `None` for Claude Code's default.
    pub model: Option<String>,
}

/// Where in the resolution chain (thread → parent channel → global, §4 `claudeclaw-core`) a
/// Scope's Launcher comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LauncherSource {
    /// The Scope's own setting.
    Scope,
    /// Inherited from the thread's parent channel.
    ParentChannel,
    /// The global default.
    Global,
}

/// The result of Test launcher (management-api §2.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherTest {
    /// `"direct"` or the Launcher path tested.
    pub launcher: String,
    /// `false` if any check that isn't advisory failed.
    pub ok: bool,
    /// The checks in the order they ran.
    pub checks: Vec<LauncherCheck>,
}

/// One Test launcher check (management-api §2.5). `detail` never shows the Launcher's
/// arguments (#8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LauncherCheck {
    /// The check's id, such as `runnable` or `print_stream_json`.
    pub id: String,
    pub ok: bool,
    /// An advisory check never makes the result's `ok` false (#39 D6).
    pub advisory: bool,
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_usage_sums_the_window() {
        let usage = ContextUsage {
            input_tokens: 10,
            cache_write_tokens: 200,
            cache_read_tokens: 3000,
            window_tokens: 200_000,
        };
        assert_eq!(usage.used_tokens(), 3210);
    }

    #[test]
    fn settings_debug_hides_token_and_section() {
        let settings = TransportSettings {
            token: Some(SecretString::from("tok-hidden")),
            section: Arc::from(r#"{"allowedUserIds":["hidden"]}"#),
        };
        let got = format!("{settings:?}");
        assert!(!got.contains("hidden"), "{got}");
        assert!(got.contains("has_token: true"), "{got}");
    }

    #[test]
    fn settings_reload_reaches_subscribers() {
        let (tx, mut rx) = watch::channel(TransportSettings::default());
        assert!(rx.borrow_and_update().token.is_none());
        tx.send_replace(TransportSettings {
            token: Some(SecretString::from("tok")),
            section: Arc::from("{}"),
        });
        assert!(rx.has_changed().unwrap());
        assert!(rx.borrow_and_update().token.is_some());
    }
}

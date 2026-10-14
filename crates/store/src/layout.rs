//! `Layout`: every path under a project dir (architecture §9.1), `.claude/settings.json` (#24),
//! and the local API address rule (§9.3).
//!
//! `Layout` is the only place that knows paths under the project dir; every other crate asks it.
//! It does no IO, except where a method says so.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::atomic::{self, DirMode};
use crate::error::StoreError;

/// An absolute socket path must be shorter than this to be used in-dir (§9.3). macOS refuses to
/// bind at 104 bytes and Linux at 108 (#11's evidence), so 100 leaves room on both.
pub const SOCKET_PATH_MAX: usize = 100;

/// The paths of one project dir: one Daemon per dir (#9, architecture §9.1).
///
/// The project dir is used as given. The CLI resolves and canonicalises it first (`--dir`, then
/// `CLAUDECLAW_DIR`, then cwd; #22), and the §9.3 hash is over exactly these bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    project: PathBuf,
    root: PathBuf,
}

impl Layout {
    /// The layout of `project_dir`.
    pub fn new(project_dir: impl Into<PathBuf>) -> Self {
        let project = project_dir.into();
        let root = project.join(".claude").join("claudeclaw");
        Layout { project, root }
    }

    /// `<project-dir>`.
    pub fn project_dir(&self) -> &Path {
        &self.project
    }

    /// `<project-dir>/.claude/claudeclaw/`, where TS and Rust share state (§9.1).
    pub fn root(&self) -> &Path {
        &self.root
    }

    // ---- The project dir (§9.1) -------------------------------------------------------------

    /// `CLAUDE.md`, read every run (S).
    pub fn claude_md(&self) -> PathBuf {
        self.project.join("CLAUDE.md")
    }

    /// `agents/`, one cwd per agent (S).
    pub fn agents_dir(&self) -> PathBuf {
        self.project.join("agents")
    }

    /// `agents/<agent>/`.
    pub fn agent_dir(&self, agent: &str) -> PathBuf {
        self.agents_dir().join(agent)
    }

    /// `agents/<agent>/jobs/`, the agent's job files.
    pub fn agent_jobs_dir(&self, agent: &str) -> PathBuf {
        self.agent_dir(agent).join("jobs")
    }

    /// `agents/<agent>/session.json`, the agent's session mirror (#14 rule 7).
    pub fn agent_session(&self, agent: &str) -> PathBuf {
        self.agent_dir(agent).join("session.json")
    }

    /// `agents/.archive/`, fired threads and archived agents (#16, #21).
    pub fn agents_archive_dir(&self) -> PathBuf {
        self.agents_dir().join(".archive")
    }

    /// `.claude/`.
    pub fn claude_dir(&self) -> PathBuf {
        self.project.join(".claude")
    }

    /// `.claude/settings.json`, Claude Code's project settings (C). Rust patches only its
    /// `statusLine` key (#24 D6).
    pub fn claude_settings(&self) -> PathBuf {
        self.claude_dir().join("settings.json")
    }

    /// `.claude/statusline.cjs` (T): never written or deleted by Rust (#24).
    pub fn ts_statusline_script(&self) -> PathBuf {
        self.claude_dir().join("statusline.cjs")
    }

    // ---- `.claude/claudeclaw/`, shared with TS (§9.1) -----------------------------------------

    /// `settings.json` (S), 0600 (§7, §9.1).
    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// `session.json` (S): the global session mirror (#14 rule 7).
    pub fn session(&self) -> PathBuf {
        self.root.join("session.json")
    }

    /// `sessions.json` (S): the thread session mirror (#14 rule 7).
    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions.json")
    }

    /// `session_fallback.json` (S).
    pub fn session_fallback(&self) -> PathBuf {
        self.root.join("session_fallback.json")
    }

    /// `fallback-sessions/` (S).
    pub fn fallback_sessions_dir(&self) -> PathBuf {
        self.root.join("fallback-sessions")
    }

    /// `session_<n>.backup` (S); the newest 20 are kept (#44).
    pub fn session_backup(&self, n: u32) -> PathBuf {
        self.root.join(format!("session_{n}.backup"))
    }

    /// `pending-resume.json` (S), consumed as TS does.
    pub fn pending_resume(&self) -> PathBuf {
        self.root.join("pending-resume.json")
    }

    /// `permission-mode.json` (S), read only.
    pub fn permission_mode(&self) -> PathBuf {
        self.root.join("permission-mode.json")
    }

    /// `jobs/` (S): job files, line-edited and renamed, never re-serialised.
    pub fn jobs_dir(&self) -> PathBuf {
        self.root.join("jobs")
    }

    /// `prompts/HEARTBEAT.md` (S), read only.
    pub fn heartbeat_prompt(&self) -> PathBuf {
        self.root.join("prompts").join("HEARTBEAT.md")
    }

    /// `inbox/discord/` (S): Discord attachments, `<snowflake>-<epoch ms><ext>`.
    pub fn discord_inbox(&self) -> PathBuf {
        self.root.join("inbox").join("discord")
    }

    /// `inbox/api/` (S): API and CLI attachments, `<turn_id>-<index><ext>` (#21, #44).
    pub fn api_inbox(&self) -> PathBuf {
        self.root.join("inbox").join("api")
    }

    /// `outbox/discord/` (S): the image upload root, never deleted.
    pub fn discord_outbox(&self) -> PathBuf {
        self.root.join("outbox").join("discord")
    }

    /// `logs/` (S): Run logs in the TS pattern (#14 rule 5); `logs/<agent>/` for agent jobs.
    pub fn run_logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// `logs/daemon.log` (T): TS's stdout redirect; Rust never writes it.
    pub fn ts_daemon_log(&self) -> PathBuf {
        self.run_logs_dir().join("daemon.log")
    }

    /// `state.json` (T): never written; a leftover is deleted at Worker start (#24).
    pub fn ts_state(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// `active-runs` (T): never written by Rust.
    pub fn ts_active_runs(&self) -> PathBuf {
        self.root.join("active-runs")
    }

    /// `web.token` (T): never written by Rust.
    pub fn ts_web_token(&self) -> PathBuf {
        self.root.join("web.token")
    }

    /// `daemon.pid` (S): `"<watchdog pid>\n"` (#14 rule 10).
    pub fn daemon_pid(&self) -> PathBuf {
        self.root.join("daemon.pid")
    }

    /// `daemon.lock` (R): the Watchdog's OS file lock and the holder record (#9, #22).
    pub fn daemon_lock(&self) -> PathBuf {
        self.root.join("daemon.lock")
    }

    /// `api.sock` (R): the in-dir local socket. Use [`Layout::api_endpoint`] to get the address
    /// the §9.3 rule picks.
    pub fn in_dir_socket(&self) -> PathBuf {
        self.root.join("api.sock")
    }

    /// `import-backup/<ts>/` (R), 0700 with 0600 files (#12, #17).
    pub fn import_backup_dir(&self, ts: &str) -> PathBuf {
        self.root.join("import-backup").join(ts)
    }

    /// `import-report-<ts>.md` (R) (#17).
    pub fn import_report(&self, ts: &str) -> PathBuf {
        self.root.join(format!("import-report-{ts}.md"))
    }

    // ---- `daemon/`, Rust-only, 0700 (§9.1) ----------------------------------------------------

    /// `daemon/`, the one 0700 boundary around everything only Rust owns.
    pub fn daemon_dir(&self) -> PathBuf {
        self.root.join("daemon")
    }

    /// `daemon/state/`.
    pub fn state_dir(&self) -> PathBuf {
        self.daemon_dir().join("state")
    }

    /// `daemon/state/jobs.json`: per-job state (#9, #46).
    pub fn job_state(&self) -> PathBuf {
        self.state_dir().join("jobs.json")
    }

    /// `daemon/state/sessions.json`: the (Scope, Launcher) session store (#8, #32, #33, #39).
    pub fn session_state(&self) -> PathBuf {
        self.state_dir().join("sessions.json")
    }

    /// `daemon/state/supervisor.json`: the Watchdog's status and Crash record (#20).
    pub fn supervisor(&self) -> PathBuf {
        self.state_dir().join("supervisor.json")
    }

    /// `daemon/state/runs.json`: the Run registry (#22, #29).
    pub fn run_registry(&self) -> PathBuf {
        self.state_dir().join("runs.json")
    }

    /// `daemon/state/import.json`: the First-start import record (#17).
    pub fn import_record(&self) -> PathBuf {
        self.state_dir().join("import.json")
    }

    /// `daemon/state/launcher-tests.json`: the last Test launcher result per Launcher (#21).
    pub fn launcher_tests(&self) -> PathBuf {
        self.state_dir().join("launcher-tests.json")
    }

    /// `daemon/state/notices.json`: content hashes already notified.
    pub fn notices(&self) -> PathBuf {
        self.state_dir().join("notices.json")
    }

    /// `daemon/launchers/`: Launcher scripts from the First-start import (#17).
    pub fn launchers_dir(&self) -> PathBuf {
        self.daemon_dir().join("launchers")
    }

    /// `daemon/tmp/`: per-run System-prompt files, 0600 (#25).
    pub fn tmp_dir(&self) -> PathBuf {
        self.daemon_dir().join("tmp")
    }

    /// `daemon/runs/`: the Run index (#23).
    pub fn run_index_dir(&self) -> PathBuf {
        self.daemon_dir().join("runs")
    }

    /// `daemon/runs/<YYYY-MM>.jsonl`: one Run index month (#23, #44).
    pub fn run_index(&self, month: &str) -> PathBuf {
        self.run_index_dir().join(format!("{month}.jsonl"))
    }

    /// `daemon/secrets/`, 0700 with 0600 files (§9.1).
    pub fn secrets_dir(&self) -> PathBuf {
        self.daemon_dir().join("secrets")
    }

    /// `daemon/secrets/devices.json`: paired Devices (#21).
    pub fn devices(&self) -> PathBuf {
        self.secrets_dir().join("devices.json")
    }

    /// `daemon/secrets/api-tokens.json`: API token hashes (#21).
    pub fn api_tokens(&self) -> PathBuf {
        self.secrets_dir().join("api-tokens.json")
    }

    /// `daemon/secrets/tls/daemon.key`.
    pub fn tls_key(&self) -> PathBuf {
        self.secrets_dir().join("tls").join("daemon.key")
    }

    /// `daemon/secrets/tls/daemon.crt`.
    pub fn tls_cert(&self) -> PathBuf {
        self.secrets_dir().join("tls").join("daemon.crt")
    }

    /// `daemon/logs/`: the Daemon logs, `worker.<date>.jsonl` and `watchdog.<date>.jsonl` (#23).
    pub fn daemon_logs_dir(&self) -> PathBuf {
        self.daemon_dir().join("logs")
    }

    /// `daemon/audit/`: the append-only audit log (#13).
    pub fn audit_dir(&self) -> PathBuf {
        self.daemon_dir().join("audit")
    }

    /// `daemon/audit/<YYYY-MM>.jsonl`.
    pub fn audit_log(&self, month: &str) -> PathBuf {
        self.audit_dir().join(format!("{month}.jsonl"))
    }

    /// Whether `path` lies under `daemon/secrets/`, where every file is 0600 (§9.1).
    pub fn is_secret(&self, path: &Path) -> bool {
        path.starts_with(self.secrets_dir())
    }

    /// Creates `daemon/` and `daemon/secrets/` at 0700 (§9.1), and the other `daemon/` subdirs.
    /// IO.
    pub fn create_daemon_dirs(&self) -> Result<(), StoreError> {
        atomic::create_dir(&self.root, DirMode::Default)?;
        atomic::create_dir(&self.daemon_dir(), DirMode::Private)?;
        atomic::create_dir(&self.secrets_dir(), DirMode::Private)?;
        for dir in [
            self.state_dir(),
            self.launchers_dir(),
            self.tmp_dir(),
            self.run_index_dir(),
            self.daemon_logs_dir(),
            self.audit_dir(),
        ] {
            atomic::create_dir(&dir, DirMode::Default)?;
        }
        Ok(())
    }

    // ---- The local API address (§9.3) ---------------------------------------------------------

    /// The first 16 hex of SHA-256 over the project dir's bytes (§9.3): the name of the hashed
    /// socket and of the Windows pipe.
    pub fn dir_hash(&self) -> String {
        let digest = Sha256::digest(self.project.as_os_str().as_encoded_bytes());
        digest[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The local Management API address for this OS (§9.3): a named pipe on Windows, otherwise
    /// [`Layout::socket_path`].
    pub fn api_endpoint(&self, env: &RuntimeEnv) -> ApiEndpoint {
        if cfg!(windows) {
            ApiEndpoint::Pipe(self.pipe_name())
        } else {
            ApiEndpoint::Socket(self.socket_path(env))
        }
    }

    /// The Unix socket path (§9.3, #11): `.claude/claudeclaw/api.sock` when its absolute path is
    /// under [`SOCKET_PATH_MAX`] bytes, else `<runtime>/claudeclaw-<uid>/<dir hash>.sock`.
    pub fn socket_path(&self, env: &RuntimeEnv) -> PathBuf {
        let in_dir = self.in_dir_socket();
        if in_dir.as_os_str().len() < SOCKET_PATH_MAX {
            in_dir
        } else {
            env.socket_dir().join(format!("{}.sock", self.dir_hash()))
        }
    }

    /// `\\.\pipe\claudeclaw-<dir hash>`, the Windows pipe name (§9.3).
    pub fn pipe_name(&self) -> String {
        format!(r"\\.\pipe\claudeclaw-{}", self.dir_hash())
    }

    /// `logs/<agent>/` (S): the Run logs of the agent's jobs, named `<agent>/<label>` (#14
    /// rule 5, #17 §4).
    pub fn agent_run_logs_dir(&self, agent: &str) -> PathBuf {
        self.run_logs_dir().join(agent)
    }

    /// The flat jobs dir for a `jobsDir` setting (#46 D2), as TS resolves it
    /// (`src/config.ts:22-23`, `:456`): the trimmed value, under the project dir unless it is
    /// absolute (Node's `path.isAbsolute`, so a rooted `/abs` counts on Windows too). Unset or
    /// blank gives [`Layout::jobs_dir`].
    pub fn jobs_dir_for(&self, jobs_dir: Option<&str>) -> PathBuf {
        match jobs_dir.map(str::trim).filter(|dir| !dir.is_empty()) {
            Some(dir) if Path::new(dir).is_absolute() || Path::new(dir).has_root() => dir.into(),
            Some(dir) => self.project.join(dir),
            None => self.jobs_dir(),
        }
    }

    /// `daemon/logs/<process>.<YYYY-MM-DD>.jsonl`: one day of the `worker` or `watchdog` Daemon
    /// log (#23 D2).
    pub fn daemon_log(&self, process: &str, date: &str) -> PathBuf {
        self.daemon_logs_dir()
            .join(format!("{process}.{date}.jsonl"))
    }
}

/// Where the local Management API listens (§9.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiEndpoint {
    /// A Unix socket path.
    Socket(PathBuf),
    /// A Windows named pipe name.
    Pipe(String),
}

/// The process facts the hashed socket path depends on (§9.3). Kept apart from [`Layout`] so the
/// rule is a pure function and the CLI, the Watchdog and tests compute it the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEnv {
    /// `$XDG_RUNTIME_DIR`, if set and not empty.
    pub xdg_runtime_dir: Option<PathBuf>,
    /// `$TMPDIR`, if set and not empty.
    pub tmpdir: Option<PathBuf>,
    /// The effective user id.
    pub uid: u32,
}

impl RuntimeEnv {
    /// This process's env: `$XDG_RUNTIME_DIR`, `$TMPDIR` and the effective uid. On Windows the
    /// uid is 0; the pipe name doesn't use it.
    pub fn current() -> Self {
        #[cfg(unix)]
        let uid = nix::unistd::geteuid().as_raw();
        #[cfg(not(unix))]
        let uid = 0;
        RuntimeEnv {
            xdg_runtime_dir: env_path("XDG_RUNTIME_DIR"),
            tmpdir: env_path("TMPDIR"),
            uid,
        }
    }

    /// `<runtime>`: `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else `/tmp` (§9.3).
    pub fn runtime_dir(&self) -> &Path {
        self.xdg_runtime_dir
            .as_deref()
            .or(self.tmpdir.as_deref())
            .unwrap_or(Path::new("/tmp"))
    }

    /// `<runtime>/claudeclaw-<uid>/`, the dir that holds hashed sockets (§9.3).
    pub fn socket_dir(&self) -> PathBuf {
        self.runtime_dir().join(format!("claudeclaw-{}", self.uid))
    }

    /// Creates [`RuntimeEnv::socket_dir`] at 0700 and checks it is owned by `uid` and not
    /// group- or world-accessible, so another user can't pre-create it (§9.3). IO; Unix only.
    #[cfg(unix)]
    pub fn create_socket_dir(&self) -> Result<PathBuf, StoreError> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};

        let dir = self.socket_dir();
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(StoreError::io("create dir", &dir)(e)),
        }
        let meta = std::fs::symlink_metadata(&dir).map_err(StoreError::io("stat", &dir))?;
        if !meta.is_dir() {
            return Err(StoreError::Refused {
                path: dir,
                reason: "not a directory",
            });
        }
        if meta.uid() != self.uid {
            return Err(StoreError::Refused {
                path: dir,
                reason: "owned by another user",
            });
        }
        if meta.mode() & 0o077 != 0 {
            return Err(StoreError::Refused {
                path: dir,
                reason: "accessible to other users",
            });
        }
        Ok(dir)
    }
}

/// Reads an env var as a path, treating an empty value as unset.
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> RuntimeEnv {
        RuntimeEnv {
            xdg_runtime_dir: None,
            tmpdir: Some("/tmp/x".into()),
            uid: 501,
        }
    }

    /// A project dir whose in-dir socket path is exactly `len` bytes.
    fn project_with_socket_len(len: usize) -> Layout {
        let suffix = "/.claude/claudeclaw/api.sock".len();
        let dir = format!("/{}", "p".repeat(len - suffix - 1));
        let layout = Layout::new(dir);
        assert_eq!(layout.in_dir_socket().as_os_str().len(), len);
        layout
    }

    #[test]
    fn paths_follow_the_section_9_tree() {
        let l = Layout::new("/p");
        assert_eq!(l.claude_settings(), Path::new("/p/.claude/settings.json"));
        assert_eq!(
            l.settings(),
            Path::new("/p/.claude/claudeclaw/settings.json")
        );
        assert_eq!(
            l.daemon_lock(),
            Path::new("/p/.claude/claudeclaw/daemon.lock")
        );
        assert_eq!(
            l.daemon_pid(),
            Path::new("/p/.claude/claudeclaw/daemon.pid")
        );
        assert_eq!(
            l.supervisor(),
            Path::new("/p/.claude/claudeclaw/daemon/state/supervisor.json")
        );
        assert_eq!(
            l.devices(),
            Path::new("/p/.claude/claudeclaw/daemon/secrets/devices.json")
        );
        assert_eq!(
            l.run_index("2026-10"),
            Path::new("/p/.claude/claudeclaw/daemon/runs/2026-10.jsonl")
        );
        assert_eq!(
            l.session_backup(3),
            Path::new("/p/.claude/claudeclaw/session_3.backup")
        );
        assert_eq!(
            l.agent_session("ops"),
            Path::new("/p/agents/ops/session.json")
        );
        assert!(l.is_secret(&l.tls_key()));
        assert!(!l.is_secret(&l.settings()));
    }

    #[test]
    fn a_socket_path_under_100_bytes_stays_in_dir() {
        let layout = project_with_socket_len(SOCKET_PATH_MAX - 1);
        assert_eq!(layout.socket_path(&env()), layout.in_dir_socket());
    }

    #[test]
    fn a_socket_path_of_100_bytes_moves_to_the_runtime_dir() {
        let layout = project_with_socket_len(SOCKET_PATH_MAX);
        let path = layout.socket_path(&env());
        assert_eq!(
            path,
            Path::new("/tmp/x/claudeclaw-501").join(format!("{}.sock", layout.dir_hash()))
        );
        assert!(path.as_os_str().len() < SOCKET_PATH_MAX);
    }

    #[test]
    fn the_runtime_dir_prefers_xdg_then_tmpdir_then_tmp() {
        let mut e = env();
        e.xdg_runtime_dir = Some("/run/user/501".into());
        assert_eq!(e.socket_dir(), Path::new("/run/user/501/claudeclaw-501"));
        e.xdg_runtime_dir = None;
        assert_eq!(e.socket_dir(), Path::new("/tmp/x/claudeclaw-501"));
        e.tmpdir = None;
        assert_eq!(e.socket_dir(), Path::new("/tmp/claudeclaw-501"));
    }

    #[test]
    fn the_hash_is_16_hex_of_sha256_over_the_dir() {
        let layout = Layout::new("/p");
        // `printf %s /p | shasum -a 256`
        assert_eq!(layout.dir_hash(), "00d74baf14ea415c");
        assert_eq!(layout.pipe_name(), r"\\.\pipe\claudeclaw-00d74baf14ea415c");
        assert_ne!(Layout::new("/q").dir_hash(), layout.dir_hash());
    }

    #[test]
    fn the_endpoint_is_a_pipe_only_on_windows() {
        let layout = Layout::new("/p");
        let endpoint = layout.api_endpoint(&env());
        if cfg!(windows) {
            assert_eq!(endpoint, ApiEndpoint::Pipe(layout.pipe_name()));
        } else {
            assert_eq!(endpoint, ApiEndpoint::Socket(layout.in_dir_socket()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn create_daemon_dirs_applies_0700() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        layout.create_daemon_dirs().unwrap();
        for d in [layout.daemon_dir(), layout.secrets_dir()] {
            let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", d.display());
        }
        assert!(layout.state_dir().is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_dir_is_0700_and_a_widened_one_is_refused() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let uid = std::fs::metadata(dir.path()).unwrap().uid();
        let env = RuntimeEnv {
            xdg_runtime_dir: Some(dir.path().into()),
            tmpdir: None,
            uid,
        };
        let created = env.create_socket_dir().unwrap();
        let mode = std::fs::metadata(&created).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        // Existing and correct: accepted again.
        env.create_socket_dir().unwrap();

        crate::atomic::set_mode(&created, 0o755).unwrap();
        assert!(matches!(
            env.create_socket_dir(),
            Err(StoreError::Refused { .. })
        ));

        let other = RuntimeEnv {
            uid: uid + 1,
            ..env
        };
        std::fs::create_dir(other.socket_dir()).unwrap();
        assert!(matches!(
            other.create_socket_dir(),
            Err(StoreError::Refused {
                reason: "owned by another user",
                ..
            })
        ));
    }
}

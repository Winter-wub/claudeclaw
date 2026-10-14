//! The login-shell capture of the Run PATH and the `__env-probe` output contract (#26 D2).
//!
//! # The `__env-probe` contract
//!
//! The Worker runs `<shell> -l -i -c "'<exe>' __env-probe <nonce>"`, where `<exe>` is the
//! Daemon's own binary. The binary's hidden `__env-probe <nonce>` subcommand prints, on stdout:
//!
//! ```text
//! <nonce>
//! {"PATH":"<the PATH it was started with>"}
//! <nonce>
//! ```
//!
//! - The JSON is one object. Only its `PATH` key is read (#26 D1, #43 D2); other keys are
//!   ignored, so the object may grow without breaking an older reader.
//! - `PATH` is a string, or `null` when the probe had no PATH or a PATH that isn't valid
//!   Unicode. `null` is a `bad_output` failure.
//! - Anything the user's rc files print before the first or after the second marker is
//!   ignored. The probe's exit code is not read.
//!
//! [`encode_probe_output`] builds those bytes for the binary, and [`parse_probe_output`] reads
//! them back. `exe` is quoted with `'`, so a binary path containing `'` fails the capture with
//! `unsafe_exe_path` rather than being escaped.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The hidden subcommand the login shell runs (#26 D2, #22).
pub const ENV_PROBE_SUBCOMMAND: &str = "__env-probe";

/// How a Run PATH capture failed (#26 D2). Mirrors `claudeclaw_api::status::RunPathErrorKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureErrorKind {
    SpawnFailed,
    Timeout,
    /// The two nonce markers weren't both on stdout, e.g. the rc file `exec`s tmux.
    NoMarker,
    BadOutput,
    /// The Daemon's binary path contains `'`.
    UnsafeExePath,
}

impl CaptureErrorKind {
    /// The wire name, as in status and `run_path.capture_failed {kind}`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpawnFailed => "spawn_failed",
            Self::Timeout => "timeout",
            Self::NoMarker => "no_marker",
            Self::BadOutput => "bad_output",
            Self::UnsafeExePath => "unsafe_exe_path",
        }
    }
}

/// A failed capture (#26 D2). The Display never includes the probe's output.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("the login shell could not be started")]
    SpawnFailed(#[source] std::io::Error),
    #[error("the login shell did not finish within {0:?}")]
    Timeout(Duration),
    #[error("the login shell's output has no env-probe markers")]
    NoMarker,
    #[error("the env probe's output is not the expected JSON")]
    BadOutput,
    #[error("the Daemon's binary path contains a single quote")]
    UnsafeExePath,
}

impl CaptureError {
    pub fn kind(&self) -> CaptureErrorKind {
        match self {
            Self::SpawnFailed(_) => CaptureErrorKind::SpawnFailed,
            Self::Timeout(_) => CaptureErrorKind::Timeout,
            Self::NoMarker => CaptureErrorKind::NoMarker,
            Self::BadOutput => CaptureErrorKind::BadOutput,
            Self::UnsafeExePath => CaptureErrorKind::UnsafeExePath,
        }
    }
}

/// The bytes `__env-probe <nonce>` prints (see the module docs). `path` is the probe's own
/// `PATH`; `None` or a non-Unicode value is written as `null`.
pub fn encode_probe_output(nonce: &str, path: Option<&OsStr>) -> String {
    let value = path
        .and_then(OsStr::to_str)
        .map_or(serde_json::Value::Null, serde_json::Value::from);
    let json = serde_json::json!({ "PATH": value });
    format!("{nonce}\n{json}\n{nonce}\n")
}

/// Reads the PATH out of a login shell's stdout (see the module docs).
///
/// The nonce is also part of the shell's `-c` string, so an rc hook that echoes the command
/// (a terminal-title `DEBUG` trap, bash-preexec) can print it outside the probe's own markers.
/// Each pair of consecutive nonce occurrences is therefore tried in turn, and the first whose
/// body is the expected JSON wins. Fewer than two occurrences is `no_marker`; no good pair is
/// `bad_output`.
pub fn parse_probe_output(stdout: &[u8], nonce: &str) -> Result<OsString, CaptureError> {
    let marker = nonce.as_bytes();
    let mut open = find(stdout, marker, 0).ok_or(CaptureError::NoMarker)?;
    let mut paired = false;
    while let Some(close) = find(stdout, marker, open + marker.len()) {
        paired = true;
        if let Some(path) = parse_probe_body(&stdout[open + marker.len()..close]) {
            return Ok(path);
        }
        open = close;
    }
    Err(if paired {
        CaptureError::BadOutput
    } else {
        CaptureError::NoMarker
    })
}

/// The PATH from the JSON between two markers, if that is what the body is.
fn parse_probe_body(body: &[u8]) -> Option<OsString> {
    let body = std::str::from_utf8(body).ok()?;
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    match value.get("PATH") {
        Some(serde_json::Value::String(path)) => Some(OsString::from(path)),
        _ => None,
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

/// A fresh nonce for one capture. It only has to be unlikely in rc-file output, not secret.
pub fn fresh_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("claudeclaw-env-probe-{:x}-{nanos:x}", std::process::id())
}

/// Picks the login shell (#26 D2): the passwd entry's shell, else `$SHELL`, else `/bin/sh`.
/// `None` when the chosen shell is `nologin` or `false`, as for service accounts; the caller
/// then also checks it is executable.
pub fn choose_shell(passwd_shell: Option<&Path>, env_shell: Option<&OsStr>) -> Option<PathBuf> {
    let shell = passwd_shell
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| env_shell.filter(|s| !s.is_empty()).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    match shell.file_name().and_then(OsStr::to_str) {
        Some("nologin" | "false") => None,
        _ => Some(shell),
    }
}

/// One login-shell capture (#26 D2).
#[derive(Debug, Clone)]
pub struct LoginShellProbe {
    /// The shell to run as `<shell> -l -i -c …`.
    pub shell: PathBuf,
    /// The binary the shell runs as `'<exe>' __env-probe <nonce>`.
    pub exe: PathBuf,
    /// The probe's cwd: the home dir, so a project's `.envrc` doesn't leak in.
    pub home: PathBuf,
    pub nonce: String,
    /// The whole process group is killed after this long.
    pub timeout: Duration,
}

impl LoginShellProbe {
    /// The capture timeout (#26 D2).
    pub const TIMEOUT: Duration = Duration::from_secs(10);
    /// How much of the shell's stdout is kept (#26 D2).
    pub const STDOUT_CAP: usize = 64 * 1024;

    /// The probe for the current user: the shell from [`choose_shell`] (the passwd entry via
    /// `getpwuid_r`), the home dir from the passwd entry (else `$HOME`), a fresh nonce and the
    /// 10 s timeout. `None` when there is no usable login shell, including on Windows.
    pub fn for_current_user(exe: &Path) -> Option<Self> {
        #[cfg(unix)]
        {
            let user = nix::unistd::User::from_uid(nix::unistd::getuid())
                .ok()
                .flatten();
            let shell = choose_shell(
                user.as_ref().map(|u| u.shell.as_path()),
                std::env::var_os("SHELL").as_deref(),
            )?;
            if !is_executable(&shell) {
                return None;
            }
            let home = user
                .map(|u| u.dir)
                .filter(|d| d.is_absolute())
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("/"));
            Some(Self {
                shell,
                exe: exe.to_path_buf(),
                home,
                nonce: fresh_nonce(),
                timeout: Self::TIMEOUT,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = exe;
            None
        }
    }

    /// The shell's basename, as `run_path.captured {shell}` and status show it.
    pub fn shell_name(&self) -> String {
        self.shell
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }

    /// The `-c` argument: `'<exe>' __env-probe <nonce>`.
    pub fn command(&self) -> Result<OsString, CaptureError> {
        let exe = self.exe.as_os_str();
        if exe.to_string_lossy().contains('\'') {
            return Err(CaptureError::UnsafeExePath);
        }
        let mut command = OsString::from("'");
        command.push(exe);
        command.push(format!("' {ENV_PROBE_SUBCOMMAND} {}", self.nonce));
        Ok(command)
    }

    /// Runs the shell and returns the PATH it reported. The shell gets the Worker's env plus
    /// `DISABLE_AUTO_UPDATE=true` and `CLAUDECLAW_ENV_PROBE=1`, cwd [`Self::home`], stdin null,
    /// its own process group, and is killed with that group after [`Self::timeout`]. Up to
    /// [`Self::STDOUT_CAP`] bytes of stdout are kept; stderr is only counted.
    #[cfg(unix)]
    pub async fn capture(&self) -> Result<OsString, CaptureError> {
        use tokio::io::AsyncReadExt;

        let command = self.command()?;
        let mut child = tokio::process::Command::new(&self.shell)
            .args(["-l", "-i", "-c"])
            .arg(&command)
            .current_dir(&self.home)
            .env("DISABLE_AUTO_UPDATE", "true")
            .env("CLAUDECLAW_ENV_PROBE", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(CaptureError::SpawnFailed)?;
        let pgid = child.id();
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");

        // The readers run as their own tasks, so a process the rc files leave holding the
        // pipes open can't hold the capture more than PIPE_GRACE past the shell's exit.
        let kept = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut read_stdout = tokio::spawn({
            let kept = kept.clone();
            async move {
                let mut buf = [0u8; 8192];
                while let Ok(n) = stdout.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
                    let room = Self::STDOUT_CAP.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        });
        let count_stderr = tokio::spawn(async move {
            let mut total = 0usize;
            let mut buf = [0u8; 8192];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                total += n;
            }
            total
        });

        let result = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(_status) => {
                let _ = tokio::time::timeout(PIPE_GRACE, &mut read_stdout).await;
                let kept = kept.lock().unwrap_or_else(|e| e.into_inner());
                tracing::debug!(stdout_bytes = kept.len(), "login shell finished");
                parse_probe_output(&kept, &self.nonce)
            }
            Err(_) => {
                if let Some(pid) = pgid.and_then(|p| i32::try_from(p).ok()) {
                    // The shell leads its own group; kill everything its rc files started.
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(pid),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                }
                let _ = child.wait().await;
                Err(CaptureError::Timeout(self.timeout))
            }
        };
        read_stdout.abort();
        if count_stderr.is_finished() {
            if let Ok(stderr_bytes) = count_stderr.await {
                tracing::debug!(stderr_bytes, "login shell stderr discarded");
            }
        } else {
            count_stderr.abort();
        }
        result
    }

    /// There is no login shell on Windows; the registry is the source there (#26 D4).
    #[cfg(not(unix))]
    pub async fn capture(&self) -> Result<OsString, CaptureError> {
        Err(CaptureError::SpawnFailed(std::io::Error::from(
            std::io::ErrorKind::Unsupported,
        )))
    }
}

/// How long the pipes are still read after the shell exits.
#[cfg(unix)]
const PIPE_GRACE: Duration = Duration::from_secs(1);

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
        && nix::unistd::access(path, nix::unistd::AccessFlags::X_OK).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_round_trips() {
        let out = encode_probe_output("N1", Some(OsStr::new("/a:/b c")));
        assert_eq!(out, "N1\n{\"PATH\":\"/a:/b c\"}\nN1\n");
        assert_eq!(
            parse_probe_output(out.as_bytes(), "N1").unwrap(),
            OsString::from("/a:/b c")
        );
    }

    #[test]
    fn parsing_table() {
        let ok = |s: &str| parse_probe_output(s.as_bytes(), "N").map_err(|e| e.kind());
        // rc-file noise before and after the markers is ignored.
        assert_eq!(
            ok("welcome!\nN\n{\"PATH\":\"/x\"}\nN\ngoodbye\n"),
            Ok(OsString::from("/x"))
        );
        // Extra keys are ignored; only PATH is imported.
        assert_eq!(
            ok("N{\"PATH\":\"/x\",\"OTHER\":\"y\"}N"),
            Ok(OsString::from("/x"))
        );
        assert_eq!(ok(""), Err(CaptureErrorKind::NoMarker));
        assert_eq!(
            ok("N\n{\"PATH\":\"/x\"}\n"),
            Err(CaptureErrorKind::NoMarker)
        );
        assert_eq!(ok("tmux took over"), Err(CaptureErrorKind::NoMarker));
        assert_eq!(ok("N\nnot json\nN"), Err(CaptureErrorKind::BadOutput));
        assert_eq!(
            ok("N\n{\"PATH\":null}\nN"),
            Err(CaptureErrorKind::BadOutput)
        );
        assert_eq!(ok("N\n{\"PATH\":3}\nN"), Err(CaptureErrorKind::BadOutput));
        assert_eq!(ok("N\n{}\nN"), Err(CaptureErrorKind::BadOutput));
        assert_eq!(ok("N\n[\"/x\"]\nN"), Err(CaptureErrorKind::BadOutput));
        assert_eq!(
            parse_probe_output(b"N\n\xff\nN", "N").map_err(|e| e.kind()),
            Err(CaptureErrorKind::BadOutput)
        );
        assert_eq!(
            encode_probe_output("N", None),
            "N\n{\"PATH\":null}\nN\n".to_owned()
        );
    }

    #[test]
    fn rc_hooks_that_echo_the_command_are_skipped() {
        let nonce = "claudeclaw-env-probe-1a-2b";
        let probe = encode_probe_output(nonce, Some(OsStr::new("/x:/y")));
        // bash `trap 'printf "\033]0;%s\007" "$BASH_COMMAND"' DEBUG` (or a bash-preexec title
        // hook) prints the `-c` command, nonce included, as a terminal title before running it.
        let title = format!("\x1b]0;'/opt/bin/claudeclaw' __env-probe {nonce}\x07");
        let parse = |out: String| parse_probe_output(out.as_bytes(), nonce).map_err(|e| e.kind());
        let want = Ok(OsString::from("/x:/y"));
        assert_eq!(parse(format!("{title}{probe}")), want, "echo before");
        assert_eq!(parse(format!("{probe}{title}")), want, "echo after");
        assert_eq!(
            parse(format!("{title}{probe}{title}")),
            want,
            "echo both sides"
        );
        assert_eq!(
            parse(format!("{title}rc noise\n{title}{probe}")),
            want,
            "echoed twice before"
        );
        // Echoes alone are still no probe output.
        assert_eq!(
            parse(format!("{title}{title}")),
            Err(CaptureErrorKind::BadOutput)
        );
        assert_eq!(parse(title.clone()), Err(CaptureErrorKind::NoMarker));
    }

    #[test]
    fn shell_choice() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(
            choose_shell(Some(Path::new("/bin/zsh")), Some(OsStr::new("/bin/bash"))),
            p("/bin/zsh")
        );
        assert_eq!(
            choose_shell(Some(Path::new("")), Some(OsStr::new("/bin/bash"))),
            p("/bin/bash")
        );
        assert_eq!(
            choose_shell(None, Some(OsStr::new("/bin/bash"))),
            p("/bin/bash")
        );
        assert_eq!(choose_shell(None, None), p("/bin/sh"));
        assert_eq!(choose_shell(None, Some(OsStr::new(""))), p("/bin/sh"));
        assert_eq!(
            choose_shell(Some(Path::new("/usr/sbin/nologin")), None),
            None
        );
        assert_eq!(choose_shell(Some(Path::new("/bin/false")), None), None);
    }

    #[test]
    fn the_command_quotes_the_exe() {
        let probe = LoginShellProbe {
            shell: "/bin/sh".into(),
            exe: "/opt/claude claw/bin/claudeclaw".into(),
            home: "/".into(),
            nonce: "N".into(),
            timeout: LoginShellProbe::TIMEOUT,
        };
        assert_eq!(
            probe.command().unwrap(),
            OsString::from("'/opt/claude claw/bin/claudeclaw' __env-probe N")
        );
        let unsafe_probe = LoginShellProbe {
            exe: "/opt/it's/claudeclaw".into(),
            ..probe
        };
        assert_eq!(
            unsafe_probe.command().map_err(|e| e.kind()),
            Err(CaptureErrorKind::UnsafeExePath)
        );
    }

    #[test]
    fn nonces_differ() {
        let a = fresh_nonce();
        std::thread::sleep(Duration::from_millis(1));
        assert_ne!(a, fresh_nonce());
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::PermissionsExt;

        use super::*;

        fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        /// A fake shell that checks the flags, prints rc-file noise and runs the `-c` string.
        fn fake_shell(dir: &Path, extra: &str) -> PathBuf {
            script(
                dir,
                "fakesh",
                &format!(
                    "[ \"$1 $2 $3\" = \"-l -i -c\" ] || exit 3\n\
                     echo 'rc noise'\necho 'rc warning' >&2\n{extra}\nexec /bin/sh -c \"$4\"\n"
                ),
            )
        }

        /// A fake `claudeclaw` binary that answers `__env-probe <nonce>` like the real one.
        fn fake_exe(dir: &Path) -> PathBuf {
            script(
                dir,
                "fake claudeclaw",
                "[ \"$1\" = __env-probe ] || exit 4\n\
                 [ \"$CLAUDECLAW_ENV_PROBE\" = 1 ] || exit 5\n\
                 [ \"$DISABLE_AUTO_UPDATE\" = true ] || exit 6\n\
                 printf '%s\\n{\"PATH\":\"/probe/bin:%s\"}\\n%s\\n' \"$2\" \"$(pwd)\" \"$2\"\n",
            )
        }

        fn probe(dir: &Path, shell: PathBuf, timeout: Duration) -> LoginShellProbe {
            LoginShellProbe {
                shell,
                exe: fake_exe(dir),
                home: dir.to_path_buf(),
                nonce: fresh_nonce(),
                timeout,
            }
        }

        #[tokio::test]
        async fn captures_the_path_through_a_fake_shell() {
            let dir = tempfile::tempdir().unwrap();
            let shell = fake_shell(dir.path(), "");
            let got = probe(dir.path(), shell, Duration::from_secs(10))
                .capture()
                .await
                .unwrap();
            // The probe ran with cwd = home.
            let home = std::fs::canonicalize(dir.path()).unwrap();
            let got = got.to_string_lossy().into_owned();
            let (first, cwd) = got.split_once(':').unwrap();
            assert_eq!(first, "/probe/bin");
            assert_eq!(std::fs::canonicalize(cwd).unwrap(), home);
        }

        #[tokio::test]
        async fn a_shell_that_never_runs_the_probe_has_no_marker() {
            let dir = tempfile::tempdir().unwrap();
            let shell = script(dir.path(), "tmuxsh", "echo 'exec tmux'\nexit 0\n");
            let err = probe(dir.path(), shell, Duration::from_secs(10))
                .capture()
                .await
                .unwrap_err();
            assert_eq!(err.kind(), CaptureErrorKind::NoMarker);
        }

        #[tokio::test]
        async fn a_missing_shell_is_a_spawn_failure() {
            let dir = tempfile::tempdir().unwrap();
            let err = probe(dir.path(), dir.path().join("nope"), Duration::from_secs(10))
                .capture()
                .await
                .unwrap_err();
            assert_eq!(err.kind(), CaptureErrorKind::SpawnFailed);
        }

        #[tokio::test]
        async fn the_timeout_kills_the_whole_process_group() {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("bg.pid");
            // The rc file starts a background job and publishes its pid before anything else
            // (atomically, via a rename), then hangs.
            let shell = script(
                dir.path(),
                "hangsh",
                &format!(
                    "sleep 300 &\necho $! > '{tmp}'\nmv '{tmp}' '{pid}'\nsleep 300\n",
                    tmp = dir.path().join("bg.pid.tmp").display(),
                    pid = pid_file.display()
                ),
            );
            let started = std::time::Instant::now();
            let p = probe(dir.path(), shell, Duration::from_secs(2));
            // The pid file appears while the probe is still waiting, however loaded the machine.
            let wait_for_pid = async {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while !pid_file.exists() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the hang script never started"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            let (captured, ()) = tokio::join!(p.capture(), wait_for_pid);
            let err = captured.unwrap_err();
            assert_eq!(err.kind(), CaptureErrorKind::Timeout);
            assert!(started.elapsed() < Duration::from_secs(10));

            let pid: i32 = std::fs::read_to_string(&pid_file)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let pid = nix::unistd::Pid::from_raw(pid);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            // The orphaned `sleep` is reparented and reaped once killed.
            while nix::sys::signal::kill(pid, None).is_ok() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the background job survived the timeout"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

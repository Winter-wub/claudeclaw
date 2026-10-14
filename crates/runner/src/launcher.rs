//! Launchers and their resolution (architecture §4 `claudeclaw-runner` `launcher`; #8, #25,
//! #26, #48).
//!
//! A [`Launcher`] is `direct` or the absolute path of one runnable executable; it carries no
//! name and no env of its own (#8). [`resolve`] turns it into the absolute program to spawn,
//! looking things up on the Run PATH itself, never through std's program search (#26 D3).
//! Errors never include a Launcher's arguments or contents (#8): their Display names only a
//! file's basename, and [`LauncherError::path`] gives the full path to the app and CLI.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::env::RunPath;

/// What `direct` looks up on the Run PATH (#8, #26 D3).
pub const DIRECT_PROGRAM: &str = "claude";

/// A Launcher (#8; glossary).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Launcher {
    /// `claude` on the Run PATH. It never imports a `CLAUDE_CONFIG_DIR` set outside the Daemon
    /// (#48; see [`crate::env::RunEnv::child_env`]).
    Direct,
    /// The absolute path of one runnable executable, usually the user's script.
    Path(AbsPath),
}

impl Launcher {
    /// The settings form: `direct`, or an absolute path.
    pub fn parse(value: &str) -> Result<Self, LauncherError> {
        if value == "direct" {
            Ok(Self::Direct)
        } else {
            AbsPath::new(value).map(Self::Path)
        }
    }
}

/// An absolute path (#8).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AbsPath(PathBuf);

impl AbsPath {
    /// `path`, if it is absolute on this OS.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, LauncherError> {
        let path = path.into();
        if path.is_absolute() {
            Ok(Self(path))
        } else {
            Err(LauncherError::NotAbsolute)
        }
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for AbsPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// A Launcher [`resolve`] found runnable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLauncher {
    program: PathBuf,
    interpreter: Option<PathBuf>,
    batch: bool,
}

impl ResolvedLauncher {
    /// The absolute path to spawn (#26 D3), recorded as the run's `launcher_resolved` (#23).
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The `#!` interpreter it was checked against (Unix), for status and Test launcher.
    pub fn interpreter(&self) -> Option<&Path> {
        self.interpreter.as_deref()
    }

    /// A Windows `.cmd` / `.bat`, which `cmd.exe` runs (#25 D3): the spawn's command-line
    /// limit is 8,191 UTF-16 units, and [`check_cwd`] applies.
    pub fn is_batch(&self) -> bool {
        self.batch
    }

    pub fn into_program(self) -> PathBuf {
        self.program
    }
}

/// Why a Launcher can't run: a Refused run (#8, #25, #26).
#[derive(Debug, thiserror::Error)]
pub enum LauncherError {
    #[error("a Launcher must be `direct` or an absolute path")]
    NotAbsolute,
    #[error("`claude` (Launcher `direct`) was not found on the Run PATH")]
    DirectNotFound,
    #[error("the Launcher `{}` does not exist", basename(.path))]
    Missing { path: PathBuf },
    #[error("the Launcher `{}` is not a regular file", basename(.path))]
    NotFile { path: PathBuf },
    #[error("the Launcher `{}` is not executable", basename(.path))]
    NotExecutable { path: PathBuf },
    #[error(
        "`{name}`, the interpreter of the Launcher `{}`, was not found on the Run PATH",
        basename(.path)
    )]
    InterpreterNotFound { path: PathBuf, name: String },
    #[error("the Launcher `{}` has a `#!` line with no interpreter", basename(.path))]
    BadShebang { path: PathBuf },
    #[error(
        "the Launcher `{}` is not a .exe, .com, .cmd or .bat file; \
         wrap a PowerShell script in a .cmd",
        basename(.path)
    )]
    WindowsExtension { path: PathBuf },
    #[error(
        "the .cmd or .bat Launcher `{}` has a `%` in its path, which cmd.exe would expand",
        basename(.path)
    )]
    WindowsPercent { path: PathBuf },
    #[error("a .cmd or .bat Launcher can't run in a UNC working directory; use a mapped drive")]
    UncCwd,
    #[error("the Launcher `{}` can't be checked", basename(.path))]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl LauncherError {
    /// A stable code for the refusal, for `run.refused {reason}` and the API. The first three
    /// match `claudeclaw_api::scopes::NotRunnable`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "missing",
            Self::NotFile { .. } => "not_file",
            Self::NotExecutable { .. } => "not_executable",
            Self::NotAbsolute => "not_absolute",
            Self::DirectNotFound => "direct_not_found",
            Self::InterpreterNotFound { .. } | Self::BadShebang { .. } => "interpreter_not_found",
            Self::WindowsExtension { .. } => "windows_extension",
            Self::WindowsPercent { .. } => "windows_percent",
            Self::UncCwd => "unc_cwd",
            Self::Io { .. } => "io",
        }
    }

    /// A miss the Run PATH might fix: the caller rebuilds it behind a
    /// [`crate::env::RebuildGate`] and resolves again before refusing (#26 D3 step 4).
    pub fn is_miss(&self) -> bool {
        matches!(
            self,
            Self::DirectNotFound | Self::Missing { .. } | Self::InterpreterNotFound { .. }
        )
    }

    /// The full path involved, for the app and the CLI only; chat shows the basename (#26 D3).
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Missing { path }
            | Self::NotFile { path }
            | Self::NotExecutable { path }
            | Self::InterpreterNotFound { path, .. }
            | Self::BadShebang { path }
            | Self::WindowsExtension { path }
            | Self::WindowsPercent { path }
            | Self::Io { path, .. } => Some(path),
            Self::NotAbsolute | Self::DirectNotFound | Self::UncCwd => None,
        }
    }
}

fn basename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Resolves a Launcher on the Run PATH before a spawn (#26 D3; formerly `check_runnable`).
///
/// - **Unix:** `direct` is the first executable regular `claude` on the Run PATH
///   (`which::which_in`); a path Launcher must exist, be a regular file and be executable.
///   Either way, a `#!` line's interpreter must resolve too: `#!/usr/bin/env [-S] [flags]
///   <name>` on the Run PATH, an absolute interpreter as an executable file. Only the first
///   line, within 256 bytes, is read.
/// - **Windows** (#25 D3, D4): `direct` is the first of `claude.exe`, `claude.cmd`,
///   `claude.bat` in each Run PATH dir in turn; a path Launcher must exist and be a regular
///   file. Either way it must be a `.exe`, `.com`, `.cmd` or `.bat`, with no `PATHEXT` search,
///   and a `.cmd` / `.bat` path must not contain `%`.
///
/// On a miss ([`LauncherError::is_miss`]) the caller may rebuild the Run PATH once, behind a
/// [`crate::env::RebuildGate`], and resolve again before refusing.
pub fn resolve(launcher: &Launcher, run_path: &RunPath) -> Result<ResolvedLauncher, LauncherError> {
    #[cfg(windows)]
    {
        resolve_windows(launcher, run_path)
    }
    #[cfg(not(windows))]
    {
        resolve_unix(launcher, run_path)
    }
}

/// Checks the run's cwd against the Launcher (#25 D3): `cmd.exe` replaces a UNC cwd with the
/// Windows dir, so a `.cmd` / `.bat` Launcher refuses one. Mapped drives are fine. A no-op for
/// any other Launcher, and so off Windows.
pub fn check_cwd(resolved: &ResolvedLauncher, cwd: &Path) -> Result<(), LauncherError> {
    if resolved.is_batch() && windows::is_unc(&cwd.to_string_lossy()) {
        Err(LauncherError::UncCwd)
    } else {
        Ok(())
    }
}

/// The checks every OS shares: the path exists and is a regular file.
fn check_regular_file(path: &Path) -> Result<(), LauncherError> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(LauncherError::NotFile { path: path.into() }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(LauncherError::Missing { path: path.into() })
        }
        Err(source) => Err(LauncherError::Io {
            path: path.into(),
            source,
        }),
    }
}

#[cfg(windows)]
fn resolve_windows(
    launcher: &Launcher,
    run_path: &RunPath,
) -> Result<ResolvedLauncher, LauncherError> {
    let program = match launcher {
        Launcher::Direct => windows::direct_candidates(run_path.dirs())
            .find(|p| p.is_file())
            .ok_or(LauncherError::DirectNotFound)?,
        Launcher::Path(path) => {
            check_regular_file(path.as_path())?;
            path.as_path().to_path_buf()
        }
    };
    let text = program.to_string_lossy();
    let kind = windows::launcher_kind(&text).ok_or_else(|| LauncherError::WindowsExtension {
        path: program.clone(),
    })?;
    let batch = kind == windows::Kind::Batch;
    if batch && windows::batch_path_unsafe(&text) {
        return Err(LauncherError::WindowsPercent { path: program });
    }
    Ok(ResolvedLauncher {
        program,
        interpreter: None,
        batch,
    })
}

#[cfg(not(windows))]
fn resolve_unix(
    launcher: &Launcher,
    run_path: &RunPath,
) -> Result<ResolvedLauncher, LauncherError> {
    let program = match launcher {
        Launcher::Direct => which::which_in(
            DIRECT_PROGRAM,
            Some(run_path.to_os_string()),
            Path::new("/"),
        )
        .map_err(|_| LauncherError::DirectNotFound)?,
        Launcher::Path(path) => {
            let path = path.as_path();
            check_regular_file(path)?;
            if !is_executable(path) {
                return Err(LauncherError::NotExecutable { path: path.into() });
            }
            path.to_path_buf()
        }
    };
    let interpreter = check_interpreter(&program, run_path)?;
    Ok(ResolvedLauncher {
        program,
        interpreter,
        batch: false,
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    nix::unistd::access(path, nix::unistd::AccessFlags::X_OK).is_ok()
}

#[cfg(not(any(unix, windows)))]
fn is_executable(_: &Path) -> bool {
    true
}

/// How much of a script is read for its `#!` line (#26 D3).
pub const SHEBANG_READ_LIMIT: usize = 256;

/// The interpreter a `#!` line names (#26 D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shebang {
    /// `#!/usr/bin/env [-S] [flags] <name>`: `<name>` is looked up on the Run PATH.
    Env(String),
    /// `#!<path> [arg]`.
    Interpreter(String),
    /// `#!` with nothing usable after it.
    Empty,
}

/// Reads the `#!` line from the first bytes of a file; `None` when it doesn't start with `#!`.
/// Only the first line is looked at.
pub fn parse_shebang(head: &[u8]) -> Option<Shebang> {
    let rest = head.strip_prefix(b"#!")?;
    let line = rest.split(|&b| b == b'\n').next().unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let mut tokens = line.split_ascii_whitespace();
    let Some(first) = tokens.next() else {
        return Some(Shebang::Empty);
    };
    if Path::new(first).file_name() != Some(OsStr::new("env")) {
        return Some(Shebang::Interpreter(first.to_owned()));
    }
    while let Some(token) = tokens.next() {
        match token {
            // Options that take the next token as their argument.
            "-u" | "-P" | "-C" | "--unset" | "--chdir" => {
                tokens.next();
            }
            // `-S` with the command glued on: `-Snode`.
            t if t.starts_with("-S") && t.len() > 2 => {
                return Some(Shebang::Env(t[2..].to_owned()));
            }
            t if t.starts_with('-') || t.contains('=') => {}
            name => return Some(Shebang::Env(name.to_owned())),
        }
    }
    Some(Shebang::Empty)
}

/// Checks a script's interpreter on the Run PATH (#26 D3 step 3). A file that can't be read
/// (an execute-only binary) has no `#!` line to check.
#[cfg(not(windows))]
fn check_interpreter(program: &Path, run_path: &RunPath) -> Result<Option<PathBuf>, LauncherError> {
    use std::io::Read;

    let mut head = Vec::with_capacity(SHEBANG_READ_LIMIT);
    let read = std::fs::File::open(program).and_then(|f| {
        f.take(SHEBANG_READ_LIMIT as u64)
            .read_to_end(&mut head)
            .map(|_| ())
    });
    if let Err(e) = read {
        tracing::debug!(
            errno = e.raw_os_error(),
            "can't read a Launcher's first line"
        );
        return Ok(None);
    }
    let not_found = |name: &str| LauncherError::InterpreterNotFound {
        path: program.into(),
        name: name.to_owned(),
    };
    let executable_file =
        |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.is_file()) && is_executable(p);
    match parse_shebang(&head) {
        None => Ok(None),
        Some(Shebang::Empty) => Err(LauncherError::BadShebang {
            path: program.into(),
        }),
        Some(Shebang::Interpreter(interp)) => {
            let interp = PathBuf::from(interp);
            if interp.is_absolute() && executable_file(&interp) {
                Ok(Some(interp))
            } else {
                Err(not_found(&basename(&interp)))
            }
        }
        Some(Shebang::Env(name)) if name.contains('/') => {
            let interp = PathBuf::from(&name);
            if interp.is_absolute() && executable_file(&interp) {
                Ok(Some(interp))
            } else {
                Err(not_found(&basename(&interp)))
            }
        }
        Some(Shebang::Env(name)) => {
            which::which_in(&name, Some(run_path.to_os_string()), Path::new("/"))
                .map(Some)
                .map_err(|_| not_found(&name))
        }
    }
}

/// The Windows Launcher rules (#25 D3, D4), as pure functions over path text so every OS
/// tests them.
pub mod windows {
    use std::path::{Path, PathBuf};

    /// The names `direct` tries in each Run PATH dir, in order (#25 D4).
    pub const DIRECT_NAMES: [&str; 3] = ["claude.exe", "claude.cmd", "claude.bat"];

    /// What runs a Windows Launcher.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        /// `.exe` / `.com`, started by `CreateProcessW`.
        Native,
        /// `.cmd` / `.bat`, started through `cmd.exe`.
        Batch,
    }

    /// The Launcher kind from the extension, case-insensitive; `None` for anything else,
    /// PowerShell included (#25 D3).
    pub fn launcher_kind(path: &str) -> Option<Kind> {
        let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
        let (stem, ext) = name.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        match ext.to_ascii_lowercase().as_str() {
            "exe" | "com" => Some(Kind::Native),
            "cmd" | "bat" => Some(Kind::Batch),
            _ => None,
        }
    }

    /// A `.cmd` / `.bat` path with `%` in it: Rust doesn't escape the script path, and
    /// `cmd.exe` would expand a `%…%` (#25 D3).
    pub fn batch_path_unsafe(path: &str) -> bool {
        path.contains('%')
    }

    /// A UNC path (`\\server\share`, `\\?\UNC\server\share`): no drive letter for `cmd.exe`
    /// (#25 D3). A verbatim drive path (`\\?\C:\…`) is not UNC.
    pub fn is_unc(path: &str) -> bool {
        let bytes = path.as_bytes();
        let sep = |b: u8| b == b'\\' || b == b'/';
        if bytes.len() < 2 || !sep(bytes[0]) || !sep(bytes[1]) {
            return false;
        }
        let rest = &path[2..];
        if let Some(verbatim) = rest
            .strip_prefix('?')
            .or_else(|| rest.strip_prefix('.'))
            .and_then(|r| r.strip_prefix(['\\', '/']))
        {
            return verbatim.get(..4).is_some_and(|p| {
                p.eq_ignore_ascii_case("UNC\\") || p.eq_ignore_ascii_case("UNC/")
            });
        }
        true
    }

    /// `direct`'s candidates: each dir in order, and in each `claude.exe`, `claude.cmd`,
    /// `claude.bat` (#25 D4).
    pub fn direct_candidates(dirs: &[PathBuf]) -> impl Iterator<Item = PathBuf> + '_ {
        dirs.iter()
            .flat_map(|dir| DIRECT_NAMES.iter().map(move |name| Path::join(dir, name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_parsing() {
        assert_eq!(Launcher::parse("direct").unwrap(), Launcher::Direct);
        assert_eq!(
            Launcher::parse("claude").unwrap_err().code(),
            "not_absolute"
        );
        let abs = std::env::temp_dir().join("launcher");
        assert_eq!(
            Launcher::parse(&abs.to_string_lossy()).unwrap(),
            Launcher::Path(AbsPath::new(&abs).unwrap())
        );
    }

    #[test]
    fn shebang_table() {
        let cases: &[(&[u8], Option<Shebang>)] = &[
            (b"\x7fELF", None),
            (b"echo hi\n", None),
            (
                b"#!/bin/sh\necho",
                Some(Shebang::Interpreter("/bin/sh".into())),
            ),
            (
                b"#! /bin/bash -e\n",
                Some(Shebang::Interpreter("/bin/bash".into())),
            ),
            (b"#!/usr/bin/env node\n", Some(Shebang::Env("node".into()))),
            (
                b"#!/usr/bin/env node\r\n",
                Some(Shebang::Env("node".into())),
            ),
            (
                b"#!/usr/bin/env -S node --flag\n",
                Some(Shebang::Env("node".into())),
            ),
            (
                b"#!/usr/bin/env -Snode\n",
                Some(Shebang::Env("node".into())),
            ),
            (
                b"#!/usr/bin/env -i FOO=1 bun\n",
                Some(Shebang::Env("bun".into())),
            ),
            (
                b"#!/usr/bin/env -u HOME python3\n",
                Some(Shebang::Env("python3".into())),
            ),
            (
                b"#!/bin/env /opt/x/bin/tool\n",
                Some(Shebang::Env("/opt/x/bin/tool".into())),
            ),
            (b"#!/usr/bin/env\n", Some(Shebang::Empty)),
            (b"#!\n", Some(Shebang::Empty)),
            (b"#!", Some(Shebang::Empty)),
            // Only the first line counts.
            (
                b"#!/bin/sh\n#!/usr/bin/env node\n",
                Some(Shebang::Interpreter("/bin/sh".into())),
            ),
        ];
        for (head, want) in cases {
            assert_eq!(
                &parse_shebang(head),
                want,
                "{}",
                String::from_utf8_lossy(head)
            );
        }
    }

    #[test]
    fn windows_extension_rules() {
        use windows::{Kind, launcher_kind};
        assert_eq!(launcher_kind(r"C:\tools\claude.exe"), Some(Kind::Native));
        assert_eq!(launcher_kind(r"C:\tools\claude.EXE"), Some(Kind::Native));
        assert_eq!(launcher_kind(r"C:\tools\x.com"), Some(Kind::Native));
        assert_eq!(launcher_kind(r"C:\tools\ccs-work.cmd"), Some(Kind::Batch));
        assert_eq!(launcher_kind(r"C:\tools\run.Bat"), Some(Kind::Batch));
        assert_eq!(launcher_kind("C:/tools/run.bat"), Some(Kind::Batch));
        for bad in [
            r"C:\tools\wrap.ps1",
            r"C:\tools\x.js",
            r"C:\tools\x.vbs",
            r"C:\tools\claude",
            r"C:\tools.exe\claude",
            r"C:\tools\.exe",
        ] {
            assert_eq!(launcher_kind(bad), None, "{bad}");
        }
    }

    #[test]
    fn windows_percent_rule() {
        assert!(windows::batch_path_unsafe(r"C:\%USERPROFILE%\x.cmd"));
        assert!(windows::batch_path_unsafe(r"C:\100%\x.cmd"));
        assert!(!windows::batch_path_unsafe(r"C:\tools\x.cmd"));
    }

    #[test]
    fn windows_unc_rule() {
        for unc in [
            r"\\server\share\project",
            "//server/share",
            r"\\?\UNC\server\share",
            r"\\?\unc\server\share",
        ] {
            assert!(windows::is_unc(unc), "{unc}");
        }
        for local in [r"C:\project", r"\\?\C:\project", "Z:\\", r"\project", "C:"] {
            assert!(!windows::is_unc(local), "{local}");
        }
    }

    #[test]
    fn windows_direct_search_order_is_dir_by_dir() {
        let dirs = vec![PathBuf::from("A"), PathBuf::from("B")];
        let got: Vec<PathBuf> = windows::direct_candidates(&dirs).collect();
        let want: Vec<PathBuf> = [
            ("A", "claude.exe"),
            ("A", "claude.cmd"),
            ("A", "claude.bat"),
            ("B", "claude.exe"),
            ("B", "claude.cmd"),
            ("B", "claude.bat"),
        ]
        .iter()
        .map(|(d, n)| Path::new(d).join(n))
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn errors_name_only_the_basename() {
        let path = std::env::temp_dir().join("private dir").join("ccs-work");
        let err = LauncherError::InterpreterNotFound {
            path: path.clone(),
            name: "node".into(),
        };
        let text = err.to_string();
        assert!(
            text.contains("`ccs-work`") && text.contains("`node`"),
            "{text}"
        );
        assert!(!text.contains("private dir"), "{text}");
        assert_eq!(err.path(), Some(path.as_path()));
        assert!(err.is_miss());
        assert!(!LauncherError::NotExecutable { path }.is_miss());
    }

    #[test]
    fn check_cwd_only_binds_batch_launchers() {
        let unc = Path::new(r"\\server\share\project");
        let batch = ResolvedLauncher {
            program: PathBuf::from(r"C:\l.cmd"),
            interpreter: None,
            batch: true,
        };
        let native = ResolvedLauncher {
            batch: false,
            ..batch.clone()
        };
        assert_eq!(check_cwd(&batch, unc).unwrap_err().code(), "unc_cwd");
        assert!(check_cwd(&batch, Path::new(r"C:\project")).is_ok());
        assert!(check_cwd(&native, unc).is_ok());
    }

    #[cfg(windows)]
    mod on_windows {
        use super::*;

        fn touch(dir: &Path, name: &str) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, "").unwrap();
            path
        }

        fn run_path(dirs: &[&Path]) -> RunPath {
            let dirs: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
            RunPath::compose(&dirs, None, None).path
        }

        #[test]
        fn direct_searches_dir_by_dir() {
            let a = tempfile::tempdir().unwrap();
            let b = tempfile::tempdir().unwrap();
            let cmd = touch(a.path(), "claude.cmd");
            touch(b.path(), "claude.exe");
            let got = resolve(&Launcher::Direct, &run_path(&[a.path(), b.path()])).unwrap();
            assert_eq!(got.program(), cmd);
            assert!(got.is_batch());
            let exe = touch(a.path(), "claude.exe");
            let got = resolve(&Launcher::Direct, &run_path(&[a.path(), b.path()])).unwrap();
            assert_eq!(got.program(), exe);
            assert!(!got.is_batch());
            // No PATHEXT search: a `claude.ps1` is never found.
            let c = tempfile::tempdir().unwrap();
            touch(c.path(), "claude.ps1");
            let err = resolve(&Launcher::Direct, &run_path(&[c.path()])).unwrap_err();
            assert_eq!(err.code(), "direct_not_found");
        }

        #[test]
        fn path_launcher_rules() {
            let dir = tempfile::tempdir().unwrap();
            let rp = RunPath::default();
            let resolve_code = |p: &Path| {
                resolve(&Launcher::Path(AbsPath::new(p).unwrap()), &rp)
                    .map(|r| r.is_batch())
                    .map_err(|e| e.code())
            };
            assert_eq!(resolve_code(&touch(dir.path(), "ccs.cmd")), Ok(true));
            assert_eq!(resolve_code(&touch(dir.path(), "tool.exe")), Ok(false));
            assert_eq!(
                resolve_code(&touch(dir.path(), "wrap.ps1")),
                Err("windows_extension")
            );
            assert_eq!(resolve_code(&dir.path().join("gone.cmd")), Err("missing"));
            let percent = dir.path().join("100%");
            std::fs::create_dir(&percent).unwrap();
            assert_eq!(
                resolve_code(&touch(&percent, "ccs.cmd")),
                Err("windows_percent")
            );
            assert_eq!(resolve_code(&touch(&percent, "tool.exe")), Ok(false));
        }
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::PermissionsExt;

        use super::*;

        fn file(dir: &Path, name: &str, body: &str, mode: u32) -> PathBuf {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        }

        fn run_path(dirs: &[&Path]) -> RunPath {
            let dirs: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
            RunPath::compose(&dirs, None, None).path
        }

        fn path_launcher(path: &Path) -> Launcher {
            Launcher::Path(AbsPath::new(path).unwrap())
        }

        #[test]
        fn direct_resolves_the_first_executable_claude() {
            let a = tempfile::tempdir().unwrap();
            let b = tempfile::tempdir().unwrap();
            file(a.path(), "claude", "not executable", 0o644);
            let want = file(b.path(), "claude", "\x7fELF", 0o755);
            let got = resolve(&Launcher::Direct, &run_path(&[a.path(), b.path()])).unwrap();
            assert_eq!(got.program(), want);
            assert!(got.program().is_absolute());
            assert_eq!(got.interpreter(), None);
            assert!(!got.is_batch());
        }

        #[test]
        fn direct_misses() {
            let a = tempfile::tempdir().unwrap();
            let err = resolve(&Launcher::Direct, &run_path(&[a.path()])).unwrap_err();
            assert_eq!(err.code(), "direct_not_found");
            assert!(err.is_miss());
            let err = resolve(&Launcher::Direct, &RunPath::default()).unwrap_err();
            assert_eq!(err.code(), "direct_not_found");
        }

        #[test]
        fn path_launcher_checks() {
            let dir = tempfile::tempdir().unwrap();
            let rp = run_path(&[Path::new("/bin"), Path::new("/usr/bin")]);
            let ok = file(dir.path(), "ok", "#!/bin/sh\nexec claude \"$@\"\n", 0o755);
            let got = resolve(&path_launcher(&ok), &rp).unwrap();
            assert_eq!(got.program(), ok);
            assert_eq!(got.interpreter(), Some(Path::new("/bin/sh")));

            let code = |p: &Path| resolve(&path_launcher(p), &rp).unwrap_err().code();
            assert_eq!(code(&dir.path().join("missing")), "missing");
            assert_eq!(code(dir.path()), "not_file");
            let plain = file(dir.path(), "plain", "#!/bin/sh\n", 0o644);
            assert_eq!(code(&plain), "not_executable");
        }

        #[test]
        fn interpreters_resolve_on_the_run_path() {
            let tools = tempfile::tempdir().unwrap();
            let scripts = tempfile::tempdir().unwrap();
            let node = file(tools.path(), "node", "\x7fELF", 0o755);
            let script = file(
                scripts.path(),
                "ccs-work",
                "#!/usr/bin/env node\nsecret-looking contents\n",
                0o755,
            );
            let launcher = path_launcher(&script);

            let got = resolve(&launcher, &run_path(&[tools.path()])).unwrap();
            assert_eq!(got.interpreter(), Some(node.as_path()));

            let err = resolve(&launcher, &run_path(&[scripts.path()])).unwrap_err();
            assert_eq!(err.code(), "interpreter_not_found");
            assert!(err.is_miss());
            let text = err.to_string();
            assert!(text.contains("`node`"), "{text}");
            assert!(!text.contains("secret"), "{text}");
        }

        #[test]
        fn direct_checks_its_interpreter_too() {
            // An npm-installed `claude` is a `#!/usr/bin/env node` script (#26 D3).
            let bin = tempfile::tempdir().unwrap();
            file(bin.path(), "claude", "#!/usr/bin/env node\n", 0o755);
            let err = resolve(&Launcher::Direct, &run_path(&[bin.path()])).unwrap_err();
            assert_eq!(err.code(), "interpreter_not_found");
            file(bin.path(), "node", "\x7fELF", 0o755);
            assert!(resolve(&Launcher::Direct, &run_path(&[bin.path()])).is_ok());
        }

        #[test]
        fn absolute_interpreters_must_exist() {
            let dir = tempfile::tempdir().unwrap();
            let script = file(dir.path(), "s", "#!/nonexistent/bin/tool -x\n", 0o755);
            let err = resolve(&path_launcher(&script), &RunPath::default()).unwrap_err();
            assert_eq!(err.code(), "interpreter_not_found");
            assert!(err.to_string().contains("`tool`"));
            let empty = file(dir.path(), "e", "#!\n", 0o755);
            let err = resolve(&path_launcher(&empty), &RunPath::default()).unwrap_err();
            assert_eq!(err.code(), "interpreter_not_found");
        }

        #[test]
        fn an_execute_only_binary_skips_the_shebang_check() {
            if nix::unistd::geteuid().is_root() {
                return; // root reads any file.
            }
            let dir = tempfile::tempdir().unwrap();
            let bin = file(dir.path(), "xonly", "#!/nonexistent\n", 0o111);
            let got = resolve(&path_launcher(&bin), &RunPath::default()).unwrap();
            assert_eq!(got.interpreter(), None);
        }
    }
}

//! The typed `claude` argv (architecture §4 runner `argv`; #8, #25).
//!
//! [`ClaudeArgs`] builds the arguments after the executable. The executable is **not** part of
//! it: spawn prepends the resolved Launcher once, so a retry can't double-wrap it (TS trap 7).
//! There is no field for prompt or system-prompt text: the prompt goes to stdin and the appended
//! system prompt to a per-run file passed as `--append-system-prompt-file` (#25 D1, D2), so argv
//! holds only short single-line tokens and `.cmd`/`.bat` Launchers are safe.
//!
//! Guards built in (#8, #25):
//! - always `--print`, never `-p`; no element may be `-p`, `--prompt` or `--prompt=…`, which
//!   trip ccs's delegation gate (#5);
//! - stream-json output always carries `--verbose`, and is the default;
//! - `argv_unsafe`: no element contains CR, LF or NUL;
//! - `argv_too_long`: on Windows the final command line fits the `cmd.exe` or `CreateProcessW`
//!   limit. It needs the executable, so spawn calls [`check_command_line`] (the seam).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

/// `--output-format` (#25 D1). The main path is always [`OutputFormat::StreamJson`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// `--output-format stream-json --verbose`: what [`crate::stream`] parses and `judge()`s.
    #[default]
    StreamJson,
    /// `--output-format text`, which has no `result` event to judge. Only for callers that
    /// don't judge the stream.
    Text,
}

/// Claude Code's permission mode (TS `src/runner.ts:840`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PermissionMode {
    Plan,
    AcceptEdits,
    /// Sent as `--dangerously-skip-permissions`. The TS default (`src/runner.ts:853`).
    #[default]
    BypassPermissions,
}

/// The security level from settings (TS `SecurityLevel`, `src/config.ts:148`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecurityLevel {
    /// `--tools Read,Grep,Glob`.
    Locked,
    /// `--disallowedTools Bash,WebSearch,WebFetch`.
    Strict,
    /// All tools; scoping is done in the system prompt. The TS default (`src/config.ts:84`).
    #[default]
    Moderate,
    /// All tools, no directory scoping.
    Unrestricted,
}

/// The security args' inputs (TS `SecurityConfig`, `src/config.ts:154`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Security {
    pub level: SecurityLevel,
    /// Joined with a space into one `--allowedTools` value, as TS does.
    pub allowed_tools: Vec<String>,
    /// Joined with a space into one `--disallowedTools` value, as TS does.
    pub disallowed_tools: Vec<String>,
}

/// Why argv was refused (#25 D1). Messages never echo an argument's value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgvError {
    /// An element contains CR, LF or NUL, or is `-p` / `--prompt` / `--prompt=…`.
    #[error("argv element {index} is unsafe")]
    Unsafe { index: usize },
    /// `--append-system-prompt-file` must be an absolute path (#25 D2).
    #[error("the system-prompt file path is not absolute")]
    RelativeSystemPromptFile,
    /// The Windows command line would exceed `limit` UTF-16 units (#25 D1).
    #[error("command line of about {len} UTF-16 units exceeds the limit of {limit}")]
    TooLong { len: usize, limit: usize },
}

impl ArgvError {
    /// The failed-outcome code (#25 D1): `argv_unsafe` or `argv_too_long`.
    pub fn code(&self) -> &'static str {
        match self {
            ArgvError::Unsafe { .. } | ArgvError::RelativeSystemPromptFile => "argv_unsafe",
            ArgvError::TooLong { .. } => "argv_too_long",
        }
    }
}

/// The typed `claude` argv, without the executable (architecture §4 runner `argv`).
///
/// [`ClaudeArgs::new`] starts from the TS defaults: stream-json, `bypassPermissions`, security
/// level `moderate`, no tool lists.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ClaudeArgs {
    output: OutputFormat,
    resume: Option<String>,
    append_system_prompt_file: Option<PathBuf>,
    model: Option<String>,
    permission_mode: PermissionMode,
    security: Security,
}

impl fmt::Debug for ClaudeArgs {
    // Content-free (architecture §2): which parts are set, never their values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeArgs")
            .field("output", &self.output)
            .field("resume", &self.resume.is_some())
            .field(
                "append_system_prompt_file",
                &self.append_system_prompt_file.is_some(),
            )
            .field("model", &self.model.is_some())
            .field("permission_mode", &self.permission_mode)
            .field("security_level", &self.security.level)
            .field("allowed_tools", &self.security.allowed_tools.len())
            .field("disallowed_tools", &self.security.disallowed_tools.len())
            .finish()
    }
}

impl ClaudeArgs {
    /// The main-path argv with TS defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// `--output-format`. Leave it at the default on the main path.
    pub fn output_format(mut self, output: OutputFormat) -> Self {
        self.output = output;
        self
    }

    /// `--resume <session_id>`.
    pub fn resume(mut self, session_id: impl Into<String>) -> Self {
        self.resume = Some(session_id.into());
        self
    }

    /// `--append-system-prompt-file <abs path>` (#25 D2). The path must be absolute.
    pub fn append_system_prompt_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.append_system_prompt_file = Some(path.into());
        self
    }

    /// `--model <model>`. Trimmed; empty or `glm` (any case) sends no `--model`, as TS does
    /// (`src/runner.ts:1150`).
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// The permission mode (TS `buildSecurityArgs`, `src/runner.ts:866`).
    pub fn permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    /// The security level and tool lists (TS `buildSecurityArgs`, `src/runner.ts:866`).
    pub fn security(mut self, security: Security) -> Self {
        self.security = security;
        self
    }

    /// The `--model` value actually sent, after TS's normalisation.
    fn model_arg(&self) -> Option<&str> {
        let model = self.model.as_deref()?.trim();
        (!model.is_empty() && !model.eq_ignore_ascii_case("glm")).then_some(model)
    }

    /// Whether this argv emits `flag` (e.g. `--append-system-prompt-file`). `judge()` uses it to
    /// tell a flag the Daemon passed from one a Launcher script added (#42 D3).
    pub fn emits_flag(&self, flag: &str) -> bool {
        flag.starts_with("--") && self.tokens().iter().any(|t| t == flag)
    }

    /// The tokens, before the guards.
    fn tokens(&self) -> Vec<OsString> {
        let mut out: Vec<OsString> = vec!["--print".into(), "--output-format".into()];
        match self.output {
            OutputFormat::StreamJson => {
                out.push("stream-json".into());
                out.push("--verbose".into());
            }
            OutputFormat::Text => out.push("text".into()),
        }

        // Security args, in TS's order (`src/runner.ts:866-895`).
        match self.permission_mode {
            PermissionMode::BypassPermissions => {
                out.push("--dangerously-skip-permissions".into());
            }
            PermissionMode::Plan => {
                out.extend(["--permission-mode".into(), "plan".into()]);
            }
            PermissionMode::AcceptEdits => {
                out.extend(["--permission-mode".into(), "acceptEdits".into()]);
            }
        }
        match self.security.level {
            SecurityLevel::Locked => out.extend(["--tools".into(), "Read,Grep,Glob".into()]),
            SecurityLevel::Strict => {
                out.extend(["--disallowedTools".into(), "Bash,WebSearch,WebFetch".into()])
            }
            SecurityLevel::Moderate | SecurityLevel::Unrestricted => {}
        }
        if !self.security.allowed_tools.is_empty() {
            out.push("--allowedTools".into());
            out.push(self.security.allowed_tools.join(" ").into());
        }
        if !self.security.disallowed_tools.is_empty() {
            out.push("--disallowedTools".into());
            out.push(self.security.disallowed_tools.join(" ").into());
        }

        // Then resume, system prompt and model, in TS's order (`src/runner.ts:1114-1150`).
        if let Some(id) = &self.resume {
            out.extend(["--resume".into(), id.into()]);
        }
        if let Some(path) = &self.append_system_prompt_file {
            out.push("--append-system-prompt-file".into());
            out.push(path.as_os_str().to_owned());
        }
        if let Some(model) = self.model_arg() {
            out.extend(["--model".into(), model.into()]);
        }
        out
    }

    /// The argv to pass after the executable, with the `argv_unsafe` guard applied (#25 D1).
    pub fn to_argv(&self) -> Result<Vec<OsString>, ArgvError> {
        if let Some(path) = &self.append_system_prompt_file
            && !path.is_absolute()
        {
            return Err(ArgvError::RelativeSystemPromptFile);
        }
        let argv = self.tokens();
        check_argv(&argv)?;
        Ok(argv)
    }
}

/// The `argv_unsafe` guard (#25 D1, #5): refuses CR, LF or NUL in any element, and any element
/// that is `-p`, `--prompt` or starts with `--prompt=`. Never truncates or re-quotes.
pub fn check_argv(args: &[OsString]) -> Result<(), ArgvError> {
    for (index, arg) in args.iter().enumerate() {
        let lossy = arg.to_string_lossy();
        let bad_char = lossy.contains(['\r', '\n', '\0']);
        let prompt_flag = lossy == "-p" || lossy == "--prompt" || lossy.starts_with("--prompt=");
        if bad_char || prompt_flag {
            return Err(ArgvError::Unsafe { index });
        }
    }
    Ok(())
}

/// `cmd.exe`'s command-line limit, for `.cmd`/`.bat` Launchers (#25 D1).
pub const WINDOWS_BATCH_LIMIT: usize = 8_191;
/// `CreateProcessW`'s command-line limit, for `.exe`/`.com` Launchers (#25 D1).
pub const WINDOWS_PROCESS_LIMIT: usize = 32_766;

/// The guard spawn runs on the final command, executable included (#25 D1): [`check_argv`] on
/// every OS, and on Windows the `argv_too_long` limit.
pub fn check_command_line(program: &Path, args: &[OsString]) -> Result<(), ArgvError> {
    check_argv(args)?;
    if cfg!(windows) {
        check_windows_len(program, args)?;
    }
    Ok(())
}

fn is_batch(program: &Path) -> bool {
    program
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

fn utf16_len(s: &OsStr) -> usize {
    s.to_string_lossy().encode_utf16().count()
}

fn count(s: &OsStr, chars: &[char]) -> usize {
    s.to_string_lossy()
        .chars()
        .filter(|c| chars.contains(c))
        .count()
}

/// An upper bound on the command line Rust std builds on Windows, in UTF-16 units: every
/// element quoted, every escape counted. For `.cmd`/`.bat` that is
/// `cmd.exe /e:ON /v:OFF /d /c "<"script"> <args>"` (std `make_bat_command_line`), each argument
/// escaped as std's `append_bat_arg` does: `"` doubled, `\` doubled before a `"` and at the end
/// of a quoted argument, `%` (and CR) expanded to `%%cd:~,%` (#25 E1). Otherwise it is
/// `"<program>" <args>` (std `make_command_line`), with `\` and `"` escaped as `append_arg`
/// does. Each `\` and `"` is counted as one extra unit, the most either escaping adds.
fn windows_command_line_len(program: &Path, args: &[OsString]) -> usize {
    let program = program.as_os_str();
    if is_batch(Path::new(program)) {
        const PREFIX: &str = "cmd.exe /e:ON /v:OFF /d /c \"";
        let mut len = PREFIX.len() + utf16_len(program) + 2 + 1;
        for arg in args {
            len += 1 + utf16_len(arg) + 2 + count(arg, &['"', '\\']) + 7 * count(arg, &['%', '\r']);
        }
        len
    } else {
        let mut len = utf16_len(program) + 2 + count(program, &['"', '\\']);
        for arg in args {
            len += 1 + utf16_len(arg) + 2 + count(arg, &['"', '\\']);
        }
        len
    }
}

fn check_windows_len(program: &Path, args: &[OsString]) -> Result<(), ArgvError> {
    let limit = if is_batch(program) {
        WINDOWS_BATCH_LIMIT
    } else {
        WINDOWS_PROCESS_LIMIT
    };
    let len = windows_command_line_len(program, args);
    if len > limit {
        return Err(ArgvError::TooLong { len, limit });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    fn strings(args: &ClaudeArgs) -> Vec<String> {
        args.to_argv()
            .unwrap()
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    #[test]
    fn main_path_is_print_stream_json_verbose() {
        assert_eq!(
            strings(&ClaudeArgs::new()),
            [
                "--print",
                "--output-format",
                "stream-json",
                "--verbose",
                "--dangerously-skip-permissions"
            ]
        );
    }

    #[test]
    fn full_argv_follows_ts_order() {
        // TS `src/runner.ts:1112-1150`: format, security args, --resume, system prompt, --model.
        let asp = abs("asp-1234.md");
        let args = ClaudeArgs::new()
            .resume("00000000-0000-4000-8000-000000000001")
            .append_system_prompt_file(&asp)
            .model(" opus ")
            .permission_mode(PermissionMode::Plan)
            .security(Security {
                level: SecurityLevel::Strict,
                allowed_tools: vec!["Read".into(), "Bash(git status)".into()],
                disallowed_tools: vec!["WebFetch".into()],
            });
        let asp = asp.to_str().unwrap();
        assert_eq!(
            strings(&args),
            [
                "--print",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "plan",
                "--disallowedTools",
                "Bash,WebSearch,WebFetch",
                "--allowedTools",
                "Read Bash(git status)",
                "--disallowedTools",
                "WebFetch",
                "--resume",
                "00000000-0000-4000-8000-000000000001",
                "--append-system-prompt-file",
                asp,
                "--model",
                "opus",
            ]
        );
    }

    #[test]
    fn security_args_match_ts() {
        // TS `buildSecurityArgs`, `src/runner.ts:866-895`.
        let cases: &[(PermissionMode, SecurityLevel, &[&str])] = &[
            (
                PermissionMode::BypassPermissions,
                SecurityLevel::Locked,
                &[
                    "--dangerously-skip-permissions",
                    "--tools",
                    "Read,Grep,Glob",
                ],
            ),
            (
                PermissionMode::AcceptEdits,
                SecurityLevel::Strict,
                &[
                    "--permission-mode",
                    "acceptEdits",
                    "--disallowedTools",
                    "Bash,WebSearch,WebFetch",
                ],
            ),
            (
                PermissionMode::Plan,
                SecurityLevel::Moderate,
                &["--permission-mode", "plan"],
            ),
            (
                PermissionMode::BypassPermissions,
                SecurityLevel::Unrestricted,
                &["--dangerously-skip-permissions"],
            ),
        ];
        for (mode, level, want) in cases {
            let args = ClaudeArgs::new().permission_mode(*mode).security(Security {
                level: *level,
                ..Security::default()
            });
            assert_eq!(&strings(&args)[4..], *want, "{mode:?} {level:?}");
        }
    }

    #[test]
    fn model_is_normalised_like_ts() {
        // TS `src/runner.ts:1150`: trimmed; empty and `glm` send no --model.
        for (model, want) in [
            ("sonnet", Some("sonnet")),
            ("  sonnet\t", Some("sonnet")),
            ("", None),
            ("   ", None),
            ("glm", None),
            ("GLM", None),
            ("glm-4.6", Some("glm-4.6")),
        ] {
            let argv = strings(&ClaudeArgs::new().model(model));
            let got = argv
                .iter()
                .position(|a| a == "--model")
                .map(|i| argv[i + 1].as_str());
            assert_eq!(got, want, "model {model:?}");
        }
    }

    #[test]
    fn text_output_has_no_verbose() {
        let argv = strings(&ClaudeArgs::new().output_format(OutputFormat::Text));
        assert_eq!(argv[..3], ["--print", "--output-format", "text"]);
        assert!(!argv.iter().any(|a| a == "--verbose"));
    }

    #[test]
    fn never_emits_dash_p() {
        let argv = strings(&ClaudeArgs::new().resume("1234").model("sonnet"));
        assert!(argv.iter().any(|a| a == "--print"));
        assert!(!argv.iter().any(|a| a == "-p" || a.starts_with("--prompt")));
    }

    #[test]
    fn unsafe_values_are_refused() {
        let cases: &[(ClaudeArgs, usize)] = &[
            (ClaudeArgs::new().model("son\nnet"), 6),
            (ClaudeArgs::new().model("a\rb"), 6),
            (ClaudeArgs::new().model("a\0b"), 6),
            (ClaudeArgs::new().model("-p"), 6),
            (ClaudeArgs::new().resume("--prompt"), 6),
            (ClaudeArgs::new().resume("--prompt=x"), 6),
            (
                ClaudeArgs::new().security(Security {
                    allowed_tools: vec!["Read\nBash".into()],
                    ..Security::default()
                }),
                6,
            ),
        ];
        for (args, index) in cases {
            let err = args.to_argv().unwrap_err();
            assert_eq!(err, ArgvError::Unsafe { index: *index }, "{args:?}");
            assert_eq!(err.code(), "argv_unsafe");
            // The message never carries the value.
            assert!(!err.to_string().contains("net"));
        }
    }

    #[test]
    fn relative_system_prompt_file_is_refused() {
        let err = ClaudeArgs::new()
            .append_system_prompt_file("tmp/asp.md")
            .to_argv()
            .unwrap_err();
        assert_eq!(err, ArgvError::RelativeSystemPromptFile);
        assert_eq!(err.code(), "argv_unsafe");
    }

    #[test]
    fn emits_flag_reports_daemon_flags() {
        let args = ClaudeArgs::new()
            .append_system_prompt_file(abs("asp.md"))
            .model("glm");
        assert!(args.emits_flag("--append-system-prompt-file"));
        assert!(args.emits_flag("--verbose"));
        assert!(!args.emits_flag("--model"));
        assert!(!args.emits_flag("--settings"));
        assert!(!args.emits_flag("stream-json"));
    }

    #[test]
    fn debug_is_content_free() {
        let args = ClaudeArgs::new().model("secret-model").resume("abcd");
        let dbg = format!("{args:?}");
        assert!(
            !dbg.contains("secret-model") && !dbg.contains("abcd"),
            "{dbg}"
        );
    }

    #[test]
    fn check_command_line_runs_argv_unsafe_everywhere() {
        let err = check_command_line(&abs("claude"), &["a\nb".into()]).unwrap_err();
        assert_eq!(err.code(), "argv_unsafe");
    }

    #[test]
    fn windows_length_estimate() {
        let exe = Path::new(r"C:\bin\claude.exe");
        let cmd = Path::new(r"C:\bin\claude.CMD");
        let short: Vec<OsString> = vec!["--print".into(), "a b".into()];
        // "C:\bin\claude.exe" quoted with 2 backslashes escaped, then two quoted args.
        assert_eq!(
            windows_command_line_len(exe, &short),
            17 + 2 + 2 + (1 + 7 + 2) + (1 + 3 + 2)
        );
        // The batch form adds the cmd.exe prefix and expands % to %%cd:~,%.
        let pct: Vec<OsString> = vec!["%PATH%".into()];
        assert_eq!(
            windows_command_line_len(cmd, &pct),
            28 + 17 + 2 + 1 + (1 + 6 + 2 + 14)
        );
        assert!(is_batch(cmd) && is_batch(Path::new("x.bat")) && !is_batch(exe));
    }

    /// Rust std's `append_bat_arg` (library/std/src/sys/args/windows.rs), for ASCII text:
    /// the exact length it appends for one argument.
    fn std_bat_arg_len(arg: &str) -> usize {
        const UNQUOTED: &str = r"#$*+-./:?@\_";
        let quote = arg.is_empty()
            || arg.ends_with('\\')
            || arg.chars().any(|c| {
                c.is_ascii() && !(c.is_ascii_alphanumeric() || UNQUOTED.contains(c))
                    || c.is_control()
            });
        let mut out = String::new();
        if quote {
            out.push('"');
        }
        let mut backslashes = 0;
        for c in arg.chars() {
            if c == '\\' {
                backslashes += 1;
            } else {
                if c == '"' {
                    out.extend(std::iter::repeat_n('\\', backslashes));
                    out.push('"');
                } else if c == '%' || c == '\r' {
                    out.push_str("%%cd:~,");
                }
                backslashes = 0;
            }
            out.push(c);
        }
        if quote {
            out.extend(std::iter::repeat_n('\\', backslashes));
            out.push('"');
        }
        out.len()
    }

    /// Rust std's `append_arg` with `Quote::Auto`, for ASCII text.
    fn std_arg_len(arg: &str) -> usize {
        let quote = arg.is_empty() || arg.contains([' ', '\t']);
        let mut out = String::new();
        if quote {
            out.push('"');
        }
        let mut backslashes = 0;
        for c in arg.chars() {
            if c == '\\' {
                backslashes += 1;
            } else {
                if c == '"' {
                    out.extend(std::iter::repeat_n('\\', backslashes + 1));
                }
                backslashes = 0;
            }
            out.push(c);
        }
        if quote {
            out.extend(std::iter::repeat_n('\\', backslashes));
            out.push('"');
        }
        out.len()
    }

    #[test]
    fn windows_estimate_bounds_std_escaping() {
        let args = [
            r"C:\dir\",
            r"C:\dir with space\\\",
            r"a\\\\b",
            r#"a\\"b"#,
            r#"\\\""#,
            r#""quoted\""#,
            r"%PATH%\",
            r"\\server\share\",
            "",
            "plain",
            "a b\t",
        ];
        for prog in [r"C:\bin\claude.cmd", r"C:\bin\claude.exe"] {
            let program = Path::new(prog);
            for arg in args {
                let os: Vec<OsString> = vec![arg.into()];
                let exact = if is_batch(program) {
                    "cmd.exe /e:ON /v:OFF /d /c \"".len()
                        + prog.len()
                        + 2
                        + 1
                        + std_bat_arg_len(arg)
                        + 1
                } else {
                    prog.len() + 2 + 1 + std_arg_len(arg)
                };
                let estimate = windows_command_line_len(program, &os);
                assert!(estimate >= exact, "{prog} {arg:?}: {estimate} < {exact}");
            }
        }
    }

    #[test]
    fn backslashes_push_a_batch_launcher_over_the_limit() {
        // 4,100 backslashes and a quote: over 8,200 units once std doubles them.
        let bat = Path::new(r"C:\bin\claude.cmd");
        let arg = format!("{}\"", "\\".repeat(4_100));
        let err = check_windows_len(bat, &[arg.into()]).unwrap_err();
        assert!(matches!(
            err,
            ArgvError::TooLong {
                limit: WINDOWS_BATCH_LIMIT,
                ..
            }
        ));
    }

    #[test]
    fn windows_limits_depend_on_the_launcher_kind() {
        let exe = Path::new(r"C:\bin\claude.exe");
        let bat = Path::new(r"C:\bin\claude.bat");
        let tools =
            |n: usize| -> Vec<OsString> { vec!["--allowedTools".into(), "R".repeat(n).into()] };

        // Fits under cmd.exe's limit: fine for both.
        assert_eq!(check_windows_len(bat, &tools(8_000)), Ok(()));
        // Over 8,191 for a batch Launcher, still fine for an .exe.
        let err = check_windows_len(bat, &tools(8_200)).unwrap_err();
        assert!(matches!(
            err,
            ArgvError::TooLong {
                limit: WINDOWS_BATCH_LIMIT,
                ..
            }
        ));
        assert_eq!(err.code(), "argv_too_long");
        assert_eq!(check_windows_len(exe, &tools(8_200)), Ok(()));
        // Over 32,766 for anything.
        let err = check_windows_len(exe, &tools(33_000)).unwrap_err();
        assert!(matches!(
            err,
            ArgvError::TooLong {
                limit: WINDOWS_PROCESS_LIMIT,
                ..
            }
        ));
    }

    #[test]
    fn typical_main_argv_is_far_below_the_batch_limit() {
        let argv = ClaudeArgs::new()
            .resume("00000000-0000-4000-8000-000000000001")
            .append_system_prompt_file(abs("asp-1234.md"))
            .model("sonnet")
            .to_argv()
            .unwrap();
        assert!(windows_command_line_len(Path::new(r"C:\x\claude.cmd"), &argv) < 1_000);
    }
}

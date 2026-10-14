//! The run environment (architecture §4 `claudeclaw-runner` `env`; #26, #43, #47, #48).
//!
//! The Worker builds one [`RunEnv`] (the Run PATH and the Run locale) with
//! [`build_run_env`], and every spawn applies [`RunEnv::child_env`]: a list of env changes the
//! spawn makes on top of the Worker's own env. The Worker's env is only ever read, never
//! changed (#26 D1).
//!
//! - The Run PATH ([`RunPath`]): `prepend ++ source ++ inherited-not-present`, from the login
//!   shell ([`LoginShellProbe`]), the registry or the inherited PATH, and the [`RebuildGate`]
//!   for rebuilds after a resolution miss (#26).
//! - The Run locale ([`RunLocaleReport`]): a UTF-8 `LC_CTYPE` when the inherited env has none
//!   (#43).
//! - The `__env-probe` output contract: see [`encode_probe_output`] and [`parse_probe_output`].

use std::ffi::{OsStr, OsString};
use std::path::Path;

use crate::launcher::Launcher;

mod locale;
mod path;
mod probe;
mod registry;

pub use locale::{
    CANDIDATES as LOCALE_CANDIDATES, CharmapCheck, CharmapError, InheritedLocale, LocaleCommand,
    NoLocaleCommand, RunLocaleReport, RunLocaleSetting, RunLocaleSource, choose_run_locale,
    select_run_locale,
};
pub use path::{
    ComposedRunPath, RebuildGate, RunPath, RunPathBuild, RunPathReport, RunPathSettings,
    RunPathSource, build_run_path, log_change,
};
pub use probe::{
    CaptureError, CaptureErrorKind, ENV_PROBE_SUBCOMMAND, LoginShellProbe, choose_shell,
    encode_probe_output, fresh_nonce, parse_probe_output,
};
pub use registry::join_registry_paths;

/// Variables removed from every run's env (#26, architecture §4; TS parity, `src/runner.ts`
/// `cleanSpawnEnv`): a parent Claude Code session's markers and its frozen OAuth token, which
/// break a detached child's auth.
pub const STRIPPED: [&str; 3] = [
    "CLAUDECODE",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
];

/// Removed from `direct` runs only: `direct` never imports a Config dir set outside the
/// Daemon; a non-default one comes from a Launcher script (#48).
pub const DIRECT_STRIPPED: &str = "CLAUDE_CONFIG_DIR";

/// Names the Daemon tells a run which Daemon it belongs to (#22).
pub const CLAUDECLAW_DIR: &str = "CLAUDECLAW_DIR";
/// The run's ID, the marker the Reap finds it by (#29).
pub const CLAUDECLAW_RUN_ID: &str = "CLAUDECLAW_RUN_ID";
/// Background tasks would keep `claude --print` alive past its `result` (#47 D2).
pub const DISABLE_BACKGROUND_TASKS: &str = "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS";

/// One env change for a child: set the variable, or remove it (`None`). The spawn's
/// `SpawnSpec.env` takes a list of these, applied in order on top of the Worker's env.
pub type EnvChange = (OsString, Option<OsString>);

/// The run environment every spawn applies (#26 D1, #43 D1): the Run PATH and, when the Run
/// locale adds one, an `LC_CTYPE`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunEnv {
    pub path: RunPath,
    pub lc_ctype: Option<String>,
}

/// What identifies one run to [`RunEnv::child_env`].
#[derive(Debug, Clone, Copy)]
pub struct RunVars<'a> {
    /// The Daemon's data dir, as `CLAUDECLAW_DIR` (#22).
    pub claudeclaw_dir: &'a Path,
    /// `CLAUDECLAW_RUN_ID` (#29).
    pub run_id: &'a str,
    /// The run's Launcher; `direct` also drops `CLAUDE_CONFIG_DIR` (#48).
    pub launcher: &'a Launcher,
}

impl RunEnv {
    /// The env changes for one Launcher run, in the order to apply them:
    /// 1. remove [`STRIPPED`], and [`DIRECT_STRIPPED`] for `direct` (#48);
    /// 2. set `PATH` to the Run PATH (#26) and `LC_CTYPE` when the Run locale adds one (#43);
    /// 3. set `CLAUDECLAW_DIR` (#22), `CLAUDECLAW_RUN_ID` (#29) and
    ///    `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` (#47), last, so nothing earlier can undo
    ///    them.
    ///
    /// Every other variable stays as the Worker inherited it.
    pub fn child_env(&self, run: &RunVars<'_>) -> Vec<EnvChange> {
        let mut changes: Vec<EnvChange> = STRIPPED
            .iter()
            .map(|name| (OsString::from(name), None))
            .collect();
        if matches!(run.launcher, Launcher::Direct) {
            changes.push((OsString::from(DIRECT_STRIPPED), None));
        }
        changes.push((OsString::from("PATH"), Some(self.path.to_os_string())));
        if let Some(lc_ctype) = &self.lc_ctype {
            changes.push((OsString::from("LC_CTYPE"), Some(OsString::from(lc_ctype))));
        }
        changes.push((
            OsString::from(CLAUDECLAW_DIR),
            Some(run.claudeclaw_dir.as_os_str().to_owned()),
        ));
        changes.push((OsString::from(CLAUDECLAW_RUN_ID), Some(run.run_id.into())));
        changes.push((OsString::from(DISABLE_BACKGROUND_TASKS), Some("1".into())));
        changes
    }
}

/// The run-env settings (#26 D1, #43 D4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunEnvSettings {
    pub run_path: RunPathSettings,
    pub run_locale: RunLocaleSetting,
}

/// The parts of the Worker's own env the run env is built from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InheritedEnv {
    pub path: Option<OsString>,
    pub locale: InheritedLocale,
}

impl InheritedEnv {
    /// Reads `PATH`, `LC_ALL`, `LC_CTYPE` and `LANG` from the Worker's env.
    pub fn from_process() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            locale: InheritedLocale::from_process(),
        }
    }
}

/// The result of [`build_run_env`]: the env, and the reports for status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEnvBuild {
    pub env: RunEnv,
    pub run_path: RunPathReport,
    pub run_locale: RunLocaleReport,
}

/// Builds the run env (#26 D1, #43 D1): the Run PATH ([`build_run_path`]), then the Run locale
/// checked against it ([`choose_run_locale`]). `exe` is the Daemon's own binary, run as
/// `__env-probe` by the login shell; `previous` is the Run PATH being replaced, if any, for
/// `run_path.changed`.
///
/// Called at Worker start, on a `run_path` or `run_locale` change and before Test launcher.
/// After a resolution miss, rebuild only the Run PATH, behind a [`RebuildGate`] (#26 D3); the
/// locale doesn't change mid-run (#43 D1).
pub async fn build_run_env(
    settings: &RunEnvSettings,
    inherited: &InheritedEnv,
    exe: &Path,
    previous: Option<&RunPath>,
) -> RunEnvBuild {
    let built = build_run_path(&settings.run_path, inherited.path.as_deref(), exe, previous).await;
    let run_path_value = built.path.to_os_string();
    let run_locale = choose_run_locale(
        settings.run_locale,
        &inherited.locale,
        OsStr::new(&run_path_value),
    )
    .await;
    RunEnvBuild {
        env: RunEnv {
            path: built.path,
            lc_ctype: run_locale.lc_ctype_added.clone(),
        },
        run_path: built.report,
        run_locale,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::launcher::AbsPath;

    fn env(lc_ctype: Option<&str>) -> RunEnv {
        let dir = std::env::temp_dir();
        RunEnv {
            path: RunPath::compose(&[dir], None, None).path,
            lc_ctype: lc_ctype.map(str::to_owned),
        }
    }

    fn render(changes: &[EnvChange]) -> Vec<(String, Option<String>)> {
        changes
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.as_ref().map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn stripped_list_matches_ts() {
        // TS parity: `cleanSpawnEnv`, src/runner.ts:108-112.
        assert_eq!(
            STRIPPED,
            [
                "CLAUDECODE",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
            ]
        );
    }

    #[test]
    fn direct_run_changes() {
        let data_dir = std::env::temp_dir().join("data");
        let got = render(&env(Some("C.UTF-8")).child_env(&RunVars {
            claudeclaw_dir: &data_dir,
            run_id: "1234",
            launcher: &Launcher::Direct,
        }));
        let path = std::env::temp_dir().to_string_lossy().into_owned();
        let s = |k: &str, v: &str| (k.to_owned(), Some(v.to_owned()));
        let r = |k: &str| (k.to_owned(), None);
        assert_eq!(
            got,
            vec![
                r("CLAUDECODE"),
                r("CLAUDE_CODE_OAUTH_TOKEN"),
                r("CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST"),
                r("CLAUDE_CONFIG_DIR"),
                s("PATH", &path),
                s("LC_CTYPE", "C.UTF-8"),
                s("CLAUDECLAW_DIR", &data_dir.to_string_lossy()),
                s("CLAUDECLAW_RUN_ID", "1234"),
                s("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"),
            ]
        );
    }

    #[test]
    fn a_path_launcher_keeps_the_config_dir_and_no_locale_adds_nothing() {
        let launcher = Launcher::Path(AbsPath::new(std::env::temp_dir().join("l")).unwrap());
        let got = render(&env(None).child_env(&RunVars {
            claudeclaw_dir: &PathBuf::from("unused"),
            run_id: "1",
            launcher: &launcher,
        }));
        let names: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!names.contains(&"CLAUDE_CONFIG_DIR"));
        assert!(!names.contains(&"LC_CTYPE"));
        // The background-task switch is always the last change (#47 D2).
        assert_eq!(
            got.last().unwrap(),
            &(
                "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS".to_owned(),
                Some("1".to_owned())
            )
        );
    }

    #[tokio::test]
    async fn builds_with_inherit_settings() {
        let settings = RunEnvSettings {
            run_path: RunPathSettings {
                source: RunPathSource::Inherit,
                prepend: vec![],
            },
            run_locale: RunLocaleSetting::Inherit,
        };
        let inherited = InheritedEnv {
            path: Some(std::env::temp_dir().into_os_string()),
            locale: InheritedLocale::default(),
        };
        let built = build_run_env(&settings, &inherited, Path::new("unused"), None).await;
        assert_eq!(built.env.path.dirs(), &[std::env::temp_dir()]);
        assert_eq!(built.env.lc_ctype, None);
        let want = if cfg!(windows) {
            RunLocaleSource::NotApplicable
        } else {
            RunLocaleSource::Setting
        };
        assert_eq!(built.run_locale.source, want);
    }
}

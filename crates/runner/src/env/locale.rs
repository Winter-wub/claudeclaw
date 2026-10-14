//! The Run locale: a UTF-8 character type for every run (#43).

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

/// `run_locale` (#43 D4). Mirrors `claudeclaw_api::settings::RunLocaleSetting`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunLocaleSetting {
    /// Add a UTF-8 `LC_CTYPE` when the inherited env has none (#43 D1).
    #[default]
    Auto,
    /// Add nothing: 1.x's C locale.
    Inherit,
}

impl RunLocaleSetting {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Inherit => "inherit",
        }
    }
}

/// Why the Run locale is what it is (#43 D1, D6). Mirrors
/// `claudeclaw_api::status::RunLocaleSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLocaleSource {
    /// The inherited ctype is already UTF-8.
    Inherited,
    /// `LC_CTYPE` was added.
    Default,
    /// An inherited `LC_ALL` decides.
    LcAll,
    /// `run_locale: inherit`.
    Setting,
    /// No UTF-8 candidate exists on this machine.
    Unavailable,
    /// Windows.
    NotApplicable,
}

impl RunLocaleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherited => "inherited",
            Self::Default => "default",
            Self::LcAll => "lc_all",
            Self::Setting => "setting",
            Self::Unavailable => "unavailable",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// The candidates tried, in order, when the inherited ctype isn't UTF-8 (#43 D1 step 4).
pub const CANDIDATES: [&str; 3] = ["C.UTF-8", "UTF-8", "en_US.UTF-8"];

/// The charmap a name must report to count as UTF-8 (#43 D3).
const UTF8: &str = "UTF-8";

/// The locale variables the Daemon inherited, as far as the Run locale cares (#43 D1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InheritedLocale {
    pub lc_all: Option<OsString>,
    pub lc_ctype: Option<OsString>,
    pub lang: Option<OsString>,
}

impl InheritedLocale {
    /// Reads `LC_ALL`, `LC_CTYPE` and `LANG` from the Worker's env (never written).
    pub fn from_process() -> Self {
        Self {
            lc_all: std::env::var_os("LC_ALL"),
            lc_ctype: std::env::var_os("LC_CTYPE"),
            lang: std::env::var_os("LANG"),
        }
    }

    fn non_empty(v: &Option<OsString>) -> Option<String> {
        v.as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string_lossy().into_owned())
    }
}

/// What a Run locale choice did, for `DaemonStatus.run_locale`
/// (`claudeclaw_api::status::RunLocale`, #43 D5) and the Daemon log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLocaleReport {
    pub setting: RunLocaleSetting,
    pub source: RunLocaleSource,
    /// The `LC_CTYPE` value every run gets, or `None` when none is added.
    pub lc_ctype_added: Option<String>,
    /// The ctype name runs see.
    pub effective: String,
    /// `locale charmap` for `effective`; empty when it couldn't be asked.
    pub charmap: String,
    /// `false` when `locale` wasn't found and nothing was checked (#43 D3).
    pub verified: bool,
}

/// Why a charmap couldn't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharmapError {
    /// There is no `locale` program on the Run PATH.
    NoLocaleCommand,
    /// `locale charmap` failed or timed out.
    Failed,
}

/// Asks what charmap a ctype name gives (#43 D3).
pub trait CharmapCheck {
    /// The charmap `name` gives as `LC_CTYPE`, e.g. `UTF-8` or `US-ASCII`.
    fn charmap(&self, name: &str) -> impl Future<Output = Result<String, CharmapError>> + Send;
}

/// [`CharmapCheck`] by running `locale charmap` (#43 D3): `locale` is resolved on the Run PATH,
/// its env is cleared except `PATH` and `LC_CTYPE=<name>`, and it has 2 s.
#[derive(Debug, Clone)]
pub struct LocaleCommand {
    /// The resolved `locale` program, or `None` when the Run PATH has none.
    pub locale: Option<PathBuf>,
    /// The Run PATH, given to `locale` as `PATH`.
    pub path: OsString,
    pub timeout: Duration,
}

impl LocaleCommand {
    /// The check's timeout (#43 D3).
    pub const TIMEOUT: Duration = Duration::from_secs(2);

    /// `locale` resolved on `path` (the Run PATH).
    pub fn on_path(path: &OsStr) -> Self {
        let locale = which::which_in("locale", Some(path), std::env::temp_dir()).ok();
        Self {
            locale,
            path: path.to_owned(),
            timeout: Self::TIMEOUT,
        }
    }
}

impl CharmapCheck for LocaleCommand {
    async fn charmap(&self, name: &str) -> Result<String, CharmapError> {
        let Some(locale) = &self.locale else {
            return Err(CharmapError::NoLocaleCommand);
        };
        let output = tokio::process::Command::new(locale)
            .arg("charmap")
            .env_clear()
            .env("PATH", &self.path)
            .env("LC_CTYPE", name)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(self.timeout, output).await {
            Ok(Ok(out)) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout)
                .trim_end_matches(['\n', '\r'])
                .to_owned()),
            _ => Err(CharmapError::Failed),
        }
    }
}

/// What to do when there is no `locale` program (#43 D3): on Linux `C.UTF-8` is used
/// unchecked (musl treats every name as UTF-8, glibc 2.35+ has `C.UTF-8` built in); on macOS
/// `/usr/bin/locale` is part of the base system, so a miss means the Run locale is
/// unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoLocaleCommand {
    AssumeCUtf8,
    Unavailable,
}

impl NoLocaleCommand {
    /// This OS's rule.
    pub fn for_this_os() -> Self {
        if cfg!(target_os = "macos") {
            Self::Unavailable
        } else {
            Self::AssumeCUtf8
        }
    }
}

/// Chooses the Run locale (#43 D1) on macOS and Linux. Use [`choose_run_locale`] for the
/// OS-dependent entry point, which is a no-op on Windows.
pub async fn select_run_locale(
    setting: RunLocaleSetting,
    inherited: &InheritedLocale,
    check: &impl CharmapCheck,
    no_locale: NoLocaleCommand,
) -> RunLocaleReport {
    let lc_all = InheritedLocale::non_empty(&inherited.lc_all);
    let ctype = InheritedLocale::non_empty(&inherited.lc_ctype)
        .or_else(|| InheritedLocale::non_empty(&inherited.lang));
    let report = |source, lc_ctype_added, effective: String, charmap, verified| RunLocaleReport {
        setting,
        source,
        lc_ctype_added,
        effective,
        charmap,
        verified,
    };

    // 1. `run_locale: inherit` adds nothing.
    if setting == RunLocaleSetting::Inherit {
        let effective = lc_all.or(ctype).unwrap_or_else(|| "C".to_owned());
        let (charmap, verified) = describe(check, &effective).await;
        return report(RunLocaleSource::Setting, None, effective, charmap, verified);
    }

    // 2. An inherited `LC_ALL` forces every category; `LC_CTYPE` couldn't override it.
    if let Some(lc_all) = lc_all {
        let (charmap, verified) = describe(check, &lc_all).await;
        if verified && charmap != UTF8 {
            tracing::warn!(
                event = "run_locale.not_utf8",
                "an inherited LC_ALL forces a character type that isn't UTF-8"
            );
        }
        return report(RunLocaleSource::LcAll, None, lc_all, charmap, verified);
    }

    // 3. The inherited ctype is already UTF-8.
    if let Some(name) = &ctype {
        match check.charmap(name).await {
            Ok(charmap) if charmap == UTF8 => {
                return report(
                    RunLocaleSource::Inherited,
                    None,
                    name.clone(),
                    charmap,
                    true,
                );
            }
            Err(CharmapError::NoLocaleCommand) => {
                return without_locale_command(setting, ctype, no_locale);
            }
            _ => {}
        }
    }

    // 4. The first candidate that checks as UTF-8.
    for candidate in CANDIDATES {
        match check.charmap(candidate).await {
            Ok(charmap) if charmap == UTF8 => {
                return report(
                    RunLocaleSource::Default,
                    Some(candidate.to_owned()),
                    candidate.to_owned(),
                    charmap,
                    true,
                );
            }
            Err(CharmapError::NoLocaleCommand) => {
                return without_locale_command(setting, ctype, no_locale);
            }
            _ => {}
        }
    }

    // 5. Nothing is UTF-8 here.
    unavailable(setting, ctype)
}

fn without_locale_command(
    setting: RunLocaleSetting,
    ctype: Option<String>,
    no_locale: NoLocaleCommand,
) -> RunLocaleReport {
    match no_locale {
        NoLocaleCommand::AssumeCUtf8 => RunLocaleReport {
            setting,
            source: RunLocaleSource::Default,
            lc_ctype_added: Some(CANDIDATES[0].to_owned()),
            effective: CANDIDATES[0].to_owned(),
            charmap: UTF8.to_owned(),
            verified: false,
        },
        NoLocaleCommand::Unavailable => unavailable(setting, ctype),
    }
}

fn unavailable(setting: RunLocaleSetting, ctype: Option<String>) -> RunLocaleReport {
    RunLocaleReport {
        setting,
        source: RunLocaleSource::Unavailable,
        lc_ctype_added: None,
        effective: ctype.unwrap_or_else(|| "C".to_owned()),
        charmap: String::new(),
        verified: false,
    }
}

/// The charmap for a status line, and whether it was really asked.
async fn describe(check: &impl CharmapCheck, name: &str) -> (String, bool) {
    match check.charmap(name).await {
        Ok(charmap) => (charmap, true),
        Err(_) => (String::new(), false),
    }
}

/// Chooses the Run locale for this OS and logs it (#43 D1, D5, D6): on macOS and Linux,
/// [`select_run_locale`] with `locale` resolved on `run_path`; on Windows, nothing
/// (`not_applicable`).
///
/// Logs `run_locale.chosen {source, lc_ctype, charmap, verified}`, or `run_locale.unavailable`
/// (and `run_locale.not_utf8` for a non-UTF-8 `LC_ALL`).
pub async fn choose_run_locale(
    setting: RunLocaleSetting,
    inherited: &InheritedLocale,
    run_path: &OsStr,
) -> RunLocaleReport {
    let report = if cfg!(windows) {
        RunLocaleReport {
            setting,
            source: RunLocaleSource::NotApplicable,
            lc_ctype_added: None,
            effective: String::new(),
            charmap: String::new(),
            verified: false,
        }
    } else {
        let check = LocaleCommand::on_path(run_path);
        select_run_locale(setting, inherited, &check, NoLocaleCommand::for_this_os()).await
    };
    log_choice(&report);
    report
}

fn log_choice(report: &RunLocaleReport) {
    if report.source == RunLocaleSource::Unavailable {
        tracing::warn!(
            event = "run_locale.unavailable",
            "no UTF-8 character type is available for runs"
        );
    } else {
        tracing::info!(
            event = "run_locale.chosen",
            source = report.source.as_str(),
            lc_ctype = report.lc_ctype_added.as_deref(),
            charmap = report.charmap.as_str(),
            verified = report.verified,
            "Run locale chosen"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// A fake `locale charmap`: a table of names to charmaps; any other name gives
    /// `US-ASCII`, as macOS does for an unknown name.
    struct Table {
        known: HashMap<&'static str, &'static str>,
        missing: bool,
        asked: Mutex<Vec<String>>,
    }

    impl Table {
        fn new(known: &[(&'static str, &'static str)]) -> Self {
            Self {
                known: known.iter().copied().collect(),
                missing: false,
                asked: Mutex::new(Vec::new()),
            }
        }
        fn missing() -> Self {
            Self {
                missing: true,
                ..Self::new(&[])
            }
        }
    }

    impl CharmapCheck for Table {
        async fn charmap(&self, name: &str) -> Result<String, CharmapError> {
            self.asked.lock().unwrap().push(name.to_owned());
            if self.missing {
                return Err(CharmapError::NoLocaleCommand);
            }
            Ok(self
                .known
                .get(name)
                .copied()
                .unwrap_or("US-ASCII")
                .to_owned())
        }
    }

    fn inherited(lc_all: &str, lc_ctype: &str, lang: &str) -> InheritedLocale {
        let v = |s: &str| (!s.is_empty()).then(|| OsString::from(s));
        InheritedLocale {
            lc_all: v(lc_all),
            lc_ctype: v(lc_ctype),
            lang: v(lang),
        }
    }

    const MAC: &[(&str, &str)] = &[
        ("C.UTF-8", "UTF-8"),
        ("UTF-8", "UTF-8"),
        ("en_US.UTF-8", "UTF-8"),
        ("C", "US-ASCII"),
    ];

    #[tokio::test]
    async fn selection_table() {
        use RunLocaleSetting::{Auto, Inherit};
        use RunLocaleSource as S;
        struct Case {
            name: &'static str,
            setting: RunLocaleSetting,
            env: (&'static str, &'static str, &'static str),
            known: &'static [(&'static str, &'static str)],
            no_locale: NoLocaleCommand,
            missing: bool,
            source: RunLocaleSource,
            added: Option<&'static str>,
            effective: &'static str,
            verified: bool,
        }
        let base = Case {
            name: "",
            setting: Auto,
            env: ("", "", ""),
            known: MAC,
            no_locale: NoLocaleCommand::Unavailable,
            missing: false,
            source: S::Default,
            added: Some("C.UTF-8"),
            effective: "C.UTF-8",
            verified: true,
        };
        let cases = [
            Case {
                name: "a GUI start with no locale gets C.UTF-8",
                ..base
            },
            Case {
                name: "run_locale: inherit adds nothing",
                setting: Inherit,
                source: S::Setting,
                added: None,
                effective: "C",
                ..base
            },
            Case {
                name: "LC_ALL decides, even when it isn't UTF-8",
                env: ("C", "", "en_US.UTF-8"),
                source: S::LcAll,
                added: None,
                effective: "C",
                ..base
            },
            Case {
                name: "an inherited UTF-8 LANG is kept",
                env: ("", "", "en_US.UTF-8"),
                source: S::Inherited,
                added: None,
                effective: "en_US.UTF-8",
                ..base
            },
            Case {
                name: "LC_CTYPE wins over LANG",
                env: ("", "C", "en_US.UTF-8"),
                ..base
            },
            Case {
                name: "a UTF-8 name this machine lacks (th_TH on macOS) gets C.UTF-8",
                env: ("", "", "th_TH.UTF-8"),
                ..base
            },
            Case {
                name: "C.UTF-8 missing falls back to UTF-8",
                known: &[("UTF-8", "UTF-8"), ("en_US.UTF-8", "UTF-8")],
                added: Some("UTF-8"),
                effective: "UTF-8",
                ..base
            },
            Case {
                name: "then to en_US.UTF-8",
                known: &[("en_US.UTF-8", "UTF-8")],
                added: Some("en_US.UTF-8"),
                effective: "en_US.UTF-8",
                ..base
            },
            Case {
                name: "no candidate is UTF-8",
                known: &[],
                source: S::Unavailable,
                added: None,
                effective: "C",
                verified: false,
                ..base
            },
            Case {
                name: "no locale command on Linux: C.UTF-8 unchecked",
                missing: true,
                no_locale: NoLocaleCommand::AssumeCUtf8,
                verified: false,
                ..base
            },
            Case {
                name: "no locale command on macOS: unavailable",
                missing: true,
                source: S::Unavailable,
                added: None,
                effective: "C",
                verified: false,
                ..base
            },
        ];
        for case in cases {
            let check = if case.missing {
                Table::missing()
            } else {
                Table::new(case.known)
            };
            let (a, c, l) = case.env;
            let got =
                select_run_locale(case.setting, &inherited(a, c, l), &check, case.no_locale).await;
            assert_eq!(got.source, case.source, "{}", case.name);
            assert_eq!(got.lc_ctype_added.as_deref(), case.added, "{}", case.name);
            assert_eq!(got.effective, case.effective, "{}", case.name);
            assert_eq!(got.verified, case.verified, "{}", case.name);
            assert_eq!(got.setting, case.setting, "{}", case.name);
        }
    }

    #[tokio::test]
    async fn stops_at_the_first_valid_candidate() {
        let check = Table::new(MAC);
        select_run_locale(
            RunLocaleSetting::Auto,
            &inherited("", "", ""),
            &check,
            NoLocaleCommand::Unavailable,
        )
        .await;
        assert_eq!(*check.asked.lock().unwrap(), vec!["C.UTF-8".to_owned()]);
    }

    #[cfg(unix)]
    mod fake_locale_command {
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;

        use super::*;

        /// A fake `locale` that knows only `UTF-8` and `en_US.UTF-8`, and checks its env was
        /// cleared down to PATH and LC_CTYPE.
        fn fake_locale(dir: &Path) {
            let path = dir.join("locale");
            std::fs::write(
                &path,
                "#!/bin/sh\n\
                 [ \"$1\" = charmap ] || exit 2\n\
                 [ -z \"$LANG\" ] && [ -z \"$LC_ALL\" ] || exit 3\n\
                 case \"$LC_CTYPE\" in\n\
                   UTF-8|en_US.UTF-8) echo UTF-8 ;;\n\
                   slow) sleep 30 ;;\n\
                   *) echo US-ASCII ;;\n\
                 esac\n",
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        #[tokio::test]
        async fn selects_through_a_fake_locale_command() {
            let dir = tempfile::tempdir().unwrap();
            fake_locale(dir.path());
            let path = std::env::join_paths([dir.path(), Path::new("/bin")]).unwrap();
            let check = LocaleCommand::on_path(&path);
            assert_eq!(
                check.locale.as_deref(),
                Some(dir.path().join("locale").as_path())
            );
            let got = select_run_locale(
                RunLocaleSetting::Auto,
                &inherited("", "", "C"),
                &check,
                NoLocaleCommand::Unavailable,
            )
            .await;
            assert_eq!(got.source, RunLocaleSource::Default);
            assert_eq!(got.lc_ctype_added.as_deref(), Some("UTF-8"));
            assert_eq!(got.charmap, "UTF-8");
            assert!(got.verified);
        }

        #[tokio::test]
        async fn a_slow_locale_command_times_out() {
            let dir = tempfile::tempdir().unwrap();
            fake_locale(dir.path());
            let path = std::env::join_paths([dir.path(), Path::new("/bin")]).unwrap();
            let check = LocaleCommand {
                timeout: Duration::from_millis(300),
                ..LocaleCommand::on_path(&path)
            };
            assert_eq!(check.charmap("slow").await, Err(CharmapError::Failed));
        }

        #[tokio::test]
        async fn no_locale_on_the_run_path() {
            let dir = tempfile::tempdir().unwrap();
            let check = LocaleCommand::on_path(dir.path().as_os_str());
            assert_eq!(check.locale, None);
            assert_eq!(
                check.charmap("C.UTF-8").await,
                Err(CharmapError::NoLocaleCommand)
            );
        }
    }
}

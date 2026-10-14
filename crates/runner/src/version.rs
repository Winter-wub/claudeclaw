//! The minimum Claude Code and the version it reports (architecture §4 "Minimum Claude Code";
//! #42).
//!
//! The version comes from each run's `system/init` event ([`crate::stream`]); nothing spawns
//! `claude --version` (#42 D2). Below [`MIN_CLAUDE_CODE`] runs still go ahead: the caller raises
//! `claude_code_outdated` from [`VersionCheck::Outdated`] (#42 D4).

use std::fmt;
use std::str::FromStr;

/// The oldest Claude Code the Daemon supports (#42 D1): the newest of every flag `ClaudeArgs`
/// emits, every stream field the runner reads, and every upstream fix that turns a good run into
/// a wrong outcome in the Daemon's mode (`--print`, stream-json, piped stdout).
pub const MIN_CLAUDE_CODE: Version = Version::new(2, 1, 214);

/// A Claude Code `MAJOR.MINOR.PATCH` (#42 D2). Pre-release and build text is not kept: only the
/// leading triple is compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// A version from its three parts.
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Why a version string has no leading `MAJOR.MINOR.PATCH` (#42 D2). Carries no input text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    /// The string doesn't start with three dot-separated decimal numbers.
    #[error("not a MAJOR.MINOR.PATCH version")]
    Unparsed,
}

impl FromStr for Version {
    type Err = VersionError;

    /// Lenient parse (#42 D2): leading whitespace is skipped, the leading `MAJOR.MINOR.PATCH` is
    /// read, and anything after it that doesn't continue the number (`-beta`, ` (Claude Code)`)
    /// is ignored. `2.1` and `v2.1.3` are rejected.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut rest = s.trim_start();
        let mut parts = [0u64; 3];
        for (i, part) in parts.iter_mut().enumerate() {
            if i > 0 {
                rest = rest.strip_prefix('.').ok_or(VersionError::Unparsed)?;
            }
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return Err(VersionError::Unparsed);
            }
            *part = rest[..digits].parse().map_err(|_| VersionError::Unparsed)?;
            rest = &rest[digits..];
        }
        // `2.1.214.5` or `2.1.2x` would otherwise read as a valid prefix of something else.
        if rest.starts_with(|c: char| c == '.' || c.is_ascii_alphanumeric()) {
            return Err(VersionError::Unparsed);
        }
        Ok(Version::new(parts[0], parts[1], parts[2]))
    }
}

/// How a reported version compares with [`MIN_CLAUDE_CODE`] (#42 D2, D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionCheck {
    /// At or above the minimum.
    Supported,
    /// Below the minimum: the run goes ahead with a `claude_code_outdated` warning (#42 D4).
    Outdated,
    /// Unparseable. Compared as unknown, which never warns (#42 D2).
    Unknown,
}

/// The version a run's `init` reported: the raw string, kept as-is, and its parse (#42 D2, D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCodeVersion {
    /// Exactly what `init.claude_code_version` said; recorded on the run (#42 D5).
    pub raw: String,
    /// `None` when [`Version::from_str`] couldn't read it.
    pub parsed: Option<Version>,
}

impl ClaudeCodeVersion {
    /// Parses `raw` leniently and keeps it.
    pub fn new(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        let parsed = raw.parse().ok();
        Self { raw, parsed }
    }

    /// The `claude_code_outdated` judgement (#42 D4).
    pub fn check(&self) -> VersionCheck {
        match self.parsed {
            None => VersionCheck::Unknown,
            Some(v) if v < MIN_CLAUDE_CODE => VersionCheck::Outdated,
            Some(_) => VersionCheck::Supported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_leniently() {
        let cases: &[(&str, Option<Version>)] = &[
            ("2.1.214", Some(Version::new(2, 1, 214))),
            ("2.1.296 (Claude Code)", Some(Version::new(2, 1, 296))),
            ("  2.1.296", Some(Version::new(2, 1, 296))),
            ("2.1.300-beta.1", Some(Version::new(2, 1, 300))),
            ("2.1.300+build", Some(Version::new(2, 1, 300))),
            ("10.0.0", Some(Version::new(10, 0, 0))),
            ("2.1", None),
            ("2.1.", None),
            ("v2.1.3", None),
            ("2.1.214.5", None),
            ("2.1.2x", None),
            ("", None),
            ("unknown", None),
        ];
        for (input, want) in cases {
            assert_eq!(input.parse::<Version>().ok(), *want, "input {input:?}");
        }
        // A part that overflows u64, built at runtime so no long digit run sits in the source.
        let overflow = format!("{}.0.0", "9".repeat(25));
        assert_eq!(overflow.parse::<Version>(), Err(VersionError::Unparsed));
    }

    #[test]
    fn compares_with_the_minimum() {
        let cases: &[(&str, VersionCheck)] = &[
            ("2.1.214", VersionCheck::Supported),
            ("2.1.296", VersionCheck::Supported),
            ("2.2.0", VersionCheck::Supported),
            ("3.0.0", VersionCheck::Supported),
            ("2.1.213", VersionCheck::Outdated),
            ("2.0.33", VersionCheck::Outdated),
            ("1.99.999", VersionCheck::Outdated),
            ("garbage", VersionCheck::Unknown),
        ];
        for (input, want) in cases {
            assert_eq!(
                ClaudeCodeVersion::new(*input).check(),
                *want,
                "input {input:?}"
            );
        }
    }

    #[test]
    fn keeps_the_raw_string() {
        let v = ClaudeCodeVersion::new("2.1.296 (Claude Code)");
        assert_eq!(v.raw, "2.1.296 (Claude Code)");
        assert_eq!(v.parsed, Some(Version::new(2, 1, 296)));
        assert_eq!(MIN_CLAUDE_CODE.to_string(), "2.1.214");
    }

    #[test]
    fn ordering_is_numeric_not_lexical() {
        assert!(Version::new(2, 1, 99) < Version::new(2, 1, 214));
        assert!(Version::new(2, 10, 0) > Version::new(2, 9, 999));
    }
}

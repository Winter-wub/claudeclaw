//! `.claude/settings.json`: Claude Code's own project settings, of which Rust patches only the
//! `statusLine` key (architecture §4 "Statusline", §9.1 owner C, #24).
//!
//! The patch is a single-key change over a fresh read of the raw JSON (`preserve_order`), written
//! in TS's format through an atomic rename that keeps the file's mode (#24 D3). An unparseable
//! file is never written. A foreign `statusLine` is never overwritten without `force`, and never
//! removed. Recognising and migrating TS's value at Worker start is `ts-compat::leftovers`'s job;
//! this module only classifies it.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::atomic::{self, DirMode, FileMode};
use crate::error::StoreError;
use crate::layout::Layout;
use crate::settings::to_ts_json;

/// The 1.x TS daemon's `statusLine.command` (`src/commands/start.ts:194-197`).
pub const TS_STATUS_LINE_COMMAND: &str = "node .claude/statusline.cjs";

/// The `refreshInterval` the Rust statusline asks for, in seconds (#24 D3).
pub const REFRESH_INTERVAL_SECS: u64 = 30;

/// Whose `statusLine` is in the file (#24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLineOwner {
    /// No `statusLine` key (or no file).
    Absent,
    /// `command` is `claudeclaw statusline` or `<abs path>/claudeclaw statusline`, with any
    /// arguments after it.
    Ours,
    /// `command` is exactly TS's `node .claude/statusline.cjs`.
    Ts,
    /// Anything else.
    Foreign,
}

/// What [`install_status_line`] did, as `statusline install` reports it (#24 D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallAction {
    /// Written where there was none, or our own entry's command was updated.
    Installed,
    /// TS's entry was switched to ours, keeping its other keys.
    Migrated,
    /// Ours already, with the same command: nothing written.
    Unchanged,
    /// A foreign entry was replaced (`--force`).
    Replaced,
}

/// The result of [`install_status_line`].
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub path: PathBuf,
    pub action: InstallAction,
    /// The `statusLine` value before, when there was one.
    pub previous: Option<Value>,
}

/// Why the `statusLine` patch wasn't written.
#[derive(Debug, thiserror::Error)]
pub enum StatusLineError {
    /// A foreign `statusLine` is in place (`statusline_foreign`, a CLI-only error; #24).
    #[error("{} has a statusLine that isn't ClaudeClaw's", path.display())]
    Foreign { path: PathBuf, previous: Value },
    /// Includes an unparseable file, which is never written (#24).
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Classifies a `statusLine` value (#24).
pub fn classify(status_line: Option<&Value>) -> StatusLineOwner {
    let Some(value) = status_line else {
        return StatusLineOwner::Absent;
    };
    match value.get("command").and_then(Value::as_str) {
        Some(TS_STATUS_LINE_COMMAND) => StatusLineOwner::Ts,
        Some(command) if is_our_command(command) => StatusLineOwner::Ours,
        _ => StatusLineOwner::Foreign,
    }
}

/// `claudeclaw statusline …` or `<abs path>/claudeclaw statusline …`; the path may be
/// double-quoted when it holds spaces.
fn is_our_command(command: &str) -> bool {
    let command = command.trim_start();
    let (exe, rest) = match command.strip_prefix('"') {
        Some(quoted) => match quoted.split_once('"') {
            Some(parts) => parts,
            None => return false,
        },
        None => command
            .split_once(char::is_whitespace)
            .unwrap_or((command, "")),
    };
    let mut args = rest.split_whitespace();
    if args.next() != Some("statusline") {
        return false;
    }
    let path = Path::new(exe);
    let name = path.file_name().and_then(|n| n.to_str());
    let is_claudeclaw = matches!(name, Some("claudeclaw" | "claudeclaw.exe"));
    is_claudeclaw && (exe == "claudeclaw" || exe == "claudeclaw.exe" || path.has_root())
}

/// Reads the `statusLine` value of `.claude/settings.json`. `None` when the file or key is
/// missing; an unparseable file is an error.
pub fn read_status_line(layout: &Layout) -> Result<Option<Value>, StoreError> {
    Ok(read(&layout.claude_settings())?.and_then(|mut map| map.shift_remove("statusLine")))
}

/// Installs `{"type": "command", "command": <command>, "refreshInterval": 30}` as `statusLine`
/// (#24 D3). TS's entry is migrated (command switched, `refreshInterval` added if absent, other
/// keys such as `padding` kept); our own entry gets the new command; a foreign one is refused
/// unless `force`. The caller builds `command` (bare `claudeclaw statusline` when the binary is
/// on its own PATH, else its unresolved absolute path).
pub fn install_status_line(
    layout: &Layout,
    command: &str,
    force: bool,
) -> Result<Installed, StatusLineError> {
    let path = layout.claude_settings();
    let mut map = read(&path)?.unwrap_or_default();
    let previous = map.get("statusLine").cloned();
    let fresh = json!({
        "type": "command",
        "command": command,
        "refreshInterval": REFRESH_INTERVAL_SECS,
    });
    let (value, action) = match classify(previous.as_ref()) {
        StatusLineOwner::Absent => (fresh, InstallAction::Installed),
        StatusLineOwner::Ts | StatusLineOwner::Ours => {
            let ts = classify(previous.as_ref()) == StatusLineOwner::Ts;
            let mut entry = previous.clone().expect("present");
            let object = entry.as_object_mut().expect("a command entry is an object");
            if !ts && object.get("command").and_then(Value::as_str) == Some(command) {
                return Ok(Installed {
                    path,
                    action: InstallAction::Unchanged,
                    previous,
                });
            }
            object.insert("command".into(), Value::String(command.into()));
            if !object.contains_key("refreshInterval") {
                object.insert("refreshInterval".into(), json!(REFRESH_INTERVAL_SECS));
            }
            let action = if ts {
                InstallAction::Migrated
            } else {
                InstallAction::Installed
            };
            (entry, action)
        }
        StatusLineOwner::Foreign if force => (fresh, InstallAction::Replaced),
        StatusLineOwner::Foreign => {
            return Err(StatusLineError::Foreign {
                path,
                previous: previous.expect("present"),
            });
        }
    };
    map.insert("statusLine".into(), value);
    write(layout, &path, map)?;
    Ok(Installed {
        path,
        action,
        previous,
    })
}

/// Removes our `statusLine` (#24 D3). `Ok(false)` when there is none; a TS or foreign entry is
/// never removed.
pub fn remove_status_line(layout: &Layout) -> Result<bool, StatusLineError> {
    let path = layout.claude_settings();
    let Some(mut map) = read(&path)? else {
        return Ok(false);
    };
    match classify(map.get("statusLine")) {
        StatusLineOwner::Absent => Ok(false),
        StatusLineOwner::Ours => {
            map.shift_remove("statusLine");
            write(layout, &path, map)?;
            Ok(true)
        }
        StatusLineOwner::Ts | StatusLineOwner::Foreign => Err(StatusLineError::Foreign {
            previous: map["statusLine"].clone(),
            path,
        }),
    }
}

fn read(path: &Path) -> Result<Option<Map<String, Value>>, StoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StoreError::io("read", path)(e)),
    };
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(map)) => Ok(Some(map)),
        Ok(_) => Err(StoreError::NotAnObject {
            path: path.to_path_buf(),
        }),
        Err(source) => Err(StoreError::Json {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write(layout: &Layout, path: &Path, map: Map<String, Value>) -> Result<(), StoreError> {
    atomic::create_dir(&layout.claude_dir(), DirMode::Default)?;
    atomic::write_atomic(path, &to_ts_json(&Value::Object(map)), FileMode::Keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "claudeclaw statusline";

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        (dir, layout)
    }

    fn file(layout: &Layout) -> String {
        std::fs::read_to_string(layout.claude_settings()).unwrap()
    }

    #[test]
    fn classifies_ours_ts_and_foreign() {
        let entry = |command: &str| json!({"type": "command", "command": command});
        for (command, owner) in [
            ("claudeclaw statusline", StatusLineOwner::Ours),
            (
                "/opt/homebrew/bin/claudeclaw statusline --json",
                StatusLineOwner::Ours,
            ),
            (
                "\"/Applications/Claude Claw.app/claudeclaw\" statusline",
                StatusLineOwner::Ours,
            ),
            ("node .claude/statusline.cjs", StatusLineOwner::Ts),
            ("node .claude/statusline.cjs --x", StatusLineOwner::Foreign),
            ("bin/claudeclaw statusline", StatusLineOwner::Foreign),
            ("claudeclaw status", StatusLineOwner::Foreign),
            ("my-statusline.sh", StatusLineOwner::Foreign),
        ] {
            assert_eq!(classify(Some(&entry(command))), owner, "{command}");
        }
        assert_eq!(classify(None), StatusLineOwner::Absent);
        assert_eq!(classify(Some(&json!("x"))), StatusLineOwner::Foreign);
    }

    #[test]
    fn installs_into_a_missing_file_and_keeps_other_keys_and_order() {
        let (_dir, layout) = layout();
        let done = install_status_line(&layout, CMD, false).unwrap();
        assert_eq!(done.action, InstallAction::Installed);
        assert_eq!(
            file(&layout),
            "{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \
             \"claudeclaw statusline\",\n    \"refreshInterval\": 30\n  }\n}\n"
        );

        std::fs::write(
            layout.claude_settings(),
            r#"{"z":1,"permissions":{"allow":["Bash"]},"a":2}"#,
        )
        .unwrap();
        install_status_line(&layout, CMD, false).unwrap();
        let map = read(&layout.claude_settings()).unwrap().unwrap();
        let keys: Vec<_> = map.keys().collect();
        assert_eq!(keys, ["z", "permissions", "a", "statusLine"]);
    }

    #[test]
    fn migrates_ts_and_refreshes_ours() {
        let (_dir, layout) = layout();
        std::fs::create_dir_all(layout.claude_dir()).unwrap();
        std::fs::write(
            layout.claude_settings(),
            r#"{"statusLine":{"type":"command","command":"node .claude/statusline.cjs","padding":2}}"#,
        )
        .unwrap();
        let done = install_status_line(&layout, CMD, false).unwrap();
        assert_eq!(done.action, InstallAction::Migrated);
        assert_eq!(
            read_status_line(&layout).unwrap().unwrap(),
            json!({"type": "command", "command": CMD, "padding": 2, "refreshInterval": 30})
        );

        let again = install_status_line(&layout, CMD, false).unwrap();
        assert_eq!(again.action, InstallAction::Unchanged);

        let moved =
            install_status_line(&layout, "/usr/local/bin/claudeclaw statusline", false).unwrap();
        assert_eq!(moved.action, InstallAction::Installed);
        assert_eq!(
            read_status_line(&layout).unwrap().unwrap()["command"],
            "/usr/local/bin/claudeclaw statusline"
        );
    }

    #[test]
    fn never_overwrites_or_removes_a_foreign_value_without_force() {
        let (_dir, layout) = layout();
        std::fs::create_dir_all(layout.claude_dir()).unwrap();
        let foreign = r#"{"statusLine":{"type":"command","command":"my-line.sh"}}"#;
        std::fs::write(layout.claude_settings(), foreign).unwrap();

        let err = install_status_line(&layout, CMD, false).unwrap_err();
        assert!(matches!(err, StatusLineError::Foreign { .. }), "{err:?}");
        let err = remove_status_line(&layout).unwrap_err();
        assert!(matches!(err, StatusLineError::Foreign { .. }), "{err:?}");
        assert_eq!(file(&layout), foreign);

        let done = install_status_line(&layout, CMD, true).unwrap();
        assert_eq!(done.action, InstallAction::Replaced);
        assert_eq!(
            done.previous,
            Some(json!({"type": "command", "command": "my-line.sh"}))
        );
    }

    #[test]
    fn removes_only_ours() {
        let (_dir, layout) = layout();
        assert!(!remove_status_line(&layout).unwrap());
        install_status_line(&layout, CMD, false).unwrap();
        assert!(remove_status_line(&layout).unwrap());
        assert_eq!(file(&layout), "{}\n");
        assert!(!remove_status_line(&layout).unwrap());
    }

    #[test]
    fn an_unparseable_file_is_never_written() {
        let (_dir, layout) = layout();
        std::fs::create_dir_all(layout.claude_dir()).unwrap();
        std::fs::write(layout.claude_settings(), "{ broken").unwrap();
        let err = install_status_line(&layout, CMD, true).unwrap_err();
        assert!(
            matches!(err, StatusLineError::Store(StoreError::Json { .. })),
            "{err:?}"
        );
        assert_eq!(file(&layout), "{ broken");
    }

    #[cfg(unix)]
    #[test]
    fn the_patch_keeps_the_files_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, layout) = layout();
        std::fs::create_dir_all(layout.claude_dir()).unwrap();
        std::fs::write(layout.claude_settings(), "{}").unwrap();
        crate::atomic::set_mode(&layout.claude_settings(), 0o640).unwrap();
        install_status_line(&layout, CMD, false).unwrap();
        let mode = std::fs::metadata(layout.claude_settings())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o640);
    }
}

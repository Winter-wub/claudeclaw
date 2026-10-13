//! `cargo xtask deps`: enforces architecture §3.1, which workspace crate may depend on which.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand};

/// Architecture §3.1, by short name (the package name without `claudeclaw-`). A crate may depend
/// only on the workspace crates in its row, in every dependency kind (normal, build and dev).
/// A workspace member without a row fails the check, and so does a row without a member.
const RULES: &[(&str, &[&str])] = &[
    ("api", &[]),
    ("store", &["api"]),
    ("jobs", &[]),
    ("runner", &[]),
    ("chat", &[]),
    ("scheduler", &["jobs", "store"]),
    (
        "core",
        &["store", "jobs", "runner", "scheduler", "chat", "api"],
    ),
    ("discord", &["chat"]),
    ("server", &["core", "api"]),
    ("client", &["api"]),
    ("ts-compat", &["jobs", "store"]),
    (
        "claudeclaw",
        &[
            "api",
            "store",
            "jobs",
            "runner",
            "scheduler",
            "chat",
            "core",
            "discord",
            "server",
            "client",
            "ts-compat",
        ],
    ),
    ("desktop", &["client", "api"]),
    (
        "parity",
        &[
            "core",
            "runner",
            "store",
            "jobs",
            "ts-compat",
            "chat",
            "discord",
            "api",
        ],
    ),
    // Not in §3.1, which covers the shipped crates: `xtask openapi` exports the API types.
    ("xtask", &["api"]),
];

/// One workspace member, reduced to what the rules look at.
#[derive(Debug, Default)]
struct Member {
    name: String,
    /// Workspace crates this member depends on, with the dependency kinds that name them.
    deps: BTreeMap<String, BTreeSet<&'static str>>,
    /// Tauri features this member's manifest turns on, directly or through its `[features]`.
    tauri_features: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct Workspace {
    members: Vec<Member>,
    /// Tauri's features in the resolved graph, which also catches features a dependency enables.
    resolved_tauri_features: BTreeSet<String>,
}

pub fn run() -> Result<ExitCode> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask has no parent dir")?;
    let metadata = MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .other_options(vec!["--locked".to_owned()])
        .exec()
        .context("cargo metadata failed")?;
    let workspace = Workspace::from_metadata(&metadata);
    let violations = check(&workspace);
    if violations.is_empty() {
        let edges: usize = workspace.members.iter().map(|m| m.deps.len()).sum();
        println!(
            "deps: {} crates, {edges} workspace edges, all allowed by architecture §3.1",
            workspace.members.len()
        );
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!(
        "deps: {} violation(s) of architecture §3.1:",
        violations.len()
    );
    for violation in &violations {
        eprintln!("  {violation}");
    }
    eprintln!(
        "Fix the dependency, or change §3.1 in docs/spec/architecture.md and RULES in xtask/src/deps.rs together."
    );
    Ok(ExitCode::FAILURE)
}

impl Workspace {
    fn from_metadata(metadata: &Metadata) -> Self {
        let packages = metadata.workspace_packages();
        let names: BTreeSet<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        let members = packages
            .iter()
            .map(|package| {
                let mut member = Member {
                    name: short(&package.name).to_owned(),
                    ..Member::default()
                };
                for dep in &package.dependencies {
                    if names.contains(dep.name.as_str()) {
                        member
                            .deps
                            .entry(short(&dep.name).to_owned())
                            .or_default()
                            .insert(kind(dep.kind));
                    }
                    if dep.name == "tauri" {
                        member.tauri_features.extend(dep.features.iter().cloned());
                    }
                }
                for enabled in package.features.values().flatten() {
                    let feature = enabled
                        .strip_prefix("tauri/")
                        .or_else(|| enabled.strip_prefix("tauri?/"));
                    member.tauri_features.extend(feature.map(str::to_owned));
                }
                member
            })
            .collect();
        let resolved_tauri_features = metadata
            .resolve
            .iter()
            .flat_map(|resolve| &resolve.nodes)
            .filter(|node| metadata[&node.id].name.as_str() == "tauri")
            .flat_map(|node| node.features.iter().map(|f| f.to_string()))
            .collect();
        Workspace {
            members,
            resolved_tauri_features,
        }
    }
}

fn check(workspace: &Workspace) -> Vec<String> {
    let rules: BTreeMap<&str, &[&str]> = RULES.iter().copied().collect();
    let mut violations = Vec::new();
    for member in &workspace.members {
        let Some(allowed) = rules.get(member.name.as_str()) else {
            violations.push(format!("{}: workspace member with no rule", member.name));
            continue;
        };
        for (dep, kinds) in &member.deps {
            if !allowed.contains(&dep.as_str()) {
                let kinds: Vec<_> = kinds.iter().copied().collect();
                let allowed = if allowed.is_empty() {
                    "nothing in the workspace".to_owned()
                } else {
                    allowed.join(", ")
                };
                violations.push(format!(
                    "{} → {dep} ({}): forbidden; {} may depend on {allowed}",
                    member.name,
                    kinds.join(", "),
                    member.name
                ));
            }
        }
        if member.tauri_features.contains("tracing") {
            violations.push(format!(
                "{}: turns on Tauri's `tracing` feature; the App log must stay content-free and in one format (#40)",
                member.name
            ));
        }
    }
    for (name, _) in RULES {
        if !workspace.members.iter().any(|m| m.name == *name) {
            violations.push(format!("{name}: has a rule but is not a workspace member"));
        }
    }
    if workspace.resolved_tauri_features.contains("tracing") {
        violations.push("tauri: `tracing` is on in the resolved graph (#40)".to_owned());
    }
    violations
}

fn short(package: &str) -> &str {
    package.strip_prefix("claudeclaw-").unwrap_or(package)
}

fn kind(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Normal => "normal",
        DependencyKind::Development => "dev",
        DependencyKind::Build => "build",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace with one member per rule and every allowed edge present.
    fn allowed_workspace() -> Workspace {
        let members = RULES
            .iter()
            .map(|(name, allowed)| Member {
                name: (*name).to_owned(),
                deps: allowed
                    .iter()
                    .map(|dep| ((*dep).to_owned(), BTreeSet::from(["normal"])))
                    .collect(),
                ..Member::default()
            })
            .collect();
        Workspace {
            members,
            ..Workspace::default()
        }
    }

    fn member<'a>(workspace: &'a mut Workspace, name: &str) -> &'a mut Member {
        workspace
            .members
            .iter_mut()
            .find(|m| m.name == name)
            .unwrap()
    }

    #[test]
    fn this_workspace_follows_the_rules() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let metadata = MetadataCommand::new()
            .manifest_path(root.join("Cargo.toml"))
            .exec()
            .unwrap();
        let workspace = Workspace::from_metadata(&metadata);
        assert_eq!(check(&workspace), Vec::<String>::new());
        assert_eq!(workspace.members.len(), RULES.len());
    }

    #[test]
    fn every_allowed_edge_passes() {
        assert_eq!(check(&allowed_workspace()), Vec::<String>::new());
    }

    #[test]
    fn an_adapter_reaching_past_the_chat_seam_fails() {
        let mut workspace = allowed_workspace();
        member(&mut workspace, "discord")
            .deps
            .insert("core".to_owned(), BTreeSet::from(["normal"]));
        assert_eq!(
            check(&workspace),
            ["discord → core (normal): forbidden; discord may depend on chat"]
        );
    }

    #[test]
    fn dev_dependencies_count_too() {
        let mut workspace = allowed_workspace();
        member(&mut workspace, "jobs")
            .deps
            .insert("store".to_owned(), BTreeSet::from(["dev"]));
        assert_eq!(
            check(&workspace),
            ["jobs → store (dev): forbidden; jobs may depend on nothing in the workspace"]
        );
    }

    #[test]
    fn nothing_may_depend_on_parity_or_desktop() {
        let mut workspace = allowed_workspace();
        let binary = member(&mut workspace, "claudeclaw");
        binary
            .deps
            .insert("desktop".to_owned(), BTreeSet::from(["normal"]));
        binary
            .deps
            .insert("parity".to_owned(), BTreeSet::from(["dev"]));
        assert_eq!(check(&workspace).len(), 2);
    }

    #[test]
    fn members_and_rules_must_match() {
        let mut workspace = allowed_workspace();
        workspace.members.retain(|m| m.name != "chat");
        workspace.members.push(Member {
            name: "telegram".to_owned(),
            ..Member::default()
        });
        assert_eq!(
            check(&workspace),
            [
                "telegram: workspace member with no rule",
                "chat: has a rule but is not a workspace member",
            ]
        );
    }

    #[test]
    fn tauri_tracing_is_forbidden() {
        let mut workspace = allowed_workspace();
        member(&mut workspace, "desktop")
            .tauri_features
            .insert("tracing".to_owned());
        workspace
            .resolved_tauri_features
            .insert("tracing".to_owned());
        assert_eq!(check(&workspace).len(), 2);
    }
}

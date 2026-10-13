# ClaudeClaw (Winter-wub fork) — contributor notes

This repo is a **public** hard fork of `moazbuilds/claudeclaw`. Upstream is no longer tracked; nothing is sent back to it.

> This file is for people (and agents) developing the repo. It is **not** the daemon's agent persona.
> The daemon writes its own `CLAUDE.md` into the directory it runs in, built from `prompts/`
> (`IDENTITY.md`, `SOUL.md`, `USER.md`, …). Edit those to change the persona template.

## Where things stand

- **TypeScript daemon** (`src/`, Bun) is the running implementation. It is **frozen to bug fixes**: no new features land in TS.
- **Rust rewrite** is being built as a cargo workspace beside the TS code (`Cargo.toml`, `crates/`, `xtask/`), following the wayfinder map: [issue #2](https://github.com/Winter-wub/claudeclaw/issues/2). Decisions live in its closed tickets; open tickets are the remaining questions. Resume with `/wayfinder https://github.com/Winter-wub/claudeclaw/issues/2`.
- **Spec** for the Rust daemon is [`docs/spec/architecture.md`](docs/spec/architecture.md) (ends with a Decision index linking every closed ticket) and [`docs/spec/management-api.md`](docs/spec/management-api.md). Change them through a PR, citing the deciding ticket as (#N).
- **Vocabulary** is fixed in [`GLOSSARY.md`](GLOSSARY.md) (Daemon, Watchdog, Worker, Launcher, Scope, Plugin shim, Desktop app). Use those terms in code, docs and issues.
- **Research and contracts** live on `research/*` branches and are linked from their tickets, e.g. the TS daemon's on-disk and Claude Code contract.

## Rules

- **Public repo.** Never put tokens, Discord/guild/channel IDs, personal names, job contents or `/Users/...` paths in commits, docs or issues. Describe shapes, not values. `scripts/check-public-safety.sh` enforces the mechanical part: a pre-commit hook (`git config core.hooksPath .githooks`) and the `Public Safety` CI job. Put your own names/IDs in the local-only `.git/info/public-safety-deny`.
- **Commit identity** is set in this clone's local git config (Winter-wub noreply). Don't commit from other clones that carry a different global identity.
- **Tests:** run `bun test` and `cargo test` from this repo, never from a live daemon directory. `bunfig.toml` sandboxes the cwd, but a live `.claude/claudeclaw/settings.json` has been wiped before.
- **`main`** is the default branch and is protected by a ruleset: **every** change, docs included, lands through a PR whose `ci-ok`, `parity-ok` and `public-safety` checks pass. There is no bypass, so don't push to `main` directly. The CI workflows (#35) are `ci.yml` (Rust fmt, clippy, `cargo xtask deps`, cargo-deny, MSRV → `ci-ok`), `parity.yml` (`cargo test` on 3 OSes, TS typecheck + tests → `parity-ok`), `audit.yml` (RustSec advisories) and `public-safety.yml`. Jobs are filtered by `scripts/ci/changes.sh`, so a docs-only PR skips them and the aggregates still pass. Any failure is real; there are no known-failing tests.
- **Rust conventions** are architecture §2: dependencies are declared once in `[workspace.dependencies]`, every cargo command uses `--locked`, and which crate may depend on which (§3.1) is enforced by `cargo xtask deps`. Change §3.1 and `RULES` in `xtask/src/deps.rs` together.

- **Deploying to a live daemon:** it runs from a dedicated git worktree pinned to a commit on `main`, not from the plugin cache. Move the pin with `scripts/deploy-live.sh` (needs `LIVE_DIR` and `LAUNCHD_LABEL`; `--rollback` restores the previous pin). Never copy files into the plugin cache by hand.

## Development workflow

**Before opening any PR, always run both version bump scripts:**

```bash
bun run bump:plugin-version
bun run bump:marketplace-version
```

Both `plugin-version-guard` and `marketplace-version-guard` are required CI checks. They fail if `.claude-plugin/plugin.json` or `.claude-plugin/marketplace.json` still carry the same version as the merge base. Run the bumps, commit alongside your code changes, and push before creating the PR.

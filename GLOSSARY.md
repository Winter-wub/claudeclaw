# ClaudeClaw

A daemon that runs Claude Code on schedules and in response to chat messages, on behalf of one owner. This glossary covers the Rust hard fork's language.

## Language

**Daemon**:
The long-running `claudeclaw` process that owns the scheduler, the chat adapters and every Claude Code run. It runs headless on its own, with or without the Desktop app.
_Avoid_: server, bot, service

**Watchdog**:
The Daemon's parent process, which only starts, restarts and re-executes the Worker; it never runs Claude Code itself.
_Avoid_: supervisor, monitor

**Worker**:
The Daemon's child process that runs the scheduler, the chat adapters and every Claude Code run.
_Avoid_: daemon (when meaning only this process), runner

**Chat adapter**:
The part of the Daemon that connects one chat transport (Discord now; Telegram and Slack later) to it through the chat seam, owning the connection, who may talk, attachment intake and rendering; it never runs Claude Code itself.
_Avoid_: bot, integration, channel (when meaning the transport)

**Turn**:
One inbound message or scheduled prompt handed to the Daemon for a Scope, queued behind that Scope's earlier Turns.
_Avoid_: request, job (when meaning a chat message), task

**Give-up**:
The Watchdog's final stop after more than 5 Worker crashes in 10 minutes: it records the give-up, DMs the owner and exits, and nothing restarts the Daemon until a person chooses Restart anyway.
_Avoid_: crash, fatal stop, shutdown

**Crash record**:
The bounded history of recent Worker crashes (time, exit, reported reason, redacted stderr tail) that the Watchdog keeps in its status file and the Management API serves.
_Avoid_: crash report, crash log, core dump

**Last-known snapshot**:
The Desktop app's own cached copy of a Daemon's status, jobs and recent log lines, shown read-only and time-labelled while that Daemon is not answering.
_Avoid_: offline mode, stale data, mirror

**Launcher**:
The path to a single runnable executable that the Daemon invokes in place of `claude`, passing the normal `claude` argv. It carries no name and no environment of its own; any wrapping (such as `ccs <profile>`) and its env live in the user's script.
_Avoid_: wrapper, runtime, provider, profile

**Scope**:
The context a Launcher choice and a session attach to: a thread, a channel, a job, or the global default. A thread inherits from its parent channel, and the channel from global.
_Avoid_: context, target

**Plugin shim**:
The Claude Code plugin's slash commands and skills, each of which only calls the `claudeclaw` binary on PATH and holds no logic of its own.
_Avoid_: plugin (when meaning the daemon), extension

**Desktop app**:
The management application for Windows, macOS and Ubuntu that bundles the Daemon and manages a local or remote Daemon. It replaces the former Web UI.
_Avoid_: Web UI, dashboard, GUI

**Management API**:
The single interface through which the Desktop app, the CLI and automation control a Daemon, reached over a local socket or, when enabled, a paired TLS connection.
_Avoid_: Web API, server API, admin endpoint

**Device**:
A Desktop app installation that has been paired with a Daemon and holds its own revocable key for it.
_Avoid_: client, session, user

**API token**:
A named, revocable credential, limited by its Token permissions, that lets automation on another machine call a narrow part of the Management API.
_Avoid_: apiToken (the legacy single setting it replaces), password, key

**Refused run**:
A run the Daemon declines to start because its Launcher is missing, not executable or fails to start; nothing runs and no other Launcher is tried.
_Avoid_: failed run (when nothing ran), fallback

**Parked session**:
The session of a (Scope, Launcher) pair whose Launcher is not the Scope's current one; it is kept and resumed if the Scope switches back.
_Avoid_: old session, archived session

**Attach**:
The Desktop app adopting an already-running Daemon (e.g. one started from the CLI), so the app watches and revives it instead of starting a second one.
_Avoid_: connect (for this meaning), adopt, take over

## Logs

**Daemon log**:
The Worker's and Watchdog's own structured JSONL logs under `daemon/logs/`, recording what the Daemon did by identifiers, counts and outcomes, never secrets or prompt or message content.
_Avoid_: daemon.log (TS's stdout file), system log, trace

**Run log**:
The markdown record of one Claude Code run in `logs/<name>-<iso>.log` (TS layout), which may hold that run's prompt and output but never secrets.
_Avoid_: job log, transcript, output file

**Run index**:
The Rust-only append-only list of finished and Refused runs (`daemon/runs/<YYYY-MM>.jsonl`), one structured record per run linking to its Run log; back-filled from TS run logs.
_Avoid_: run history file, run database, sibling file

## Jobs

**Job**:
A markdown file in a jobs directory whose YAML frontmatter holds a cron `schedule` and options and whose body is the prompt; the Daemon runs it in its own Scope. An agent Job lives under `agents/<agent>/jobs/`.
_Avoid_: cron, task, schedule (when meaning the file)

**One-shot job**:
A Job with `recurring: false`; after it finishes, the Daemon sets `enabled: false` and keeps the file.
_Avoid_: single job, temporary job

**First-start import**:
The ordered, copy-only pass the Daemon runs the first time it starts in a 1.x data dir. It imports Launchers, the legacy `apiToken`, job state and job lines, and records the result. It also runs on demand, and `--dry-run` previews it without writing.
_Avoid_: migration, upgrade, conversion, first-run setup

**Job import**:
The one-time pass over TS data on the Daemon's first start, also runnable on demand. After taking a backup, it rewrites only the Job lines whose meaning differs between the TS and Rust Daemons, into spellings both read identically.
_Avoid_: migration, conversion, upgrade

## Cutover

**Zero**:
The maintainer's own Daemon instance; the reference install whose cutover and Stability window gate switching `main` to Rust.
_Avoid_: prod, the live bot

**Shadow instance**:
A Daemon run against a scrubbed copy of a live data dir with its own test bot, so it never shares writable state, session IDs or the bot token with the live Daemon.
_Avoid_: staging, canary, mirror

**Recorder launcher**:
A Launcher that records the argv it receives and returns a canned or scripted stream-json result instead of running Claude Code; it contains side effects in a Shadow instance and replays fixtures in the Parity suite.
_Avoid_: mock, dry-run mode, fake claude

**Parity suite**:
The offline test suite that feeds the same inputs to the TS daemon and the Rust Daemon and requires the same observable outputs, apart from registered divergences; passing it on the release candidate is cutover gate G1.
_Avoid_: regression tests, golden tests, compat tests

**Divergence registry**:
The list (`parity/divergences.toml`) of every intended difference between TS and Rust behaviour, each citing the closed ticket that decided it; an uncovered mismatch, or an entry no fixture hits, fails the Parity suite.
_Avoid_: allowlist, known failures, exceptions

**Stability window**:
The N consecutive incident-free days of Zero on the Rust Daemon that gate deleting the TS code from `main`.
_Avoid_: burn-in, soak period

**Token permission**:
One named right granted to an API token (e.g. `send`, `jobs:run`, `status:read`, `logs:read`) that limits which Management API calls it may make.
_Avoid_: scope (reserved for Scope), role, grant

**Offline write**:
A CLI change applied as a plain file edit while no Daemon is running, holding `daemon.lock` and recorded in the audit log; while a Daemon runs, the same change goes through the Management API instead.
_Avoid_: direct edit, local mode, maintenance mode

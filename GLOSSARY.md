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
A named, scope-limited, revocable credential that lets automation on another machine call a narrow part of the Management API.
_Avoid_: apiToken (the legacy single setting it replaces), password, key

## Jobs

**Job**:
A markdown file in a jobs directory whose YAML frontmatter holds a cron `schedule` and options and whose body is the prompt; the Daemon runs it in its own Scope. An agent Job lives under `agents/<agent>/jobs/`.
_Avoid_: cron, task, schedule (when meaning the file)

**One-shot job**:
A Job with `recurring: false`; after it finishes, the Daemon sets `enabled: false` and keeps the file.
_Avoid_: single job, temporary job

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
A Launcher script that records the argv it receives and returns a canned stream-json result instead of running Claude Code, used to contain side effects and simulate failures.
_Avoid_: mock, dry-run mode, fake claude

**Stability window**:
The N consecutive incident-free days of Zero on the Rust Daemon that gate deleting the TS code from `main`.
_Avoid_: burn-in, soak period


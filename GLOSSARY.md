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

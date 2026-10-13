# ClaudeClaw

A daemon that runs Claude Code on schedules and in response to chat messages, on behalf of one owner. This glossary covers the Rust hard fork's language.

## Language

**Daemon**:
The long-running `claudeclaw` process that owns the scheduler, the chat adapters and every Claude Code run. It runs headless on its own, with or without the Desktop app.
_Avoid_: server, bot, service

**Launcher**:
The single runnable executable, plus an env overlay, that the Daemon invokes in place of `claude`, receiving the normal `claude` argv. Any wrapping such as `ccs <profile>` lives in a user-owned script that the Launcher points at.
_Avoid_: wrapper, runtime, provider

**Plugin shim**:
The Claude Code plugin's slash commands and skills, each of which only calls the `claudeclaw` binary on PATH and holds no logic of its own.
_Avoid_: plugin (when meaning the daemon), extension

**Desktop app**:
The management application for Windows, macOS and Ubuntu that bundles the Daemon and manages a local or remote Daemon. It replaces the former Web UI.
_Avoid_: Web UI, dashboard, GUI

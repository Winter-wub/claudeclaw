<p align="center">
  <img src="images/claudeclaw-banner.svg" alt="ClaudeClaw Banner" />
</p>
<p align="center">
  <img src="images/claudeclaw-wordmark.png" alt="ClaudeClaw Wordmark" />
</p>

<p align="center">
  <a href="https://github.com/Winter-wub/claudeclaw/commits/main">
    <img src="https://img.shields.io/github/last-commit/Winter-wub/claudeclaw/main?style=flat-square&color=0ea5e9" alt="Last Commit" />
  </a>
  <a href="https://github.com/Winter-wub/claudeclaw/issues/2">
    <img src="https://img.shields.io/badge/Rust%202.0-planning-f59e0b?style=flat-square" alt="Rust 2.0: planning" />
  </a>
  <a href="https://github.com/moazbuilds/ClaudeClaw">
    <img src="https://img.shields.io/badge/forked%20from-moazbuilds%2FClaudeClaw-a855f7?style=flat-square" alt="Forked from moazbuilds/ClaudeClaw" />
  </a>
</p>

<p align="center"><b>A lightweight, open-source OpenClaw version built into your Claude Code.</b></p>

ClaudeClaw turns your Claude Code into a personal assistant that never sleeps. It runs as a background daemon, executing tasks on a schedule, responding to messages on Telegram, Discord, and Slack, transcribing voice commands, and integrating with any service you need.

> Note: Please don't use ClaudeClaw for hacking any bank system or doing any illegal activities. Thank you.

> [!IMPORTANT]
> **This is a hard fork** of [moazbuilds/ClaudeClaw](https://github.com/moazbuilds/ClaudeClaw), maintained independently; changes are not sent upstream.
> - **1.x (TypeScript, this branch)** is what runs today. It is **frozen to bug fixes**: no new features land in TS.
> - **2.0 (Rust)** is being specified on a public planning map, **[ClaudeClaw in Rust with pluggable launchers](https://github.com/Winter-wub/claudeclaw/issues/2)**. It covers a single-binary daemon with a Watchdog, user-defined Launchers, a desktop app (Windows, macOS, Ubuntu) that replaces the Web UI, and drop-in reading of existing `.claude/claudeclaw/` data. See [Rust 2.0](#rust-20) below.

## Why ClaudeClaw?

| Category | ClaudeClaw | OpenClaw |
| --- | --- | --- |
| Anthropic Will Come After You | No | Yes |
| API Overhead | Directly uses your Claude Code subscription | Nightmare |
| Setup & Installation | ~5 minutes | Nightmare |
| Deployment | Install Claude Code on any device or VPS and run | Nightmare |
| Isolation Model | Folder-based and isolated as needed | Global by default (security nightmare) |
| Reliability | Simple reliable system for agents | Bugs nightmare |
| Feature Scope | Lightweight features you actually use | 600k+ LOC nightmare |
| Security | Average Claude Code usage | Nightmare |
| Cost Efficiency | Efficient usage | Nightmare |
| Memory | Uses Claude internal memory system + `CLAUDE.md` | Nightmare |

## Getting Started in 5 Minutes

```bash
claude plugin marketplace add Winter-wub/claudeclaw
claude plugin install claudeclaw
```
Then open a Claude Code session and run:
```
/claudeclaw:start
```
The setup wizard walks you through model, heartbeat, Telegram, Discord, Slack, and security, then your daemon is live with a web dashboard.

If you previously installed the upstream plugin, remove it first: both use the `/claudeclaw:*` command namespace.

### Contributor Note: Plugin Version Metadata

If you change shipped plugin files under `src/`, `commands/`, `prompts/`, or `.claude-plugin/`, the plugin metadata version may also need to be bumped so Claude Code and marketplace consumers detect the update correctly.

Helpers:

```bash
bun run bump:plugin-version
bun run bump:marketplace-version
```

Docs-only and other non-shipped changes do not require these bumps.

## Upgrading

### Fork changes since upstream 1.0.44

- **Runtime launcher.** Claude Code can run through a [CCS](https://github.com/kaitranntt/ccs) profile, settable per channel/thread from the Web UI or Discord `/config` ([docs/RUNTIME_LAUNCHER.md](docs/RUNTIME_LAUNCHER.md)).
- **Missed cron runs are caught up.** Runs missed while the daemon wasn't ticking still fire, within 6 hours.
- **In-flight guard.** A long job is no longer re-fired every tick while it is still running.
- **Every Discord attachment reaches Claude.** PDFs, JSON, spreadsheets, archives and any other file, plus every image or text file after the first, are saved to `.claude/claudeclaw/inbox/discord/` and passed to Claude as paths. Files over 25 MB are reported instead of downloaded.
- **Tool reactions on Discord.** While Claude works, your message collects one emoji per kind of tool used (📖 read, ✏️ edit, 💻 shell, 🌐 web, 🤖 subagent, 🧩 skill, 🔌 MCP, 🛠️ other). They are cleared when the reply is posted.
- **Fixes:**
  - Session-recovery retries work again: they no longer wrap `claude` twice.
  - A malformed cron schedule is rejected when jobs load instead of crashing the scheduler tick.
  - `/jobs create` writes `recurring: true`, so new jobs no longer run once and vanish from the schedule.
  - `/jobs run` no longer starts a daemon by accident.

### v1.0.26 — Allowlist behavior change (Telegram & Discord)

Prior to this release, an empty `allowedUserIds` list meant **allow everyone**. That was a potential security vulnerability; any Telegram or Discord user could drive the daemon.

**New behavior:** an empty list means **block everyone**. The daemon will refuse to start if a bot token is configured without at least one allowed user ID.

**Migration:** add your user ID(s) to `settings.json` before upgrading:

```json
"telegram": { "allowedUserIds": [123456789] },
"discord":  { "allowedUserIds": ["987654321012345678"] }
```

Run `claudeclaw config` for guided setup if you're unsure of your user ID.

### v1.1.0 — Web UI bearer token gate

All `/api/*` routes (except `/api/health`) now require an `Authorization: Bearer <token>` header. The token is auto-generated on first start and written to `.claude/claudeclaw/web.token`. The daemon also prints the full URL with the token embedded when the web UI starts.

**Migration:** update any scripts that call `/api/state` or other API routes to pass the token:

```
Authorization: Bearer <contents of .claude/claudeclaw/web.token>
```

Existing `/api/inject` users who configured `settings.apiToken` are unaffected; that fallback still works.

### v1.1.0 — Discord text-attachment truncation limit reduced

Text attachments sent to the Discord bot are now truncated at **2,048 bytes** (previously 51,200). Payloads over that limit have `…[truncated]` appended silently; there is no config knob to restore the old limit.

**Migration:** if you rely on passing large text files through Discord attachments, switch to gists or another file-sharing mechanism and paste the URL instead.

---

## Rust 2.0

ClaudeClaw 2.0 is a Rust rewrite, planned in the open before any code is written. Each decision lives in a closed ticket on the **[planning map](https://github.com/Winter-wub/claudeclaw/issues/2)**; open tickets are the questions still being worked. Highlights so far:

- **One binary, supervised.** A Watchdog process restarts the Worker with backoff, gives up loudly on a crash loop, and allows one daemon per project dir.
- **Bring your own launcher.** A launcher is any executable that receives the normal `claude` argv: `claude` itself, a `ccs` script, a router, or your own wrapper. If it can't run, the run is refused, never silently switched to another launcher.
- **Desktop app.** Tauri 2 on Windows, macOS and Ubuntu, with the daemon bundled. It manages local and paired remote daemons through one Management API, and replaces the Web UI.
- **Drop-in.** The first start reads today's `.claude/claudeclaw/` data, copies rather than moves it, and previews everything with `claudeclaw import --dry-run`.
- **Proven parity.** A differential test suite runs the TS daemon to generate expected behaviour, and every intended difference is registered.

Further reading:

- [GLOSSARY.md](GLOSSARY.md): the project's vocabulary
- [Architecture spec](https://github.com/Winter-wub/claudeclaw/blob/spec/architecture/docs/spec/architecture.md)
- Research: [TS daemon contract](https://github.com/Winter-wub/claudeclaw/blob/research/ts-daemon-contract/docs/research/ts-daemon-contract.md), [Rust crates](https://github.com/Winter-wub/claudeclaw/blob/research/rust-crates/docs/research/rust-crates.md), [Claude Code launchers](https://github.com/Winter-wub/claudeclaw/blob/research/claude-code-launchers/docs/research/claude-code-launchers.md)

## Features

### Automation
- **Heartbeat:** Periodic check-ins with configurable intervals, quiet hours, and editable prompts.
- **Cron Jobs:** Timezone-aware schedules for repeating or one-time tasks with reliable execution.

### Communication
- **Telegram:** Text, image, and voice support.
- **Discord:** DMs, server mentions/replies, slash commands, voice messages, attachments of any type, and live tool-use reactions.
- **Slack:** Socket Mode bot — DMs, channel mentions, threads, voice messages, and file attachments. Configure `SLACK_BOT_TOKEN` and `SLACK_APP_TOKEN` in your environment or `settings.json`.
- **Time Awareness:** Message time prefixes help the agent understand delays and daily patterns.

### Multi-Session Threads (Discord)
- **Independent Thread Sessions:** Each Discord thread gets its own Claude CLI session, fully isolated from the main channel.
- **Parallel Processing:** Thread conversations run concurrently — messages in different threads don't block each other.
- **Auto-Create:** First message in a new thread automatically bootstraps a fresh session. No setup needed.
- **Session Cleanup:** Thread sessions are automatically cleaned up when threads are deleted or archived.
- **Backward Compatible:** DMs and main channel messages continue using the global session.

See [docs/MULTI_SESSION.md](docs/MULTI_SESSION.md) for technical details.

### Reliability and Control
- **GLM Fallback:** Automatically continue with GLM models if your primary limit is reached.
- **Runtime Launcher:** Route Claude Code through a [CCS](https://github.com/kaitranntt/ccs) profile instead of your Anthropic quota, or switch back to the direct `claude` binary, globally or per channel/thread. Switch from the Web UI or Discord `/config` without restarting the daemon.
- **Web Dashboard:** Manage jobs, monitor runs, and inspect logs in real time.
- **Security Levels:** Four access levels from read-only to full system access.
- **Model Selection:** Switch models based on your workload.

See [docs/RUNTIME_LAUNCHER.md](docs/RUNTIME_LAUNCHER.md) for the runtime launcher's configuration and technical details.

## FAQ

<details open>
  <summary><strong>Can ClaudeClaw do &lt;something&gt;?</strong></summary>
  <p>
    If Claude Code can do it, ClaudeClaw can do it too. ClaudeClaw adds cron jobs,
    heartbeats, and Telegram/Discord/Slack bridges on top. You can also give your ClaudeClaw new
    skills and teach it custom workflows.
  </p>
</details>

<details open>
  <summary><strong>Is this project breaking Anthropic ToS?</strong></summary>
  <p>
    No. ClaudeClaw is local usage inside the Claude Code ecosystem. It wraps Claude Code
    directly and does not require third-party OAuth outside that flow.
    If you build your own scripts to do the same thing, it would be the same.
  </p>
</details>

<details open>
  <summary><strong>Will Anthropic sue you for building ClaudeClaw?</strong></summary>
  <p>
    I hope not.
  </p>
</details>

<details open>
  <summary><strong>Are you ready to change this project name?</strong></summary>
  <p>
    If it bothers Anthropic, I might rename it to OpenClawd. Not sure yet.
  </p>
</details>

## Screenshots

### Claude Code Folder-Based Status Bar
![Claude Code folder-based status bar](images/bar.png)

### Cool UI to Manage and Check Your ClaudeClaw
![Cool UI to manage and check your ClaudeClaw](images/dashboard.png)

## Contributors

ClaudeClaw was created by [@moazbuilds](https://github.com/moazbuilds) and its contributors. This fork builds on their work.

<a href="https://github.com/moazbuilds/claudeclaw/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=moazbuilds/claudeclaw" />
</a>

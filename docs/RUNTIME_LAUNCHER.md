# Runtime Launcher

Technical documentation for ClaudeClaw's configurable runtime launcher.

## Overview

Every Claude Code invocation the daemon makes — heartbeat, cron jobs, chat, Discord/Telegram/Slack
replies, session compaction — goes through one helper that decides *how* to start Claude Code. By
default it runs the `claude` binary directly. It can instead route through
[CCS](https://github.com/kaitranntt/ccs), which launches Claude Code against an alternative
provider, so the daemon stops consuming your Anthropic quota.

The launcher is a daemon-wide setting, not per-message: one profile is active at a time.

## Architecture

```
call site (runner.ts, rotation.ts, discord.ts)
  │  builds the full argument vector, --model included
  ▼
buildClaudeCommand(baseArgs)        ← runtime-launcher.ts
  │
  ├─ kind: "direct" ──→ [CLAUDE_EXECUTABLE, ...baseArgs]
  └─ kind: "ccs" ─────→ ["ccs", <profile>, ...baseArgs with -p rewritten to --print]
```

The executable can only be chosen once the arguments are final, so each call site builds its
complete argv first and wraps it at the end. `--model` in particular is appended by the caller
rather than inside the run helpers.

`CLAUDE_EXECUTABLE` lives in `src/executable.ts` rather than `src/runner.ts` to break the
`runner → runtime-launcher → runner` import cycle that would otherwise form.

## Configuration

`.claude/claudeclaw/settings.json`:

```json
{
  "runtime": {
    "launcher": {
      "kind": "ccs",
      "profile": "openrouter",
      "directFallback": true
    }
  }
}
```

| Field | Values | Meaning |
| --- | --- | --- |
| `kind` | `"direct"` \| `"ccs"` | Run `claude` directly, or wrap with `ccs <profile>`. Defaults to `"direct"`. |
| `profile` | string | CCS profile name. Required when `kind` is `"ccs"`; preserved when you toggle to `"direct"` so switching back keeps your selection. |
| `directFallback` | boolean | Parsed and preserved, but **not currently acted on** — `buildClaudeCommand` does not yet fall back to direct `claude` when a CCS launch fails. |

Omitting the `runtime` block entirely is equivalent to `kind: "direct"`, which is the pre-existing
behavior.

## Web UI

Settings → **⚙️ Runtime** shows the active launcher, a profile dropdown, and a `CCS`/`Direct`
toggle. Picking a profile switches to it; the toggle flips between CCS and the direct binary and
disables the dropdown while direct.

Profiles are discovered by reading `~/.ccs/config.yaml` with `Bun.YAML.parse`, grouped the same way
the `ccs` CLI reports them:

| Group | YAML source | Examples |
| --- | --- | --- |
| CLIProxy | `cliproxy.providers[]` | `codex`, `agy`, `gemini`, `qwen` |
| API | `profiles` keys | `openrouter`, `studio` |
| Account | `accounts` keys | `personal` |

No `ccs` subcommand emits a machine-readable profile list, hence the direct file read. If the file
is missing or unparseable the endpoint reports `profilesAvailable: false` and the dropdown degrades
to a free-text input, so the control still works on machines without CCS configured.

### Endpoints

- `GET /api/settings/runtime` → `{ launcher, profiles, profilesAvailable }`
- `POST /api/settings/runtime` with `{ kind, profile? }`

A `POST` rejects `kind: "ccs"` with an empty profile, and rejects a profile name absent from the
discovered list (only when discovery succeeded, so hand-configured profiles still work). Rejected
requests leave `settings.json` untouched.

On success the daemon **resets the session**. An existing session resumes under whatever runtime
created it, so keeping it would `--resume` a session across providers.

## Two launcher quirks

Both are handled in code and worth knowing about, because each fails silently.

### `-p` is rewritten to `--print` for CCS

`ccs` inspects its arguments and, for **API-type profiles** (those backed by a settings file —
`openrouter`, `studio`), routes any invocation containing `-p`/`--prompt` through its *delegation*
formatter. That formatter decorates stdout with a summary box:

```
╭─────────────────────────────────────────────╮
│[i] Delegated to <MODEL> (ccs:openrouter)    │
╰─────────────────────────────────────────────╯
```

which is not valid `stream-json`. The daemon parses nothing, the Web UI chat comes back blank, and
the run still exits 0 so the log shows a clean `Done: chat` with no error.

The gate matches `-p`, `--prompt` and `--prompt=` but **not `--print`**, Claude Code's equivalent
long form, so `buildClaudeCommand` rewrites the first `-p` to `--print`. Only the first occurrence
is rewritten — a later `-p` is prompt text, not the flag. CLIProxy-type profiles were never
affected; they exec `claude` directly and pass stdout through unchanged.

### A launcher change must re-read settings from disk

`buildClaudeCommand` resolves the launcher through `getSettings()`, the module-level cache in
`config.ts` — not the daemon's own `currentSettings` object. Mutating `currentSettings` therefore
does not change what gets spawned. The Web UI callback calls `reloadSettings()` so the file stays
the single source of truth and the new launcher applies to the very next spawn.

Without that callback the change would still take effect, but only on the next 30-second
hot-reload tick.

## Files

| Path | Role |
| --- | --- |
| `src/runtime-launcher.ts` | `buildClaudeCommand()`, `getRuntimeLauncher()` |
| `src/executable.ts` | `CLAUDE_EXECUTABLE`, split out to break an import cycle |
| `src/config.ts` | `RuntimeConfig` / `RuntimeLauncherConfig` types and parsing |
| `src/ui/services/ccs-profiles.ts` | Profile discovery from `~/.ccs/config.yaml` |
| `src/ui/services/settings.ts` | `readRuntimeSettings()` / `updateRuntimeSettings()` |
| `src/ui/server.ts` | `/api/settings/runtime` routes |
| `src/commands/start.ts` | `onRuntimeSettingsChanged` — reload + session reset |

## Limitations

- One launcher for the whole daemon; there is no per-channel or per-job override.
- `directFallback` is inert (see the table above).
- Switching profiles discards the current session by design.
- Provider errors surface as ordinary reply text — a `429` from an exhausted CCS profile arrives as
  a chat message, not a daemon-level failure.

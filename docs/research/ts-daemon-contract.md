# TS daemon contract inventory (on-disk + Claude Code)

Research for issue #3. Facts only, read from `main` at `c30c87c`. Every claim cites `file:line`
in this repo. "cwd" means the daemon's working directory (the project root it was started in);
all `.claude/claudeclaw/...` paths are relative to it. Placeholders: `<channelId>`, `<threadId>`,
`<job>`, `<agent>`, `<uuid>`, `<ts>`.

## Summary

- **State is a handful of JSON files plus markdown jobs**, all under `<cwd>/.claude/claudeclaw/`:
  `settings.json` (config, hot-reloaded every 30 s), `session.json` (global session),
  `sessions.json` (`threads` map keyed by Discord channel/thread ID **and by cron job name**),
  `state.json` (statusline/UI snapshot), `jobs/*.md`, `logs/<name>-<iso>.log` (one per run, also the
  catch-up seed), `daemon.pid`, `web.token`, `active-runs`, `permission-mode.json`,
  `pending-resume.json`, `inbox/discord/`, `fallback-sessions/`, `session_<N>.backup`.
- **Every Claude call is `claude -p <prompt> ...`**, optionally wrapped as `ccs <profile> --print ...`.
  Main path: `--output-format stream-json --verbose`, security args, `--resume <id>` when a session
  exists, `--append-system-prompt <ClaudeClaw preamble + whole CLAUDE.md + scope text>` on every
  call, `--model` unless empty/`glm`. Only `result.result`, `session_id`, and (for live callbacks)
  `assistant`/`user` content blocks are consumed.
- **Concurrency**: one serial queue for the global session; one serial queue per `threadId`
  (Discord channel/thread, cron job name, `agent:<agent>`); queues run in parallel.
- **Scheduler**: 60 s `setInterval` (not minute-aligned), cron matcher that ANDs all five fields,
  6 h catch-up window seeded from log filenames, in-memory in-flight guard and retry state.
- **Discord**: raw gateway v10 + REST, 👀 reaction while working, per-tool-category reactions,
  `[react:x]` directive, attachments to `inbox/discord/`, image paths in replies uploaded as files,
  guild channels get isolated sessions, threads also get their own cwd under `agents/`.
- **Web UI**: Bun HTTP server, bearer token from `web.token`, ~17 JSON routes + one SSE chat route.
- Several TS bugs a port must decide whether to replicate: see the
  [Compat traps](#compat-traps--oddities) section at the end.

## On-disk layout

```
<cwd>/
├── CLAUDE.md                         created once if absent (managed block); read on every call
├── .claude/
│   ├── settings.json                 Claude Code project settings: daemon sets/removes statusLine; preflight adds enabledPlugins
│   ├── statusline.cjs                written at start, deleted at stop
│   ├── CLAUDE.md                     legacy; read once to seed root CLAUDE.md
│   ├── skills/<name>/SKILL.md        read for Discord "/name" skill routing
│   └── claudeclaw/
│       ├── settings.json             config (schema below)
│       ├── session.json              global session
│       ├── session_<N>.backup        rotated/cleared/corrupt global sessions (renamed session.json)
│       ├── session_fallback.json     global fallback-model session
│       ├── sessions.json             { threads: { <key>: ThreadSession } }
│       ├── fallback-sessions/<urlencoded key>.json
│       ├── state.json                scheduler snapshot (compact JSON)
│       ├── daemon.pid                "<pid>\n"
│       ├── web.token                 "<token>\n", mode 0600
│       ├── active-runs               plain integer, no newline
│       ├── permission-mode.json      {"mode": "plan"|"acceptEdits"|"bypassPermissions"} (written by Telegram only)
│       ├── pending-resume.json       one-shot wake-up request (written by Claude/agents, consumed by daemon)
│       ├── jobs/<job>.md             cron jobs (dir overridable via settings.jobsDir)
│       ├── logs/<name>-<iso>.log     one file per run; logs/daemon.log is external stdout redirect
│       ├── prompts/HEARTBEAT.md      optional override of the heartbeat template
│       ├── inbox/discord/<attId>-<epochMs><ext>
│       ├── inbox/{telegram,slack}/   phase 2
│       ├── outbox/discord/           default allowed root for image uploads (never created by daemon)
│       ├── outbox/slack/             phase 2
│       └── whisper/{bin,lib,models,tmp}/   phase 2 (voice)
└── agents/<agent>/                   per-agent cwd; jobs/*.md, session.json, session_fallback.json
```

Directories created at boot: `.claude/claudeclaw/`, the jobs dir, `logs/` (`src/config.ts:270-278`).
If `settings.json` is missing it is written from `DEFAULT_SETTINGS` (`src/config.ts:275-277`).

### `settings.json`

Read with `JSON.parse` and normalised by `parseSettings` (`src/config.ts:337-486`). Unknown keys are
ignored on read but **preserved** by every writer, because writers do read-modify-write of the raw
JSON (`src/ui/services/settings.ts:34-60,114-181`). Writers emit `JSON.stringify(data, null, 2) + "\n"`.

| Field | Type | Default when absent (parse) | Read by | Written by | Cite |
|---|---|---|---|---|---|
| `model` | string | `""` (= let Claude Code pick) | execClaude, streamClaude, compact | Discord `/config model` | `config.ts:350`, `settings.ts:174-181` |
| `api` | string | `""`; non-empty → child env `ANTHROPIC_AUTH_TOKEN` | buildChildEnv | — | `config.ts:351`, `runner.ts:326-338` |
| `fallback.model` / `fallback.api` | string | `""` | rate-limit fallback | — | `config.ts:352-355`, `runner.ts:1091-1201` |
| `agentic.enabled` | bool | `false` | execClaude model routing | — | `config.ts:305` |
| `agentic.defaultMode` | string | `"implementation"` | model-router | — | `config.ts:332` |
| `agentic.modes[]` | `{name, model, keywords[], phrases?[]}` | built-in planning(opus)/implementation(sonnet) lists | model-router | — | `config.ts:40-71,287-335` |
| `agentic.planningModel` / `implementationModel` | string | legacy; mapped onto the two default modes | — | — | `config.ts:308-319` |
| `timezone` | string | `""` (file default writes `"UTC"`); accepts `UTC±H[:MM]`/`GMT±..` or IANA, invalid → `""` | offset fallback | — | `timezone.ts:27-41` |
| `timezoneOffsetMinutes` | number\|numeric string | from `timezone`: UTC±/GMT± parse, else IANA **current** offset, else 0; clamped −720..+840 | cron, clock prefix, exclude windows | — | `timezone.ts:43-50,8-11` |
| `heartbeat.enabled` | bool | `false` | scheduler | Web UI | `config.ts:360` |
| `heartbeat.interval` | number (minutes) | `15`; **no validation** on parse; UI clamps 1..1440 | scheduler | Web UI | `config.ts:361`, `settings.ts:42-45` |
| `heartbeat.prompt` | string (literal or path ending `.md/.txt/.prompt`) | `""` | heartbeat | Web UI | `config.ts:362,575-590` |
| `heartbeat.excludeWindows[]` | `{start:"HH:MM", end:"HH:MM", days?:int[0-6]}` | `[]`; bad times dropped; empty days → all 7 | heartbeat | Web UI | `config.ts:495-517` |
| `heartbeat.forwardToTelegram` | bool | parse default **`false`**; `DEFAULT_SETTINGS` writes `true` | heartbeat forwarding (governs Discord too) | — | `config.ts:79,364`, `start.ts:739` |
| `discord.token` | string | `""`; env `DISCORD_TOKEN` overrides | gateway | — | `config.ts:377` |
| `discord.allowedUserIds` | string[] (or bare numbers) | `[]`; extracted from raw text by regex to keep snowflake precision | auth, cron/heartbeat DM targets | — | `config.ts:378-382,528-540` |
| `discord.listenChannels` | string[] | `[]` | guild trigger | — | `config.ts:383-385` |
| `discord.listenGuilds` | string[] | `[]` | guild trigger | — | `config.ts:386-388` |
| `discord.allowedGuilds` | string[] | `[]` | welcome-on-join only | — | `config.ts:389-391` |
| `discord.channelNames` | `{<channelId>: name}` | absent | prompt tag, UI labels | — | `config.ts:392-396` |
| `discord.imageOutputRoots` | string[] (absolute only) | `[]` → `<cwd>/.claude/claudeclaw/outbox/discord` | reply image extraction | — | `config.ts:19,397-399` |
| `discord.streaming` | bool (`=== true`) | `false` | live preview message | — | `config.ts:400` |
| `discord.channelAllowedUserIds` | `{<channelId>: string[]}` | absent | guild-only auth | — | `config.ts:401-407`, `allowlist.ts:10-20` |
| `security.level` | `locked\|strict\|moderate\|unrestricted` | `moderate` (invalid → moderate) | security args, scope prompt | — | `config.ts:341-345` |
| `security.allowedTools` / `disallowedTools` | string[] (not type-checked per item) | `[]` | security args | — | `config.ts:417-425` |
| `web.enabled` / `web.host` / `web.port` | bool / string / number | `false` / `"127.0.0.1"` / `4632` | startup only | — | `config.ts:426-430` |
| `apiToken` | string | absent | `/api/inject` alt bearer | — | `config.ts:455`, `server.ts:82-86` |
| `sessionTimeoutMs` | number > 0 | `1800000` | `/compact` timeout only | — | `config.ts:438-440`, `runner.ts:977,1007` |
| `timeouts.{telegram,heartbeat,job,default}` | minutes > 0 | `5/15/30/5` | per-run timeout, read fresh each spawn | — | `config.ts:441-446`, `runner.ts:355-368` |
| `watchdog.maxConsecutiveTimeouts` | positive int \| null | `null` | watchdog | — | `watchdog.ts:139-159` |
| `watchdog.maxRuntimeSeconds` | positive number \| null | `null` | watchdog | — | same |
| `session.autoRotate` | bool | `false` | global-session rotation | — | `config.ts:449-454` |
| `session.maxMessages` / `maxAgeHours` | number | `50` / `24` | rotation | — | same |
| `session.summaryPath` | string (dir) | `""` = no summaries | rotation, bootstrap | — | same |
| `jobsDir` | string (abs, or relative to cwd) | `.claude/claudeclaw/jobs` | job loader, UI quick-job | — | `config.ts:21-26,456` |
| `runtime.launcher` | `{kind:"direct"\|"ccs", profile?, directFallback}` | absent → `{direct, directFallback:true}`; present-but-partial → `directFallback:false` unless `true` | buildClaudeCommand | Web UI, Discord `/config profile` | `config.ts:457-484` |
| `runtime.launchers` | `{<scopeKey>: launcher}` | absent | per-scope override | Web UI, Discord `/config profile` (guild) | same, `settings.ts:114-163` |
| `telegram.*` | token, allowedUserIds (numbers), listenChats, receiveEnabled, dmIsolation, whisperModel | phase 2 | — | — | `config.ts:366-375` |
| `slack.*`, `stt.*` | phase 2 | — | — | — | `config.ts:409-416,431-437` |
| `plugins` | `{<id>: {enabled, source, config}}` | `{}` | plugin manager (phase 2) | — | `plugins.ts:341-354` |

Env overrides: `DISCORD_TOKEN`, `TELEGRAM_TOKEN`, `SLACK_BOT_TOKEN`, `SLACK_APP_TOKEN`
(`config.ts:367,377,410-411`). Startup refuses to run if `discord.token` is set but
`allowedUserIds` is empty (`start.ts:611-617`).

**Hot reload** (`start.ts:784-840`): every 30 s, `reloadSettings()` + `loadJobs()`. A JSON parse error
is logged and the previous settings stay cached (`config.ts:551-556`). Heartbeat is rescheduled only
if `enabled/interval/prompt/timezone/timezoneOffsetMinutes/excludeWindows` changed. A changed
`discord.token` restarts the gateway; removal stops it (`start.ts:428-444`). Everything read via
`getSettings()` at use time (Discord lists, security, timeouts, model, runtime) applies on the next
message/spawn. **Not** hot-reloaded: `web.*` (bound once), `plugins`. Web UI and `/config` writes call
`reloadSettings()` immediately (`start.ts:560-575`, `discord.ts:1471,1498,1512`).

### Session files

| File | Shape | Writer | Cite |
|---|---|---|---|
| `session.json` | `{sessionId, createdAt, lastUsedAt, turnCount, compactWarned, messageCount?}` (ISO strings) | createSession on first `session_id`; `lastUsedAt` bumped on every `getSession` | `sessions.ts:9-16,62-86` |
| `sessions.json` | `{threads: {<key>: {sessionId, threadId, createdAt, lastUsedAt, turnCount, compactWarned}}}` | createThreadSession; bumped on every get | `sessionManager.ts:7-71` |
| `agents/<agent>/session.json` | same as global | only when `agentName` is set and `threadId` is not | `sessions.ts:24-27` |
| `session_fallback.json`, `agents/<agent>/session_fallback.json`, `fallback-sessions/<encodeURIComponent(key)>.json` | same as global | rate-limit fallback path | `sessions.ts:132-194` |
| `session_<N>.backup` | renamed `session.json`; N = max existing + 1 | rotation, `--clear`, corrupt/stale recovery | `sessions.ts:196-218` |

A record without a non-empty string `sessionId` is treated as absent (`sessionValidate.ts:2-6`).
All are pretty-printed (`null, 2`) plus `"\n"`. `sessions.json` keys observed in practice: Discord
channel/thread snowflakes, cron job names, `agent:<agent>`, and phase-2 `tg:...`/`slk:...` keys
(`telegram.ts:981-994`, `slack.ts:636-638`). Nothing ever prunes job-name keys.

### `state.json`

Written by `updateState()` every 60 s tick, at boot and on UI changes, as **compact** JSON + `"\n"`
(`statusline.ts:27-32`, `start.ts:843-870`).

| Field | Type | Notes |
|---|---|---|
| `heartbeat` | `{nextAt: epochMs}` \| absent | absent when disabled; `nextAt` is display-only (see Scheduler) |
| `jobs[]` | `{name, nextAt, lastResult?, lastRanAt?, failCount?, retryAt?}` | `nextAt` from `nextCronMatch` (48 h search cap); `lastResult` ∈ `ok\|error\|skipped`; retry fields only while retrying |
| `security` | string | level |
| `telegram`, `discord` | bool | token configured |
| `startedAt` | epochMs | |
| `web` | `{enabled, host, port}` | actual bound port |

Read by the statusline script and Web UI `/api/technical-info`. Deleted by `--stop` (`stop.ts:47-51`).
`state.json` is **not** a source of truth for anything on boot (`start.ts:890-897`).

### Other files under `.claude/claudeclaw/`

| File | Format | Behaviour | Cite |
|---|---|---|---|
| `daemon.pid` | `"<pid>\n"` | written at daemon start; a live PID aborts a second start unless `--replace-existing` (SIGTERM, wait 4 s); stale file removed; removed on SIGTERM/SIGINT | `pid.ts:15-49`, `start.ts:310-336,358-369` |
| `web.token` | base64url(32 random bytes) + `"\n"`, mode 0600 | created on first web start, reused after | `ui/auth.ts:8-16` |
| `active-runs` | integer string | count of in-flight `execClaude` calls; rewritten at start/end of each | `runner.ts:254-266,1042-1044,1442-1445` |
| `permission-mode.json` | `{"mode": ...}` pretty | read once and cached; absent/invalid → `bypassPermissions`; only Telegram writes it | `runner.ts:840-864`, `telegram.ts:1266` |
| `pending-resume.json` | `{transport:"discord"\|"telegram", channelId, threadId?, sessionKey?, agentName?, wakeUpPrompt, expires?: epochMs}` | see Sessions & resume | `pending-resume.ts:11-35` |
| `logs/<name>-<iso>.log` | markdown run record | `<iso>` = `toISOString()` with `:` and `.` → `-`; one per `execClaude`; never rotated | `runner.ts:1065-1066,1346-1360` |
| `logs/daemon.log` | stdout/stderr | written by the launcher (`nohup ... > logs/daemon.log`), **not** by the daemon; read by `/api/logs` | `commands/start.md:116`, `ui/services/logs.ts:6` |
| `inbox/discord/*` | downloaded attachments | never cleaned up | `discord.ts:670-690` |

Run-log body (`runner.ts:1346-1358`): `# <name>`, `Date: <iso>`, `Session: <id|unknown> (new|resumed)`,
`Model config: primary|fallback`, optional `Task type:`/`Routing:`, `Prompt: <prompt>`,
`Exit code: <n>`, blank, `## Output`, stdout, optional `## Stderr` + stderr. Agent jobs are named
`<agent>/<label>`, so their logs land in a subdirectory `logs/<agent>/` (Bun.write creates it).

## Job files & cron semantics

### Loading (`jobs.ts:102-147`)

- Flat jobs: every `*.md` in the jobs dir; `name` = file stem.
- Agent jobs: every `agents/<agent>/jobs/*.md`; `name` = `<agent>/<stem>`, `agent` and `label` forced
  from the path (frontmatter `agent:`/`label:` are overwritten).
- Files whose parse returns null, or with `enabled` false, are skipped silently.

### Frontmatter parser (`jobs.ts:28-100`) — line matching, not YAML

| Aspect | Exact behaviour |
|---|---|
| Envelope | Regex `^---\s*\n([\s\S]*?)\n---\s*\n([\s\S]*)$`. File must start with `---` at byte 0 (a BOM fails). Closing `---` must be followed by a newline. `\r\n` tolerated (lines are trimmed). No match → `console.error` + skipped. |
| Line handling | Frontmatter split on `\n`, each line `trim()`med. For each key the **first** line that `startsWith("<key>:")` wins. Case-sensitive; `key :` (space before colon) is not recognised. |
| Value | Text after the first `<key>:`, trimmed, then `replace(/^["']\|["']$/g,"")`: strips **one** leading and **one** trailing quote independently (mismatched quotes accepted, inner quotes kept). No comment stripping, no escapes, no multi-line/block scalars, no lists. |
| Unknown keys / non `k: v` lines | Ignored, but preserved on rewrite. |
| Body | Everything after the closing `---\n`, `trim()`med = prompt. At run time `resolvePrompt` reads it as a file if the whole trimmed body ends in `.md`/`.txt`/`.prompt` (relative to cwd; missing file → literal) (`config.ts:568-590`). |

| Key | Accepted | Result |
|---|---|---|
| `schedule` | any string | **Required**; missing → job ignored. Not validated at load. |
| `recurring` | `true\|yes\|1` (case-insensitive) | `true`; anything else, **including absent**, → `false` (one-shot). |
| `daily` | legacy alias of `recurring` | consulted only when `recurring:` absent |
| `notify` | `false\|no` → false; `error` → errors only; anything else/absent → true | |
| `model` | string; empty → none | overrides global/agentic model for this job |
| `timeout` | `parseInt` seconds > 0 (`"90s"` → 90) | per-job timeout; else `timeouts.job` |
| `agent`, `label` | string | only meaningful for flat files; agent jobs get them from the path |
| `enabled` | `false\|no\|0` → disabled | anything else → enabled |
| `retry` | `parseInt` (0/NaN → none) | max retries after a failure |
| `retry_delay` | `parseInt` seconds (0/NaN → default 300) | |

Daemon rewrites of job files:

- **Frontmatter guard** (`jobs.ts:164-204`, `start.ts:919,935`): before a run the frontmatter text is
  snapshotted; afterwards, if the frontmatter differs (trimmed compare) or the envelope is gone, the
  file is rewritten as `---\n<original fm>\n---\n<current body trimmed>\n` (or fully restored).
- **One-shot clearing** (`jobs.ts:206-221`, `start.ts:962-972`): after a non-recurring job finishes and
  no retry is pending, every frontmatter line starting `schedule:` is deleted and the file rewritten
  `---\n<fm>\n---\n<body>\n`. The file stays; it is ignored from then on (no schedule).
- Web UI quick job writes `---\nschedule: "<m> <h> * * *"\nrecurring: true|false\n---\n<prompt>\n` to
  `<jobsDir>/quick-<YYYYMMDDHHMMSS>-<HHMM>.md` (`ui/services/jobs.ts:12-44`).

### Cron matcher (`cron.ts:3-42`), verified empirically against `bun`

| Aspect | TS semantics |
|---|---|
| Fields | `expr.trim().split(/\s+/)` → minute, hour, day-of-month, month, day-of-week. Extra fields ignored. **Fewer than 5 → `TypeError` thrown** (`field.split` on undefined). |
| Combination | **All five fields ANDed**, including DOM and DOW (standard cron ORs them when both are restricted). `0 9 13 * 1` fires only on a Monday the 13th. |
| `*` | matches any value (`value % 1 === 0`). With `*` in DOM or DOW that field is simply always true, so the AND reduces to the other field, same as standard cron in that case. |
| Lists | `,` separated parts, any part matching wins. |
| Ranges | `a-b` inclusive; `a-b/n` matches `a, a+n, ...`; reversed range never matches. |
| Steps on `*` | `*/n` matches `value % n === 0`, **not** "from the field minimum": DOM `*/2` = even days (2,4,..), month `*/3` = 3,6,9,12 (standard: odd days; 1,4,7,10). Minute/hour/DOW behave as standard. |
| Steps on a single value | `5/15` ignores the step: matches only 5. |
| Not supported | names (`MON`, `JAN`), `7` as Sunday, `?`, `L`, `W`, `#`, `@daily`-style macros. They never match (parseInt → NaN) and raise no error. |
| DOW numbering | 0 = Sunday .. 6 = Saturday. |
| Timezone | Date shifted by `timezoneOffsetMinutes` (fixed offset, clamped −720..840), then UTC getters used (`timezone.ts:52-54`). IANA zones are converted to their offset **at settings parse time**; reparsed every 30 s, so DST is picked up within 30 s, but `prevCronMatch` over 6 h uses the current offset for the whole window. |
| Seconds | `nextCronMatch`/`prevCronMatch` zero seconds; `cronMatches` ignores seconds entirely. |

`nextCronMatch` (`cron.ts:44-53`) scans minute-by-minute up to 2880 minutes (48 h) and, if nothing
matches, returns `after + 2881 min` anyway, so `state.json.jobs[].nextAt` is wrong for schedules
rarer than every 48 h. `prevCronMatch` (`cron.ts:64-77`) scans back from `before` (seconds zeroed,
inclusive) up to `withinMinutes` (360).

## Sessions & resume

### Session key per call site

| Caller | `name` (log + timeout category) | `threadId` (session key / queue) | `agentName` (cwd) | Session store |
|---|---|---|---|---|
| Bootstrap at start (no `--trigger`) | `bootstrap` | — | — | `session.json` (only if absent) (`runner.ts:1809-1821`) |
| `--trigger` | `trigger` | — | — | global (`start.ts:762-771`) |
| Heartbeat | `heartbeat` | — | — | global (`start.ts:733`) |
| Cron job (flat) | `<job>` (category `job`) | `<job>` | — | `sessions.json["<job>"]` (`start.ts:924-932`) |
| Cron job (agent) | `<agent>/<label>` (category `job`) | `agent:<agent>` | `<agent>` | `sessions.json["agent:<agent>"]`, shared by all that agent's jobs |
| Discord DM | `discord` | — | — | global |
| Discord guild channel or thread | `discord` | `<channelId>` | thread: `<slug>-<threadId>`; plain channel: none | `sessions.json["<channelId>"]` (`discord.ts:1304,1321-1338`) |
| Discord guild-join welcome | `discord` | — | — | global (`discord.ts:1696`) |
| Discord pending resume | `discord` | `sessionKey` | `agentName` | per file (`discord.ts:1786-1801`) |
| Web UI chat (`/api/chat`) | `chat` | — (streamClaude) | — | global (`start.ts:588`, `runner.ts:1463-1675`) |
| `/api/inject`, `claudeclaw send`, one-shot `--prompt` | `inject` / `send` / `prompt` | — | — | global |

`execClaude` uses `getThreadSession(threadId)` when `threadId` is set, else `getSession(agentName)`
(`runner.ts:1059-1061`); so an agent job uses the thread store, not `agents/<agent>/session.json`.

### Queueing (`runner.ts:249-278`)

`run()` enqueues on `threadQueues[threadId]` if set, else the single global queue. Same key =
strictly serial (each task waits for the previous to settle, success or failure). Different keys
and the global queue run concurrently. Discord DMs, web chat, heartbeat, inject, send and the
guild-join welcome all share the global queue. `bootstrap()` calls `execClaude` directly, bypassing
the queue.

### Session lifecycle in `execClaude` (`runner.ts:1026-1446`)

1. If global (no thread, no agent) and `session.autoRotate` and `needsRotation` → `rotateSession`
   (see below) (`:1047-1057`).
2. Look up existing session. New = no `sessionId`.
3. Spawn (argv below). New sessions are persisted from the stream's `session_id` **regardless of
   exit code** (a timed-out first run is still resumable) unless the output was a rate-limit
   message (`:1312-1331`).
4. **Thinking-signature corruption** (`/Invalid.*signature.*thinking block/i` on a non-zero resumed
   primary run): drop the session (thread: remove; agent: reset; global: backup) and rerun without
   `--resume`, persist the new ID (`:1211-1245`).
5. **Stale session** (`/No conversation found with session ID/i` on non-zero resumed run, in stdout or
   stderr): drop as above (or the fallback session if fallback was used), rerun without `--resume`,
   persist new ID (`:1254-1331`).
6. **Rate limit** (`/you(?:'|')ve hit your limit|out of extra usage/i` in result text or stderr):
   global flag until the parsed reset time (`resets 3pm (UTC)`-style, UTC; else +1 h); stdout
   replaced by the limit message (`:1292-1302`, `:200-243`). If `fallback` is configured and differs,
   the call is first retried with the fallback model/api against its own fallback session
   (`:1160-1201`).
7. If exit ≠ 0, no result text, and stderr non-empty → stdout = stderr (`:1305-1307`).
8. Write the run log; increment `messageCount` (global only) (`:1360-1362`).
9. Watchdog (opt-in, in-memory): consecutive exit-124 count and wall-clock since first seen; on
   breach logs and returns without compacting (`:1368-1383`, `watchdog.ts`).
10. **Auto-compact on timeout**: exit 124 on a resumed session → `claude -p /compact --resume <id>`
    then one retry of the same argv; turn incremented on success (`:1386-1423`).
11. On exit 0 for a resumed session: `turnCount++`; at ≥ 25 turns emit a one-time `warn` compact
    event and set `compactWarned` (`:1426-1439`). No code subscribes to compact events outside
    `runner.ts`, so nothing is shown to users.

### Rotation (`rotation.ts`)

Global session only, when `session.autoRotate`. `needsRotation`: `messageCount >= maxMessages` or
`now - createdAt >= maxAgeHours`. `rotateSession`: if `summaryPath` set, spawn
`claude -p <prompts/SUMMARY.md> --resume <id> --output-format text` (global launcher, cleaned env,
**no** security args/model/api env, cwd = daemon cwd, 60 s kill) and write stdout to
`<summaryPath>/YYYY-MM-DD-HH-mm.md` (local time); then `session.json` → `session_<N>.backup`. The
summary text is injected into the next call's `--append-system-prompt` as
`Context from the previous session:\n\n<summary>`. `bootstrap()` injects the newest `*.md` (by
filename sort) from `summaryPath` into its prompt (`runner.ts:1814-1819`).

### Pending resume (`pending-resume.ts:47-84`, `discord.ts:1786-1801`)

On every Discord `READY` (first time only per process, module flag `consumed`): if the file exists
and parses and `transport === "discord"`, it is renamed to `pending-resume.json.consumed`, then
deleted, **before** running (a crash loses the wake-up rather than doubling it). Expired (`expires`
< now) or missing `wakeUpPrompt/transport/channelId` → discarded. Otherwise
`runUserMessage("discord", wakeUpPrompt, sessionKey, agentName)` and the non-empty stdout is posted to
`threadId ?? channelId`. A file for another transport is left untouched.

### Other session commands

`--clear`: backup global session, stop daemon (`commands/clear.ts`). Discord `/reset`: guild →
remove `sessions.json[<channelId>]` and its fallback file; DM → delete `session.json` and global
fallback (`discord.ts:1398-1411`). Launcher change (UI or `/config profile`) resets only that scope's
session (`server.ts:224-227`, `discord.ts:1484-1487`). `THREAD_DELETE` and archive via
`THREAD_UPDATE` remove the thread's session (`discord.ts:1882-1904`).

## Claude Code invocation

### Launcher wrapping (`runtime-launcher.ts:18-49`)

`getRuntimeLauncher(scopeChain)`: first `settings.runtime.launchers[key]` hit in the chain, else
`runtime.launcher`, else direct. `buildClaudeCommand(baseArgs, chain)`:

- direct → `["claude", ...baseArgs]` (`claude` resolved via `PATH`; Windows resolves `claude.exe`, `executable.ts`).
- ccs → `["ccs", <profile>, ...baseArgs]` with the **first** `-p` replaced by `--print`; throws if no profile.

Scope chains: Discord guild runs `[channelId, parentChannelId?]`; jobs `[<job>]` or `["agent:<agent>"]`;
everything without a `threadId` uses the global launcher (`runner.ts:1041`, `discord.ts:1337`).

### Security args (`runner.ts:866-895`)

| Source | Args |
|---|---|
| permission mode `bypassPermissions` (default) | `--dangerously-skip-permissions` |
| `plan` / `acceptEdits` | `--permission-mode <mode>` |
| level `locked` | `--tools Read,Grep,Glob` |
| level `strict` | `--disallowedTools Bash,WebSearch,WebFetch` |
| `moderate` / `unrestricted` | none (moderate relies on the scope prompt) |
| `allowedTools` non-empty | `--allowedTools "<items joined by space>"` (single argv element) |
| `disallowedTools` non-empty | `--disallowedTools "<items joined by space>"` (can appear twice with strict) |

### Per call site argv

Order is exactly as listed; `[...]` = conditional. `SEC` = security args. `ASP` = append-system-prompt text.

| Call site | argv after the executable | Output | cwd | Timeout | Cite |
|---|---|---|---|---|---|
| execClaude (all `run`/`runUserMessage`) | `-p <prompt> --output-format stream-json --verbose SEC [--resume <id>] --append-system-prompt <ASP> [--model <m>]` | stream-json | agent dir if `agentName`, else inherited | see Timeouts | `runner.ts:1112-1152` |
| fallback retry | same with fallback session/model | stream-json | same | same | `:1165-1174` |
| `/compact` (manual + auto) | `-p /compact --output-format text --resume <id> SEC [--model <m>]` | text (exit code only) | agent dir or inherited | manual: `sessionTimeoutMs`; auto: run timeout | `:939-964` |
| Web chat (streamClaude) | `-p <prompt> --output-format stream-json --verbose SEC [--resume <id>] --append-system-prompt <ASP> [--model settings.model]` | stream-json | inherited | **none** | `:1494-1532` |
| Rotation summary | `-p <SUMMARY.md> --resume <id> --output-format text` | text | inherited | 60 s | `rotation.ts:63-73` |
| Thread-intent classifier (every guild message < 200 chars) | `--print --output-format text`, prompt on **stdin** | text | inherited | 60 s | `discord.ts:611-619` |
| Fork (Telegram only) | `-p <prompt> --output-format json SEC --model claude-haiku-4-5-20251001 --append-system-prompt <fork prompt>` | json `.result` | inherited | 120 s | `runner.ts:1747-1803` |
| Plugin CLI (phase 2) | `claude plugin ...` (not wrapped by launcher) | | | | `plugin-cli.ts:35-53` |

`--model` is omitted when the model is empty or equals `glm` (case-insensitive); `glm` instead sets
env `ANTHROPIC_BASE_URL=https://api.z.ai/api/anthropic`, `API_TIMEOUT_MS=3000000` (`runner.ts:332-335`).
Model choice in execClaude: job/caller override → agentic routing (if enabled) → `settings.model`
(`:1076-1089`). Agentic routing (`model-router.ts:19-100`): lowercase substring match; any mode
phrase → that mode; else count keyword hits per mode, `+0.5` per `?` for modes that have phrases;
highest wins; tie → `defaultMode` among tied, else first; zero hits → `defaultMode` (or first mode).
Web chat, compact, summary and classifier ignore agentic routing.

### `--append-system-prompt` (ASP) content (`runner.ts:1123-1148`)

Joined with `"\n\n"`, passed on every call (it does not persist across `--resume`):

1. `You are running inside ClaudeClaw.`
2. `Context from the previous session:\n\n<summary>` (only right after a rotation)
3. Full trimmed text of `<cwd>/CLAUDE.md` (daemon cwd, also for agent cwds)
4. Plugin `before_prompt_build` context (phase 2)
5. Unless `unrestricted`: four-line `CRITICAL SECURITY CONSTRAINT: You are scoped to the project directory: <cwd> ...` (`:789-794`)
6. Fixed sentence about `<untrusted-...>` tags being data, not instructions.

### Prompt shaping

- Clock prefix `[YYYY-MM-DD HH:MM:SS UTC±H[:MM]]\n` prepended for `runUserMessage`/`streamUserMessage`
  (Discord, web chat, inject, send, pending resume), heartbeat and jobs (`timezone.ts:67-76`,
  `runner.ts:1677-1686`). Not for bootstrap, trigger, welcome.
- Untrusted text is wrapped `<untrusted-<label>-<8 random [a-z0-9]>>\n...\n</...>`, truncated to
  `maxLen` (8000 default) + `\n[truncated]`, with any `</?untrusted-<label>-...>` inside replaced by
  `[redacted-tag]` (`prompt-safety.ts:1-13`).
- Heartbeat prompt: template (`.claude/claudeclaw/prompts/HEARTBEAT.md` override, else install
  `prompts/heartbeat/HEARTBEAT.md`) + `\n\nUser custom heartbeat prompt:\n<heartbeat.prompt>`; both
  empty → no run (`start.ts:720-734`, `runner.ts:923-936`).

### Child env (`runner.ts:107-119,326-338`)

`process.env` minus `CLAUDECODE`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST`;
plus `ANTHROPIC_AUTH_TOKEN=<api>` when the active config's `api` is non-empty; plus the `glm` vars.
Exception: the intent classifier passes the **unfiltered** `process.env` plus `HOME`
(`discord.ts:615`). stdin is not piped except for the classifier.

### Timeouts and kill

| Category | Source | Default |
|---|---|---|
| `heartbeat` | `timeouts.heartbeat` | 15 min |
| job | `timeout:` frontmatter (s) else `timeouts.job` | 30 min |
| `telegram` | `timeouts.telegram` | 5 min |
| everything else via execClaude: `discord`, `inject`, `send`, `prompt`, `trigger`, `bootstrap` | `timeouts.default` | 5 min |
| manual `/compact` | `sessionTimeoutMs` | 30 min |
| web chat | none | — |
| summary / classifier / fork | fixed | 60 s / 60 s / 120 s |

Category = `timeoutCategory ?? name` (`runner.ts:1096,355-368`), read fresh per spawn. On expiry:
SIGTERM, SIGKILL 5 s later, result `{stdout:"", stderr:"Claude session timed out after Ns", exit:124}`
(`runner.ts:577-585`). stdout/stderr capped at 10 MB, rest drained (`:372-405`).

## stream-json consumption

`runClaudeStream` (`runner.ts:471-586`), used by every execClaude spawn. NDJSON split on `\n`, each
line `JSON.parse`d; parse errors ignored.

| Event | Fields read | Use |
|---|---|---|
| `type: "system"` | `session_id` (string) | session ID (any system event, not only `subtype:"init"`) |
| `type: "result"` | `session_id`, `result` (string) | session ID (last one wins) and **the reply text**; `is_error`, `subtype`, usage, cost are ignored |
| `type: "assistant"` | `message.id`, `message.content[]`: `text.text`, `tool_use.{id,name,input}` | only when callbacks given: text deltas (by diffing accumulated text per `message.id`, `"\n"` between messages) and `● <Tool>(<summary>)` tool lines |
| `type: "user"` | `message.content[]` `tool_result.{tool_use_id, content, is_error}` | `  ⎿  [<tool>] <first line, 80 chars>` lines (Discord streaming preview only) |

Returned `rawStdout` is only the `result` text; exit code is the process exit code. Tool summaries
(`runner.ts:588-601`): `Read/Write/Edit(file_path)`, `Bash(command, 60)`, `Grep(pattern in path)`,
`Glob(pattern)`, `WebSearch(query)`, `WebFetch(url, 60)`, else `<Name>(...)`. Discord tool reactions
parse the tool name back out of these lines with `/^● ([^(\s]+)\(/` (`runner.ts:604-607`).

`streamClaude` (web chat, `runner.ts:1553-1635`) differs: creates the session on the first `system`
event carrying `session_id`; emits each assistant `text` block whole (no diffing); `tool_use` with
`name === "Agent"` → `agent_spawn` (description = `input.description ?? input.prompt`); matching
`tool_result` → `agent_done`; any assistant activity, top-level `tool_use` event, or `result`
"unblocks" the UI; `result.result` is emitted only if no text was seen. Stale-session recovery only
(stderr pattern, no text emitted) by backing up and recursing. No fallback, rate-limit detection,
timeout, watchdog, run log, or signature recovery on this path.

## Scheduler semantics (`start.ts`)

| Aspect | Behaviour | Cite |
|---|---|---|
| Tick | `setInterval(60_000)` from daemon start; not aligned to minute boundaries; while rate-limited nothing runs (see below) | `:979-1025` |
| Order per job | 1) due retry (`retryAt <= now`) → set `retryAt = MAX_SAFE_INTEGER`, run, `continue`; 2) `cronMatches(now)` → run unless in flight; 3) catch-up | `:982-1012` |
| Catch-up | `due = prevCronMatch(schedule, now, offset, 360)`; fire if `lastRanAt < due`, not in flight, not mid-retry | `:999-1011` |
| `lastRanAt` seed | at boot, from `logs/` filenames `^(.+)-YYYY-MM-DDTHH-MM-SS-mmmZ\.log$` (latest per name, marked `ok`); in memory thereafter, set at run **completion** | `:898-912,937-940` |
| In-flight guard | in-memory `Set` of job names; added at `runJob`, removed in final `finally` | `:879-881,916-977` |
| Retry | in memory; on failure with `retry > 0`: `failCount++`; if `≤ retry`, `retryAt = now + retryDelay(300 s)`; else cleared. Success clears. Lost on restart | `:943-956` |
| Rate-limited tick | no runs; jobs due by cron or retry get `lastResult: "skipped"`, `lastRanAt = now` (so catch-up will not replay them) | `:1013-1023` |
| Notify | `notify:false` → nothing; `"error"` → only exit ≠ 0; else DM every `discord.allowedUserIds` (and Telegram) with `[<job>]\n<stdout or "(empty)">` or `[<job>] error (exit N): <detail>` | `:656-666,957-960` |
| One-shot | after run (non-recurring, no pending retry) remove `schedule:` line | `:962-972` |
| Heartbeat timer | `setTimeout` chain of fixed `interval` minutes from (re)schedule time; at fire time: skip if rate-limited (one-time DM "Usage limit hit..."), skip if inside an exclude window, else run. `nextAt` in state is computed separately (`nextAllowedHeartbeatAt`) and is display-only | `:681-757` |
| Heartbeat output | suppressed if stdout trimmed starts or ends with `HEARTBEAT_OK`, unless `heartbeat.forwardToTelegram` is true; else DM to all Discord/Telegram allowed users | `:735-744` |
| Exclude windows | in configured offset; `start<end`: `[start,end)` on listed days; `start==end`: whole listed days; `start>end`: overnight, `minute>=start` on a listed day or `minute<end` on the day after a listed day | `:137-164` |
| Startup | `--trigger`: one `trigger` run (prompt or `Wake up, my friend!`); else `bootstrap()` if no global session (`Wakeup, my friend!` + latest summary). Then background preflight, heartbeat schedule, reload loop, tick | `:762-781` |
| Shutdown | SIGTERM/SIGINT: stop plugins, gateway, web; remove statusline; delete pid; `exit(0)` | `:358-369` |

## Discord parity behaviours (`src/commands/discord.ts`)

**Gateway.** `wss://gateway.discord.gg/?v=10&encoding=json`, intents GUILDS, GUILD_MESSAGES,
GUILD_MESSAGE_REACTIONS, DIRECT_MESSAGES, MESSAGE_CONTENT (`:33,79-84`). Identify properties
`{os, browser:"claudeclaw", device:"claudeclaw"}`; heartbeat with first-beat jitter, missing ACK →
close 4000; resume via `resume_gateway_url`; fatal closes 4004, 4010-4014 stop reconnecting
(`:1709-2013`). REST v10 with `Authorization: Bot <token>`, 429 retry up to 3× using `retry_after`
(`:219-257`). Dispatches handled: READY, RESUMED, MESSAGE_CREATE, INTERACTION_CREATE, GUILD_CREATE,
THREAD_CREATE/UPDATE/DELETE, THREAD_LIST_SYNC (`:1803-1914`).

**Message intake** (`handleMessageCreate`, `:951-1376`):

| Step | Behaviour |
|---|---|
| Filter | ignore `author.bot`; only message types 0 and 19; strip NUL chars |
| Guild trigger | reply to bot → mention (mentions array) → `<@botId>` in content → `listenChannels` has channel → `listenGuilds` has guild → thread whose parent is a listen channel; else skip (logged) (`:552-574`) |
| Auth | global `allowedUserIds` anywhere; `channelAllowedUserIds[channelId]` for guild messages in that channel only. Unauthorized DM → reply `Unauthorized.`; guild → silent |
| Empty | no text, no attachments, no forwarded content → ignored |
| Forward coalescing | forward-only message (reference type 1, no text/attachments) is held 1.5 s per `<channel>:<user>` and merged into a follow-up message as `[Forwarded message from <user>]: ...` |
| Mention strip | `<@id>` / `<@!id>` removed |
| `/plugin` wizard | phase 2; intercepts before anything else |
| Thinking reaction | 👀 added to the triggering message before downloads; removed in `finally` |
| Attachments | first `image/*` → `inbox/discord`, prompt `Image path: <abs>`; first voice (flag bit 13 or `audio/*`) → downloaded + transcribed, transcript only in prompt (phase-2 whisper); first `text/*`/`.txt`/`.md` → fetched inline (not saved), first 2048 chars, wrapped; **every other attachment** (other types and each extra image/voice/text) → `inbox/discord` and `Document path: <abs>` + `Original filename: <wrapped>`. 25 MB cap applies to these "file" attachments only; oversized → prompt tells Claude it was not downloaded. Local name `<attachmentId>-<Date.now()><ext>`, ext kept only if `^\.[A-Za-z0-9]{1,10}$` (else `.jpg`/`.ogg`/none) |
| Thread hire/fire | guild messages < 200 chars go through a Claude classifier; `{"action":"hire","names":[...]}` creates public threads (`type 11`, auto-archive 4320) under the channel and posts a confirmation; `fire` finds child threads by exact (case-insensitive) name, removes session, deletes the channel. Either ends handling |
| Skill routing | content starting `/name` → `SKILL.md` from `.claude/skills`, `~/.claude/skills`, plugin caches (`plugin:skill` supported) injected as `<command-name>/name</command-name>` + skill text + wrapped args (`skills.ts:111-145`) |
| Prompt | lines: `[Discord Channel: <channelNames[id] ?? id>]` or `[Discord DM]`; `[Discord from <username>]`; `Message: <untrusted user-message>`; attachment lines; `[In reply to <user>]: <content>` or forward line |
| Run | `runUserMessage("discord", prompt, guild ? channelId : undefined, threadAgentName, streamCb?.onChunk, onToolEvent, undefined, [channelId, parentId?])` |
| Tool reactions | per tool category, once each, queued 300 ms apart, all removed when the run ends: `mcp__*` 🔌, Read/Grep/Glob/LS 📖, Edit/MultiEdit/Write/NotebookEdit ✏️, Bash 💻, WebFetch/WebSearch 🌐, Agent/Task 🤖, Skill 🧩, other 🛠️ (`:39-65,339-376`) |
| Streaming (opt-in) | `⏳` placeholder on first chunk/tool event, PATCHed every 1.5 s with `_<last 1990 chars, "_" escaped>_`, deleted at the end (`:829-943`) |
| Reply | exit ≠ 0 → `Error (exit N): <detail>`. Else: first `[react:<x>]` (any case) becomes a reaction on the triggering message and all such tags are removed; absolute image paths (`png/jpe?g/gif/webp`, whitespace-delimited) that realpath inside `imageOutputRoots` and have `mtime >= requestStart - 30 s` are removed from text and uploaded as `files[i]` multipart on the last chunk; text split into hard 2000-char slices; empty → `(empty response)` (`:380-481,1348-1363`) |
| Exceptions | `Error: <message>` posted to the channel |

`detail` = stderr, else JSON `result`/`error.message`/`error` from stdout, else stdout
(`messaging.ts:8-23`).

**Threads.** `knownThreads` (memory only) maps thread → `{parentId, agentName}`, filled from
GUILD_CREATE, THREAD_CREATE/UPDATE/LIST_SYNC, and recovered on demand via `GET /channels/<id>` when a
message arrives in a channel that has a `sessions.json` entry (`:968-981`). Every thread with a name
gets `agentName = agentDirKey(name, threadId)` = slug of the name (lowercase, `[^a-z0-9_]+`→`-`,
trimmed) truncated so that `<slug>-<threadId>` ≤ 64 chars (`runner.ts:747-757`); its runs use cwd
`agents/<that>` (created, symlink-checked by realpath, `runner.ts:765-787`). On GUILD_CREATE, each
snowflake-keyed session is rejoined (`PUT thread-members/@me`, preceded by DELETE if not a member)
(`:486-548`). THREAD_CREATE under a listen channel → join.

**Slash commands** (bulk `PUT /applications/<appId>/commands` on every READY, `:694-806`): `start`,
`reset`, `compact`, `status`, `context`, `config` with subcommands `show`, `model value:<""|opus|sonnet|haiku>`,
`profile value:<__inherit__|direct|<ccs profile>>` (choices fixed at registration, max 25). Interaction
auth uses only global `allowedUserIds`. Guild invocations act on `<channelId>`; DMs on global.
`/context` reads the session JSONL and reports the last `message.usage` against a hard-coded 200,000.
`/config model` always writes global `settings.model`. Ephemeral replies use `flags: 64`. Button
`sec_(yes|no)_<8hex>` calls `http://127.0.0.1:9999/confirm/<id>/<action>` (`:1631-1659`).

**Guild join**: if not present at READY and listed in `allowedGuilds`, a Claude-written welcome is
posted to `system_channel_id` (`:1672-1705`).

**Outbound DMs** (cron/heartbeat/send): `POST /users/@me/channels {recipient_id}` then post, per user,
every time (`:291-304`).

## Web UI HTTP API (`src/ui/server.ts`)

Bound to `web.host`, port `--web-port` / `web.port`, retrying `+1` up to 10 times on EADDRINUSE
(`start.ts:506-598`). `idleTimeout: 0`. Global checks, in order:

1. `Host` must be one of `127.0.0.1:<port>`, `localhost:<port>`, `[::1]:<port>`, `<host>:<port>` unless
   bound to `0.0.0.0`/`::` → else `421 Bad Host` (`:42-54`).
2. POST/DELETE with an `Origin` header must equal `http(s)://<Host>` → else `403 Bad Origin` (`:58-66`).
3. `/` and `/index.html` serve the SPA with no auth; `/api/health` no auth.
4. All other `/api/*`: `Authorization: Bearer <web.token>` or `?token=<web.token>` (constant-time
   compare); `/api/inject` also accepts `settings.apiToken`. Else `401 {"ok":false,"error":"unauthorized"}`.
   The SPA takes `?token=` from the URL into `sessionStorage.__claw_tok` and sends it as a bearer
   header (`ui/page/script.ts:1-31`).

Errors in handlers are mostly returned as **HTTP 200** `{ok:false, error}`.

| Method | Path | Request | Response | Cite |
|---|---|---|---|---|
| any | `/`, `/index.html` | — | HTML | `:68-72` |
| any | `/api/health` | — | `{ok:true, now}` | `:75-77` |
| any | `/api/state` | — | `{daemon:{running,pid,startedAt,uptimeMs}, heartbeat:{enabled,intervalMinutes,nextAt\|null,nextInMs\|null}, jobs:[{name,schedule,prompt}], security, telegram:{configured,allowedUserCount}, discord:{...}, session:{sessionIdShort,createdAt,lastUsedAt}\|null, web}` | `state.ts:25-64` |
| any | `/api/settings` | — | `{timezone, timezoneOffsetMinutes, heartbeat, security, telegram:{configured,allowedUserCount}, discord:{...}, web, runtime}` | `state.ts:6-23` |
| POST | `/api/settings/heartbeat` | any of `{enabled, interval, prompt, excludeWindows[]}` | `{ok:true, heartbeat:{enabled,interval,prompt,excludeWindows}}`; writes settings.json and live-applies | `:102-168` |
| GET | `/api/settings/heartbeat` | — | `{ok:true, heartbeat}` (from disk) | `:248-254` |
| GET | `/api/settings/runtime?scope=<key>` | — | `{ok, scope\|null, launcher:{kind,profile,directFallback}, profiles:[{label,names[]}], profilesAvailable, overrides:[key], scopes:[{key,label,overridden}]}` | `:170-201` |
| POST | `/api/settings/runtime` | `{kind:"direct"\|"ccs", profile?, scope?}` or `{inherit:true, scope}` | `{ok, launcher, scope, sessionReset:true[, inherited:true]}`; resets that scope's session, then reloads settings | `:203-246` |
| any | `/api/technical-info` | — | `{daemon, files:{settingsJson (secrets redacted as "<redacted:N chars>"), sessionJson (redacted), stateJson}, snapshot}` | `state.ts:66-111` |
| POST | `/api/jobs/quick` | `{time:"HH:MM", prompt (≤10k), recurring?/daily?}` (default recurring true) | `{ok:true, name, schedule, recurring}` | `:260-269`, `jobs.ts:12-44` |
| DELETE | `/api/jobs/<name>` | name `^[a-zA-Z0-9._-]+$` (flat jobs only) | `{ok:true}` | `:271-281`, `jobs.ts:46-53` |
| any | `/api/jobs` | — | `{jobs:[{name, schedule, promptPreview (160 chars)}]}` | `:283-290` |
| any | `/api/logs?tail=N` (20..2000, default 200) | — | `{daemonLog:[lines], runs:[{file, lines}]}` (5 newest run logs by mtime among the first 200 dir entries) | `logs.ts:5-55` |
| GET | `/api/sessions` | — | `[{id, agent, channel:"web"\|"discord"\|"agent"\|"unknown", lastUsedAt, createdAt, turnCount, firstMessage, lastMessage}]` from session.json, `agents/*/session.json`, snowflake keys of sessions.json, and up to 20 orphan JSONLs | `sessions.ts:75-183` |
| GET | `/api/sessions/<uuid>/messages?limit=1..2000&offset=N\|-1` | — | `{messages:[{role,text,timestamp,uuid}], total}` (`offset=-1` = last `limit`) | `:322-332`, `sessions.ts:190-232` |
| GET | `/api/usage` | — | `[{sessionId, label, channel, inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, estimatedCostUsd, cacheHitPct, turnCount, lastUsedAt}]` (60 s cache, fixed Sonnet pricing) | `usage.ts:92-140` |
| GET | `/api/agents` | — | `[{id,name}]` = hard-coded first entry + every dir under `agents/` | `sessions.ts:234-249` |
| POST | `/api/inject` | `{message}` | `{ok:true, result, exitCode}`; 400 if empty; output also sent to the first Telegram allowed user | `:334-354` |
| POST | `/api/chat` | `{message, attachments?:[{name,type,data(base64)}]}` (≤5, ≤10 MB each) | **SSE** `data: {...}\n\n` events: `chunk{text}`, `unblock`, `agent_spawn{id,description}`, `agent_done{id,description,result}`, `done`, `error{message}`; text attachments inlined as fenced blocks, images saved to `<tmpdir>/claudeclaw-uploads/<uuid>.<ext>` and deleted afterwards | `:356-472` |
| — | anything else | — | `404 Not found` | `:474` |

## Things outside `.claude/claudeclaw/`

| Path | R/W | Behaviour | Cite |
|---|---|---|---|
| `<cwd>/CLAUDE.md` | R every call; W once | If absent: created from legacy `.claude/CLAUDE.md` (if any) + managed block `<!-- claudeclaw:managed:start -->\n<IDENTITY.md + USER.md + SOUL.md from install prompts/>\n<!-- claudeclaw:managed:end -->`. Never rewritten once it exists. Run at start, one-shot, `send`, Discord gateway start | `runner.ts:796-838` |
| `<cwd>/.claude/settings.json` | R/W | `statusLine: {type:"command", command:"node .claude/statusline.cjs"}` set at start, deleted at stop (whole file re-serialised); preflight adds `enabledPlugins[<key>]=true` | `start.ts:184-215`, `preflight.ts:127-140` |
| `<cwd>/.claude/statusline.cjs` | W | Node script: reads `claudeclaw/state.json` + `daemon.pid`, renders box | `start.ts:27-123` |
| `<cwd>/agents/<name>/` | R/W | Claude cwd for agent jobs and Discord threads; `jobs/*.md`; legacy `session.json`, `session_fallback.json` | `runner.ts:765-787`, `jobs.ts:119-144` |
| `~/.claude/projects/<slug>/<uuid>.jsonl` | R | Session transcripts for `/context`, `/api/sessions*`, `/api/usage`. `<slug>` = cwd with `/`, `\`, `.` → `-`; if missing, scan all project dirs, newest mtime wins | `sessionFiles.ts:8-41` |
| `~/.claude/projects/*` (names) | R | `--stop-all` / `status --all` decode dir names back to paths (`-`→`/`) to find other daemons' pid files | `stop.ts:56-94`, `status.ts:22-51` |
| `~/.ccs/config.yaml` | R | CCS profile discovery: `cliproxy.providers[]`, keys of `profiles`, keys of `accounts` | `ccs-profiles.ts:27-49` |
| `<install>/prompts/` | R | `IDENTITY.md`, `USER.md`, `SOUL.md` (CLAUDE.md seed), `SUMMARY.md` (rotation), `heartbeat/HEARTBEAT.md` | `runner.ts:38-39,898-916`, `rotation.ts:12-13` |
| `session.summaryPath/` | R/W | rotation summaries `YYYY-MM-DD-HH-mm.md` | `rotation.ts:53-120` |
| `<cwd>/.claude/skills`, `~/.claude/skills`, `~/.claude/plugins/cache/*/*/*/skills` | R | Discord `/skill` resolution | `skills.ts:111-145` |
| `~/.claude/plugins/{marketplaces,cache,installed_plugins.json,known_marketplaces.json}` | W | background preflight on every start installs a fixed list of plugin repos (git clone, `bun install`) | `preflight.ts:22-47,400-444` |
| `<tmpdir>/claudeclaw-uploads/` | W | web chat image uploads, deleted after the stream | `server.ts:410-456` |
| `http://127.0.0.1:9999/confirm/...` | net | secretary button callback | `discord.ts:1642` |
| env | R | `DISCORD_TOKEN`, `TELEGRAM_TOKEN`, `SLACK_BOT_TOKEN`, `SLACK_APP_TOKEN`, `HOME` | `config.ts`, `runner.ts:1703` |

Phase-2 footprint only: `inbox/telegram`, `inbox/slack`, `outbox/slack`, `whisper/` (binary, models,
tmp), `permission-mode.json` (Telegram `/mode`), plugin loading from `.claude/claudeclaw/<id>/`,
`node_modules/<pkg>`, `~/.openclaw/extensions/<id>` (`plugins.ts:150-168`), Telegram `tg:*` and Slack
`slk:*` session keys.

CLI surface (`index.ts:10-33`): `start [--prompt <text>] [--trigger] [--telegram|--discord|--slack]
[--debug] [--web] [--web-port N] [--replace-existing]`, `--stop`, `--stop-all`, `--clear`,
`status [--all]`, `send <msg> [--telegram] [--discord]`, `discord`/`telegram`/`slack` (standalone);
no arguments = `start`.

## Compat traps & oddities

Things a careful reimplementation would get wrong, or must consciously choose to replicate or fix.

| # | Trap | Cite |
|---|---|---|
| 1 | Cron ANDs DOM and DOW; `*/n` on DOM/month is `value % n == 0` (even days, months 3/6/9/12); `n/m` ignores the step; names, `7`, `?`, `L`, macros silently never match. | `cron.ts:3-42` |
| 2 | A schedule with < 5 fields throws a `TypeError` inside the 60 s tick, aborting the rest of that tick (later jobs, `updateState`); at boot the same throw happens in `updateState()`. | `cron.ts:25`, `start.ts:854,914,993` |
| 3 | `recurring:` absent means **one-shot**: after the first run the daemon deletes the `schedule:` line. | `jobs.ts:57`, `start.ts:962-972` |
| 4 | Frontmatter is prefix line matching: first matching line wins, one quote stripped per side, no `#` comments (`schedule: "0 9 * * *" # x` never matches), no YAML. | `jobs.ts:28-100` |
| 5 | Cron jobs are sessions: each job name (or `agent:<agent>`) is a key in `sessions.json` with its own serial queue and per-scope launcher override; never pruned. | `start.ts:927`, `server.ts:178-183` |
| 6 | Catch-up seeds `lastRanAt` from log **filenames** (start time). Agent-job logs are written to `logs/<agent>/`, which the non-recursive seed scan never sees, so agent jobs can be replayed after a restart. In-flight/retry/lastResult are memory only. | `start.ts:898-912`, `runner.ts:1066` |
| 7 | Recovery retries re-wrap an already-wrapped argv: signature-error retry, stale-session retry and post-compact retry call `buildClaudeCommand(args)` on `args` that already start with `claude`/`ccs <p>`, giving `claude claude -p ...` or `ccs <p> ccs <p> --print ...`. Pre-launcher code passed `args` as-is. | `runner.ts:1223,1274,1403` (introduced in `d35a720`) |
| 8 | New sessions are persisted even when the run failed or timed out (any `session_id` seen), except on rate-limit output. | `runner.ts:1309-1331` |
| 9 | Snowflakes: `discord.allowedUserIds` may be bare JSON numbers; the reader regex-extracts digits to avoid precision loss, but every TS **writer** (`JSON.parse` + `stringify` of the whole file) would round such numbers. The extraction regex stops at the first `}` inside the `discord` block. | `config.ts:528-540`, `settings.ts` |
| 10 | Every Discord thread (not just "hired" ones) runs with cwd `agents/<slug(name)>-<threadId>`; renaming a thread changes the slug and therefore the cwd, so the next `--resume` can miss the transcript and fall into stale-session recovery. | `discord.ts:201-208`, `runner.ts:747-757` |
| 11 | Every guild message under 200 chars spawns an extra Claude process (intent classifier, unfiltered env, global launcher, no security args) before the real run. | `discord.ts:584-635,1151-1213` |
| 12 | Launcher scope differs by path: normal guild runs use `[channelId, parentId]`; manual `/compact` uses `[channelId]` only; pending resume `[sessionKey]`; web chat, heartbeat, rotation summary, classifier use global. A thread inheriting its parent's override can be compacted under a different provider. | `discord.ts:1337`, `runner.ts:1018` |
| 13 | Timeout category is the run name, so Discord, inject and send use `timeouts.default` (5 min), not a Discord-specific value; web chat has no timeout at all; `sessionTimeoutMs` only affects manual `/compact`. | `runner.ts:355-368,1096,1463-1665` |
| 14 | `heartbeat.forwardToTelegram` also gates Discord DMs, and defaults to `false` on parse but `true` in a freshly written file. | `config.ts:79,364`, `start.ts:739-743` |
| 15 | `state.json` `nextAt` caps at 48 h (wrong for monthly/rare jobs); heartbeat `nextAt` is computed independently of the actual timer. | `cron.ts:44-53`, `start.ts:690-756` |
| 16 | The 25 MB cap applies only to "extra" attachments; the first image is downloaded without a size check; the first voice/text file is never passed as a path. | `discord.ts:1095-1148` |
| 17 | `extractImagePaths` only uploads files whose realpath is under `imageOutputRoots` (default `outbox/discord`, which the daemon never creates) and whose mtime is within 30 s before the request start. | `discord.ts:396-431` |
| 18 | Compact warnings/auto-compact events are emitted but nothing subscribes, so users never see them. | `runner.ts:121-139` |
| 19 | Project slug for JSONL lookup replaces only `/ \ .`; Claude Code's own sanitizer appears broader (non-alphanumerics), so paths with spaces/underscores rely on the slower scan fallback. `--stop-all`/`status --all` decode slugs lossily. | `sessionFiles.ts:8-10`, `stop.ts:68` |
| 20 | Shutdown exits 0 on SIGTERM, so a supervisor configured to restart only on failure will not restart it. | `start.ts:358-369` |
| 21 | Docs are stale vs code: `docs/RUNTIME_LAUNCHER.md` says there is no per-scope override; `docs/MULTI_SESSION.md` says guild main channels use the global session and new sessions use `--output-format json`. Code: per-scope `runtime.launchers`, every guild channel isolated, always stream-json. | `docs/*.md` vs `runtime-launcher.ts`, `discord.ts:1304` |
| 22 | `runner.ts` imports `cleanSpawnEnv` from `runtime-launcher.ts` (which does not export it) and also declares it locally; Bun accepts this. `runClaudeStreaming` is dead code. `runtime.*.directFallback` is parsed but has no effect. | `runner.ts:32,107,627`, `config.ts:200-205` |
| 23 | Settings writers drop formatting and rewrite the whole file; concurrent writers (UI, `/config`, user edits) race with no locking. | `ui/services/settings.ts` |

## Could not determine

- How Claude Code itself sanitises project directory names (relevant to #19); stated here only as
  observed TS behaviour.
- Bun's default `stdin` for `Bun.spawn` when omitted (assumed not a pipe); no call site except the
  classifier writes stdin.
- The external supervisor (launchd/systemd wrapper, `daemon.log` redirect) is outside this repo
  apart from the `commands/start.md` instructions.

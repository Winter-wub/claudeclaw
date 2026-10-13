# Management API resources (v1)

Resolves [#21 "List the Management API resources the app's screens need"](https://github.com/Winter-wub/claudeclaw/issues/21) on the wayfinder map [#2](https://github.com/Winter-wub/claudeclaw/issues/2), and carries every later decision that changes it. Each later decision is folded into the section it changes and cited inline as (#N). The architecture these routes run on is [`architecture.md`](architecture.md), whose Decision index lists every closed ticket.

> **Delegated decision.** An agent made the #21 decisions on 2026-10-13 because the maintainer asked to "continue without asking". They are open to revision. The enumeration follows from closed tickets; the shapes in §1 and the decisions in §7 are design choices. [Review points](#8-review-points) lists the ones most likely to need a second look.

Terms follow `GLOSSARY.md`: Daemon, Watchdog, Worker, Turn, Scope, Launcher, Refused run, Parked session, Device, API token, Token permission, Principal, Revision, Deadline, Detach, Pairing code, Pairing string, Daemon log, Run log, Run index, Back-fill, Housekeeping, Inbox, Transcript, Config dir, Cost baseline, Bug-report export, First-start import, Job import, Offline write, Crash record, Attach. Type names are `claudeclaw-api` types (`api::…`); the crate also owns the route path constants and the `api::Deadline` table (architecture §4), so this list is what those constants and `openapi.json` must contain.

**Inputs #21 had to honour:**

| Ticket | What it fixed for the API |
|---|---|
| [#8](https://github.com/Winter-wub/claudeclaw/issues/8) | A Launcher is a path; picker = `direct` + paths in use; thread → channel → global (+ job frontmatter); sessions keyed by (Scope, Launcher); refused, never fallen back; Test launcher; errors never show the command |
| [#10](https://github.com/Winter-wub/claudeclaw/issues/10) | 8 screen groups; one API for local and remote; the app never writes Daemon files; chat into any Scope, not mirrored to Discord; OS notifications |
| [#11](https://github.com/Winter-wub/claudeclaw/issues/11) | [`architecture.md`](architecture.md): axum + utoipa, `api` crate owns wire types, `DaemonEvent`, the error envelope and path constants; principals `Local` / `Device` / `ApiToken` / `Pairing`; audit layer; secrets write-only; typed Scope keys |
| [#13](https://github.com/Winter-wub/claudeclaw/issues/13) | HTTP/JSON + SSE; local 0600 socket / owner-only pipe; opt-in pinned TLS; Pairing codes (10 min) → per-Device keys, revocable; Devices are full admin; scoped hashed API tokens; legacy `apiToken` imported; audit of every mutation |
| [#16](https://github.com/Winter-wub/claudeclaw/issues/16) | 9 screens; Usage gets an "All daemons" view; Logs stay per daemon; Fire archives `agents/<slug>-<id>/` to `agents/.archive/`, permanent delete is separate; names shown, "Copy ID" for snowflakes; next-run preview uses croner OR semantics |
| [#17](https://github.com/Winter-wub/claudeclaw/issues/17) | `GET /v1/import` + plan and apply; `api::ImportPlan`; the legacy token's permission name |
| [#20](https://github.com/Winter-wub/claudeclaw/issues/20) | `/v1/status` embeds `SupervisorStatus` + `boot_id`; crash history = `supervisor.crashes`; `device_revoked`; `DaemonReachability` in `api`; mutations disabled while down |
| [#22](https://github.com/Winter-wub/claudeclaw/issues/22) | The CLI's route list and JSON shapes; Token permissions in colon form; `JobsCheck`, `JobsAudit`, `ImportPlan`, the status type with `health` / `problems[]` |
| [#23](https://github.com/Winter-wub/claudeclaw/issues/23) | D5's logs, runs, audit and summary routes; `LogEntry`, `RunRecord`; `logs:read`, `runs:content`; content-free Daemon logs |

Tickets closed after #21 that change this list: #24, #26, #27, #28, #29, #32, #33, #37, #38, #39, #40, #41, #42, #43, #44, #45, #46, #47, #48.

---

## 1. Conventions

These apply to every route in §2.

### 1.1 Base path, versioning, wire format

- Every route is under **`/v1`**. Inside v1, changes are additive only: new routes, new optional request fields, new response fields, new enum variants behind a catch-all. A breaking change means `/v2`.
- JSON in and out (`application/json`), except the run-log body (`text/markdown`) and SSE (`text/event-stream`).
- **Times** are RFC 3339 UTC strings. **Durations** in responses are `*_ms` integers. Query-string waits are whole seconds (`wait=30`).
- **IDs are strings**: snowflakes, UUID v7 turn/run IDs, Device and token IDs.
- **Field names** are `snake_case`, as in #22's JSON and #23's types. The one exception is `/v1/settings`, whose keys are `settings.json`'s own key names (§2.6), so a dotted `config set` path and the file agree.
- `operationId` = the **Op** column below (`jobs.update`). The same string is the audit log's `action`, so the prototype's `jobs.update` / `launchers.test` audit rows come for free.

### 1.2 Principals and the "Who" column

The server's auth layer yields one principal (architecture §4, `claudeclaw-server`):

| Principal | How | Who column |
|---|---|---|
| `Local` | any caller on the 0600 socket / owner-only pipe | `admin`, `local` |
| `Device(id)` | TLS client cert whose SPKI matches `devices.json` | `admin` |
| `ApiToken(id, allow)` | `Authorization: Bearer` on the remote listener | the Token permission named in the cell |
| `Pairing` | an unknown client cert | only `POST /v1/pair` |

- **`admin`** = `Local` or `Device`. Devices are full admin in v1 (#13).
- **`local`** = `Local` only. A Device or token gets `403 local_only`. These are marked **[local]**.
- **`any`** = any authenticated principal, with no permission needed.
- A **Token permission** cell (e.g. `jobs:run`) means: `admin`, **or** an API token holding that permission.
- On the local socket an `Authorization` header is ignored; the caller is `Local`.

### 1.3 Token permissions (final enum, `api::TokenPermission`)

| Permission | Grants | Notes |
|---|---|---|
| `send` | `turns.create` into the **`global` Scope only**, plus reading Turns this token created | The legacy `apiToken` is imported with exactly this (#17 §3), the inject equivalent |
| `jobs:run` | `jobs.run`, plus reading Turns this token created | |
| `status:read` | `health.get`, `status.get`, `events.stream`, `jobs.list`, `jobs.check`, `jobs.audit`, `update.get` | Content-free only: no prompts, job bodies or reply text |
| `logs:read` | `logs.*` (including `logs.export`, #28), `runs.list`, `runs.get`, `audit.list` | #23 D5. The Bug-report export holds strictly less than `logs:read` already returns, so it needs no new permission (#28). |
| `runs:content` | `runs.log` (Run log bodies) | Only with `logs:read`; `tokens.create` refuses it alone (`422 invalid_permission_set`) |

Everything else (settings, job edits, Launchers, sessions, Discord, agents, usage, import, housekeeping, devices, tokens, daemon control) is `admin` only. A token without the permission gets `403 missing_permission` with the permission name in `error.details.required`. These names are final (#22's provisional names, unchanged).

### 1.4 Errors

One envelope, the same as the CLI's `--json` errors (#22 §4):

```json
{ "error": { "code": "launcher_not_runnable", "message": "…", "hint": "…", "details": { } } }
```

- `code` is `api::ErrorCode`, `snake_case`, stable within v1. `message` never echoes a Launcher's arguments, a secret, or request content (#8, #23 D6).
- Catalogue (HTTP status → codes):
  - **400** `bad_request`, `invalid_scope`, `invalid_cursor`
  - **401** `unauthorized`, `unknown_device`, `device_revoked` (#20), `pairing_code_invalid`
  - **403** `missing_permission`, `local_only`
  - **404** `not_found`, `transcript_not_found` (`details{reason: expired | not_located | unreadable, searched_roots, retention_days}`, #39)
  - **409** `revision_mismatch` (`details.current`), `job_exists`, `agent_exists`, `session_in_use`, `remote_disabled`, `plan_changed`, `use_job_frontmatter`, `export_busy` (one export at a time, #28)
  - **410** `pairing_expired`
  - **413** `payload_too_large`
  - **422** `job_invalid` (`details.diagnostics`), `launcher_not_runnable`, `ts_only_key`, `unknown_key`, `invalid_value`, `invalid_permission_set`, `exe_check_failed`, `invalid_host` (#41)
  - **428** `revision_required`
  - **503** `daemon_starting`, `daemon_stopping` (both with `Retry-After`)
  - **504** `deadline_exceeded`, the server's Deadline backstop (#40)
  - **507** `insufficient_storage`: `turns.create` with attachments below the free-space floor (#44)
  - **500** `internal`, `export_leak` (the Bug-report export failed closed; `details{section, field, kind}`, never the value, #28)
- The CLI adds its own codes on top (`daemon_not_running`, `statusline_foreign`, `data_unreadable`, …, #22 §4, #24); they never come from the server.

### 1.5 Revisions (no silent lost updates)

Things a person edits as a whole carry a **`revision`**: the first 16 hex of SHA-256 over the file bytes. That covers job files, `settings.json` and an agent's persona.
- Writes accept `If-Match: <revision>`. A mismatch gives `409 revision_mismatch` with the current revision, so the app can show "changed on disk since you opened it" instead of overwriting.
- **Whole-file writes** (`jobs.replace`, `agents.persona.put`) **require** `If-Match` (`428 revision_required`).
- **Key-level writes** (`jobs.update`, `settings.update`) accept it optionally. Without it they are line edits / patches over a fresh read, which is what the CLI sends.

### 1.6 Pagination, waits, SSE

- **Lists that grow** (logs, runs, audit, transcript messages) page backwards: `?limit=&before=<cursor>` → `{ <items>: […], next: <cursor>|null }`, the #23 D5 shape. Cursors are opaque. Short lists (jobs, scopes, devices…) are returned whole.
- **Long operations.** `turns.create`, `jobs.run` and `sessions.compact` (#40) return `202` at once. `?wait=<s>` (max 300) holds the request until the Turn finishes and then returns `200` with the `TurnRecord`, so `curl` automation needs one call. Pairing uses the same `wait` as a long-poll. `launchers.test` is synchronous, bounded by its own `timeout_s` (10–600), and a second request for the same Launcher joins the running test (#40).
- **SSE** (axum `Sse`, 15 s keep-alive) on exactly three families: `/v1/events`, `/v1/turns/{turn_id}/events`, `/v1/logs/{source}/stream`. Each starts with `event: hello` carrying `boot_id`. Each reports lost events as `event: gap` and never skips silently (#23 D5). A client that sees a new `boot_id` re-fetches.
- **Body limits**: 64 MiB on `turns.create` (attachments, §2.3), 1 MiB everywhere else (`413 payload_too_large`).

### 1.7 IDs in paths

- **Scope** is the typed key from architecture §6 / #22: `global`, `discord:<id>` (channel or thread), `job:<name>`, `agent:<name>`. Percent-encoded in paths.
- **Job** is the job name: the file stem, or `<agent>/<stem>` for agent jobs, with the `/` sent as `%2F`. axum matches on the raw path and decodes the segment afterwards, so `jobs/researcher%2Fpaper-digest/run` is one `{job}`. `claudeclaw-client` and the app's `openapi-fetch` wrapper do the encoding.
- **No static segment shares a position with a name.** Job tools live under `/v1/schedule/…` and archived agents under `/v1/archived-agents/…`, so a job called `check` or an agent called `archive` can't shadow a route.
- A Launcher (a path) never appears in a URL path. It goes in a body or the `launcher=` query parameter.

### 1.8 While the Daemon starts or stops

- The Worker serves the API **before** the First-start import finishes (#17 §1), so the app can show progress. During that time reads are served and `health.state` is `starting` with `import.step`; every mutation gets `503 daemon_starting`.
- After stop begins, mutations get `503 daemon_stopping`; reads and streams keep working until the listener closes.
- While no Worker answers, nothing is queued for later (#20 D4). The app disables its controls. A Daemon that gave up has no API, so Give-up and Held-start state is read offline from `supervisor.json` (#45).

### 1.9 Deadlines and Detach (#40)

- **Every route has an `api::Deadline`**, declared beside the route constants and published as `x-claudeclaw-deadline` in `openapi.json`. The app's bridge and the CLI apply it per call; the server enforces it as a backstop with `504 deadline_exceeded`. Defaults: 30 s; 120 s for slow and upload calls; `timeout_s` + 45 s for `launchers.test`. Streams use an idle limit instead (45 s of SSE silence before a reconnect).
- **Run-starting handlers Detach.** hyper drops a handler as soon as its client disconnects (logged as `api.client_gone`), so the work is handed off before the response. Cancel, a Deadline or a disconnect means "stop waiting"; in v1 no client action kills a run.

---

## 2. Resources

Columns: **Op** (operationId and audit action), **Route**, **Request → Response**, **Who** (§1.2–1.3), **Used by** (screen ▸ control, or CLI command). **[SSE]** marks a stream and **[local]** a local-socket-only route. Mutating routes are audited (#13) unless noted.

### 2.1 Daemon and status

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `health.get` | `GET /v1/health` | → `Health{state: starting\|ready\|stopping, boot_id, version, api_version, import: {running, step}\|null}` | `any` | App reachability probe (#20 D1, 3 s); CLI `start -d` and `restart` wait for `ready`; CLI version-mismatch check |
| `status.get` | `GET /v1/status` | → `DaemonStatus` (below) | `status:read` | Status & Jobs (cards, crash timeline, problems), per-daemon banner, tray dot, sidebar badges; #20 D4 snapshot; CLI `status`; `claudeclaw statusline` (#24) |
| `openapi.get` | `GET /v1/openapi.json` | → the committed OpenAPI 3.1 document | `any` | Automation; `claudeclaw-client` version check |
| `daemon.stop` | `POST /v1/daemon/stop` | `{}` → `202 StopAccepted{boot_id, in_flight_runs, deadline_at}`. The Worker drains (≤30 s, #9), the Watchdog records `stop.by: "api"` and exits 0 | `admin` | Tray / Status ▸ Stop; CLI `stop` (path 1) |
| `daemon.restart` | `POST /v1/daemon/restart` | `{}` → `202 RestartAccepted{old_boot_id}`. The Watchdog replaces the Worker under the same lock; not counted as a crash | `admin` | Status ▸ Restart; CLI `restart` |
| `daemon.reexec` | `POST /v1/daemon/reexec` | `ReexecRequest{exe}` (unresolved PATH location from `argv[0]`, #22 §7, #36) → `202 ReexecAccepted{old_boot_id, target_version}`; `422 exe_check_failed` if `<exe> --version` fails, before anything stops | **[local]** | CLI `update` |
| `update.get` | `GET /v1/update` | → `UpdateInfo{current, latest, available, checked_at, release_url}` (the Worker's daily check, #6; channel implied by the running version, #35) | `status:read` | Settings ▸ Updates; "new version" OS notification; CLI `update --check` (online) |
| `update.check` | `POST /v1/update/check` | `{}` → `UpdateInfo` | `admin` (not audited) | Settings ▸ Updates ▸ Check now |
| `housekeeping.run` | `POST /v1/housekeeping` | `{dry_run}` → `HousekeepingReport{would_delete\|deleted: {<kind>: {count, bytes}}, freed_bytes}` (#44) | `admin` (audited unless `dry_run`) | CLI `housekeeping [--dry-run]`; available to the app |

**`api::DaemonStatus`** is the #22 `status --json` type, served online with `source: "api"`:
- `dir`, `state` (#20 D1 local states; always `running` when served), `health: ok|degraded`, `problems[]: Problem{kind, scope?, job?, launcher? (#42), since}`.
- Problem kinds: `discord_disconnected`, `launcher_refused`, `job_invalid`, `settings_invalid`, `update_available`, `version_mismatch`, `import_errors`, `run_path_capture_failed` (#26), `leftover_runs` (#29), `run_pipe_held` (#37), `claude_code_outdated` (#42), `run_locale_unavailable` (#43), `disk_low` (#44), `jobs_dir_unavailable` (#46).
- `version{daemon}`, `started_at`, `boot_id`, `socket`, `remote{enabled, listen[], fingerprint, fingerprint_curl}`, `adapters[]: {transport, state, since}`, `heartbeat{enabled, next_at}`, `jobs{total, invalid, warnings, next[]: {job, at}}`, `active_runs`, `launchers{default, refused_scopes[]}`, `update{available, latest}`.
- `run_path` (#26) and `run_locale: RunLocale{setting: auto|inherit, source: inherited|default|lc_all|setting|unavailable|not_applicable, lc_ctype_added, effective, charmap, verified}` (#43).
- `run_index: RunIndexState`, the Back-fill progress (#44).
- `beacons[]: {beacon, quiet_ms}`, the Worker's stall-monitor beacons (#27; see §8 for the name).
- An advisory notice when the login shell exports `CLAUDE_CONFIG_DIR` but `direct` runs use a different Config dir. Only the variable's presence and the mismatch are reported, never its value (#48).
- **`supervisor: SupervisorStatus`** (#20 D2; architecture §4). Crash history is `supervisor.crashes`, each with `cause` and `counted` (#27) and `reaped` (#29). #45 adds `gave_up.held_starts`, `gave_up.reminded_at`, `give_ups[]`, `watchdog.service_manager` and `stop.by: clear_give_up`. All of these are additive.
- CLI-only fields (`lock_holder`, `watchdog_pid` when offline) are `null` online.

### 2.2 Events

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `events.stream` | `GET /v1/events?kinds=<prefix>,…` **[SSE]** | `hello{boot_id}`, then `event: daemon`, `data: DaemonEvent`; `gap{reason: lagged\|restart, missed?}` | `status:read` | Every screen's live refresh; OS notifications (job failed / refused / finished, update available); tray |

**`api::DaemonEvent`** (tagged by `kind`; content-free, the same rule as Daemon logs, #23 D6):
- `status.changed{health, problems}`, `adapter.changed{transport, state}`
- `turn.queued{turn_id, scope, position}`, `run.started{run_id, turn_id, scope, job?, launcher}`, `run.finished{record: RunRecord}`, `turn.finished{turn_id, scope, outcome}`
- `job.fired{job, turn_id, catch_up}`, `job.changed{job, change: created|updated|deleted|invalid|restored, source: api|daemon|external|startup, revision|null, carried_from?}` (#46)
- `notice{kind, scope?, job?, run_id?, title}`. `kind`: `job_failed`, `job_finished`, `run_refused`, `job_invalid`, `settings_invalid`, `update_available`, `import_done`, `heartbeat_output`, `job_frontmatter_restored` (#46), `disk_low` (#44). The body (reply text, heartbeat output) is **not** included; the app links to the Run log or Turn.
- `session.changed{scope, launcher}`, `launcher.changed{scope}`, `launcher.tested{launcher, ok}` (#40), `settings.reloaded{changed_keys[]}`
- `device.paired{device_id}`, `device.revoked{device_id}` (each also raises an owner notice, #41), `import.finished{record_step}`
- No resume: the bus is a broadcast with no ring. After `gap` or a new `boot_id` the app re-GETs what it shows.

"Desktop notification when finished" stays in the app (#16 decision 8); the app filters `notice{kind: job_finished}` by its own per-job flag.

### 2.3 Turns (Chat, `send`, inject, compact)

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `turns.create` | `POST /v1/turns?wait=<s>` | `TurnRequest{scope = "global", text, attachments?: [AttachmentUpload{name, content_type, data_base64}], notify = false}` → `202 TurnAccepted{turn_id, scope, queue_position}`, or `200 TurnRecord` if it finished within `wait`; `507 insufficient_storage` for attachments below the disk floor (#44) | `send` (global only) | Chat ▸ composer (any Scope, with files); CLI `send` (`--file` is read and uploaded as `AttachmentUpload`); legacy `/api/inject` callers |
| `turns.get` | `GET /v1/turns/{turn_id}` | → `TurnRecord` | `admin`; a token for its own Turns | CLI `send --no-wait` follow-up; Chat after reconnect |
| `turns.events` | `GET /v1/turns/{turn_id}/events` **[SSE]** | `hello`, then `TurnEvent`s from the Turn's start (replayed), live until `outcome`, then the stream ends | `admin`; a token for its own Turns | Chat ▸ streaming reply and tool chips; CLI `send` and `sessions compact` progress on stderr |

- **Attachments**: at most 5, 10 MB each (the TS Web UI's limits). They are written to `inbox/api/` as `<turn_id>-<index><ext>` (#44) and handed to Claude as files, the same way Discord's are (#1 parity). Housekeeping deletes them 30 days after the Turn.
- `notify: true` also sends the reply to the owner through every adapter's `Notifier` (#22 `send --notify`). The Chat screen sends `false`: replies stay in the app (#10).
- **`TurnRecord`** = `{turn_id, scope, origin, principal, purpose: prompt|compact (#40), state: queued|running|done, outcome: ok|failed|refused|interrupted|null, text|null, compact: SessionCompacted|null (#40), session_id, launcher, run_ids[], created_at, finished_at, duration_ms}`. It matches #22's `send --json`.
- **`TurnEvent`** = `queued{position}` · `started{run_id, attempt, session: new|resumed, session_id, launcher}` · `text{delta}` · `tool{name, category, summary}` · `tool_result{name, ok}` · `agent{id, description, state: spawned|done}` · `retry{reason_kind, attempt, at}` · `refused{reason_kind}` · `outcome{record: TurnRecord}`.
- Finished Turns stay readable for 1 h, or the newest 256, whichever is more. After that, history is the session Transcript (§2.5) and the Run index (§2.9).
- There is no cancel route in v1: Ctrl-C in the CLI and cancel in the app Detach, and the Turn continues (#22, #40).

### 2.4 Jobs

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `jobs.list` | `GET /v1/jobs?agent=` | → `JobList{jobs: [JobSummary]}`, served from the Change scan's snapshot (#46) | `status:read` | Status & Jobs ▸ table; Agents ▸ jobs; CLI `jobs list` |
| `jobs.get` | `GET /v1/jobs/{job}` | → `JobDetail`, read fresh from disk (#46) | `admin` | Job editor (form and raw) |
| `jobs.create` | `POST /v1/jobs` | `JobCreate{name, agent?, fields: JobFields, prompt}` or `{name, agent?, raw}` → `201 JobDetail`; `409 job_exists`; `422 job_invalid` | `admin` | Job editor ▸ New job; Agents ▸ New job; CLI `jobs create` |
| `jobs.update` | `PATCH /v1/jobs/{job}` | `JobPatch{set: {<key>: <value>\|null}, prompt?}`, optional `If-Match` → `JobDetail`. Line edits (#12), validated first | `admin` | Job editor ▸ Save (form); Status ▸ enable toggle; CLI `jobs set`, `enable`, `disable` |
| `jobs.replace` | `PUT /v1/jobs/{job}/raw` | `JobRaw{raw}`, **`If-Match` required** → `JobDetail`; `422 job_invalid` | `admin` | Job editor ▸ Save (raw) |
| `jobs.delete` | `DELETE /v1/jobs/{job}` | optional `If-Match` → `JobDeleted{deleted, sessions_removed, state_removed}` (#12 §6) | `admin` | Job editor ▸ Delete; CLI `jobs delete` |
| `jobs.run` | `POST /v1/jobs/{job}/run?wait=<s>` | `{}` → `202 TurnAccepted` / `200 TurnRecord` | `jobs:run` | Status ▸ Run now; CLI `jobs run` (the explicit way to run a job immediately, #50) |
| `jobs.preview` | `POST /v1/schedule/preview` | `JobPreviewRequest{raw}` \| `{fields, prompt}` \| `{schedule}`, `count = 5` → `JobPreview{fields, prompt, raw, diagnostics[], next_fires[], timezone, notes[]}`. Writes nothing | `admin` (not audited) | Job editor ▸ cron builder's next-5 preview, never-match and day-field warnings; raw ↔ form toggle |
| `jobs.check` | `GET /v1/schedule/check?job=&next=5` | → `JobsCheck` (#22 `jobs check` shape) | `status:read` | Status ▸ problems; CLI `jobs check` reads disk and prints the same type |
| `jobs.audit` | `GET /v1/schedule/audit?since=&until=&late=` | → `JobsAudit` (#22 `jobs audit` shape, from the Run index, with `index: RunIndexState`, #44) | `status:read` | CLI `jobs audit` reads disk and prints the same type; available to the app |

- **`JobSummary`** = #22's `jobs list` record: `{name, agent, path, status: valid|invalid|warning, schedule, recurring, enabled, launcher, notify, model, next_at, last_run: {at, outcome}, in_flight, retry_at, revision}`.
- **`JobDetail`** = `JobSummary` + `{fields: JobFields, prompt, raw, diagnostics: [JobDiagnostic{severity, kind, line, column, message}], import_findings: [ImportFinding], next_fires[5]}`. `JobDiagnostic.kind` is stable (`schedule.invalid`, `key.unknown`, …, #28). `import_findings` are #17 §5's findings on the job's API record.
- **`JobFields`** = the frontmatter keys the Daemon parses (#12): `schedule`, `recurring`, `enabled`, `notify`, `model`, `timeout`, `retry`, `retry_delay`, `launcher`. Each is typed and optional; unknown keys are kept in `raw` and never dropped.
- **The preview is server-side** so it uses croner's OR day semantics and the Daemon's timezone (#12, #16 correction). The app never computes cron. A new or rescheduled job never catches up a slot that passed before its schedule took effect, so the preview's next fire is the next future slot (#50).
- **API edits survive the in-flight frontmatter guard.** Edits made outside the Daemon to a running job's frontmatter are restored, with a `job_frontmatter_restored` notice (#46).
- A job's Launcher override is its frontmatter `launcher:` key, edited here, not through `/v1/scopes/{scope}/launcher` (§2.5).
- Jobs are not renamed in v1. The name is the file stem, fixed at creation.

### 2.5 Scopes, Launchers and sessions

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `scopes.list` | `GET /v1/scopes?kind=` | → `ScopeList{scopes: [ScopeInfo]}` | `admin` | Chat ▸ scope picker and header (effective Launcher, resumed session); Launchers ▸ "Who uses which launcher" table |
| `launchers.list` | `GET /v1/launchers` | → `LauncherOverview{default, picker[], launchers: [LauncherInfo], scopes: [ScopeLauncher]}` (#22 `launcher list` + cards) | `admin` | Launchers ▸ cards with "used by" chips; Job editor ▸ Launcher picker; CLI `launcher list` |
| `launchers.set` | `PUT /v1/scopes/{scope}/launcher` | `LauncherSet{launcher: "direct"\|"<abs path>"}` → `LauncherSetResult{scope, launcher, effective, session: resumed\|new}`; `422 launcher_not_runnable`; `409 use_job_frontmatter` for `job:` Scopes | `admin` | Launchers ▸ set; Discord ▸ Edit channel ▸ Launcher; CLI `launcher set` |
| `launchers.unset` | `DELETE /v1/scopes/{scope}/launcher` | → `{scope, effective, inherited_from}`; `400 invalid_scope` for `global` (set `direct` instead) | `admin` | Launchers ▸ "inherit"; CLI `launcher unset` |
| `launchers.test` | `POST /v1/launchers/test` | `LauncherTestRequest{launcher?, scope = "global", timeout_s = 120}` (10–600; a request for a Launcher already under test joins it, #40) → `LauncherTestResult{launcher, scope, ok, checks: [LauncherCheck{id, ok, advisory, detail}], claude_code, duration_ms, tested_at}` | `admin` | Launchers ▸ Test launcher; CLI `launcher test` |
| `sessions.list` | `GET /v1/sessions?scope=&status=in_use\|parked\|orphaned` | → `SessionList{sessions: [SessionRecord]}` | `admin` | Sessions ▸ table (in use / parked filter); CLI `sessions list` |
| `sessions.get` | `GET /v1/sessions/{session_id}` | → `SessionRecord` + `context{used_tokens, window}\|null` (#39) | `admin` | Sessions ▸ detail; Chat ▸ header context meter (the Discord `/context` equivalent) |
| `sessions.messages` | `GET /v1/sessions/{session_id}/messages?limit=&before=` | → `TranscriptPage{messages: [TranscriptMessage{uuid, role, text, timestamp, tools[]}], next}`; `404 transcript_not_found` | `admin` | Chat ▸ history of the Scope's session; Sessions ▸ view |
| `sessions.reset` | `POST /v1/scopes/{scope}/sessions/reset` | `{launcher?}` (default: the current one) → `SessionReset{scope, launcher, backup}`; mirror included (rule 7) | `admin` | Sessions ▸ Clear (keeps a backup); CLI `sessions reset` |
| `sessions.compact` | `POST /v1/scopes/{scope}/sessions/compact?wait=<s>` | `{}` → `202 TurnAccepted` / `200 TurnRecord` (with `purpose: compact` and `compact: SessionCompacted{scope, launcher, ok, context_before, context_after}`). Queued on the Scope as a Turn; progress over `turns.events` (#40). The app never uses `?wait` | `admin` | Sessions ▸ Compact; CLI `sessions compact` (`?wait`, then `turns.events`; exit 124 on a wait timeout) |
| `sessions.delete` | `DELETE /v1/scopes/{scope}/sessions?launcher=` | → `{deleted}`; `409 session_in_use` (use reset) | `admin` | Sessions ▸ delete a Parked or orphaned row; CLI `sessions delete` |

- **`ScopeInfo`** = `{scope, kind: global|channel|thread|job|agent, label, parent, launcher: ScopeLauncher, session: {launcher, session_id, status}|null, queue: {running, depth}}`. Scopes listed: `global`, every Discord channel or thread with config, a session or an override, every job, every agent.
- **`ScopeLauncher`** = `{scope, set_here: "direct"|"<path>"|null, effective, inherited_from: <scope>|"default", source: settings|job_frontmatter|default}`. This is the scope → effective-Launcher table.
- **`LauncherInfo`** = `{launcher, builtin, runnable: {ok, reason: missing|not_file|not_executable|null}, last_test: {ok, tested_at, failed_checks[]}|null, used_by: [{scope, label, via: settings|job_frontmatter}], claude_code (#42)}`. The picker is `direct` + every path in use (#8).
- **`LauncherCheck.id`** is one of: `runnable`, `print_stream_json`, `clean_stdout`, `system_prompt`, `result_event` (the five fixed by #16 / #22), `prompt_stdin` (#25), `claude_code_version` (#42), `transcript` (#39, advisory) and `stragglers` (#37, a warning). An `advisory` check never makes `ok` false. The last result per Launcher is kept in `daemon/state/launcher-tests.json`, so the card can say "Last tested today 09:12".
- **`SessionRecord`** = #22's shape plus the prototype's columns: `{scope, scope_label, launcher, session_id, status: in_use|parked|orphaned, created_at, last_used_at, turns, first_message, prune_at, timeout_breaker (#32), auto_compact: {spent_at, last} (#33)}`. `first_message` is cut to 160 chars.
- **Transcripts** are read from `<config dir>/projects/<project dir>/<id>.jsonl`. Each run's Config dir is detected from `init.memory_paths`, with a lazy search plus the `transcript_roots` setting as fallback (#39), so Launchers that move Claude Code's config dir stay readable. A Transcript that still can't be read is `404 transcript_not_found` with `details.reason` and a hint, never an empty list. The context meter comes only from the Daemon's own stream, so it is `null` for imported sessions until their first Rust Turn (#39).

### 2.6 Settings

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `settings.get` | `GET /v1/settings` | → `SettingsView{revision, settings: Settings, ts_only_keys[], env_overrides[]}` | `admin` | Settings ▸ every "Daemon" section; Status ▸ Heartbeat card; CLI `config get` |
| `settings.update` | `PATCH /v1/settings` | `SettingsPatch` (JSON Merge Patch, RFC 7396, over the file's own key names), optional `If-Match` → `SettingsApplied{view: SettingsView, changed_keys[], restarted[]}` | `admin`; **[local]** for `remote.*` keys | Settings ▸ Save; Settings ▸ Discord ▸ Bot token ▸ Replace…; CLI `config set` / `unset` |

- **`Settings`** is a typed view of the keys the Rust Daemon reads (architecture §7, #23 D1): `timezone`, `heartbeat{enabled, interval, prompt, excludeWindows}`, `discord{token, allowedUserIds, listenChannels, listenGuilds, allowedGuilds, channelAllowedUserIds, channelNames, …}`, `security{level, allowedTools, disallowedTools}`, `model`, `fallback…`, `agentic{…}`, `session{autoRotate, …}`, `timeouts{…}`, `logging{…, daemonMaxTotalMiB, runLogKeepDays, auditKeepMonths}`, `remote{enabled, listen, port}`, `run_path{source, prepend}` (#26), `run_locale` (#43), `timeout_breaker{max_consecutive_timeouts, max_failing_seconds}` (#32), `auto_compact{enabled, timeout_secs}` (#33), `transcript_roots` (#39), `inbox{keepDays, maxMiB}` and `disk{warnFreeMiB, minFreeMiB}` (#44). Snowflakes are strings.
- **Secrets are write-only.** Every secret reads as `SecretState{set: bool}`. A patch sets one with a string and clears it with `null` (#13).
- **Refused keys**: TS-only keys (`web.*`, `runtime.launcher(s)`, `apiToken`, `watchdog.*`) give `422 ts_only_key` (#14 rules 2–3, #22, #32). The Rust `launcher` key gives `422 invalid_value` with a hint to use §2.5, so every Launcher write passes Launcher resolution. Unknown keys give `422 unknown_key`. Unknown keys **already in the file** are preserved and never shown.
- **`remote.*` changes are local-only.** A Device can't turn the remote listener off under itself, or move it.
- `env_overrides` lists keys an env var wins over (e.g. `DISCORD_TOKEN`), so the app can say "set by environment".
- `restarted` names what a change restarted (a new Discord token restarts only the adapter, architecture §7).

### 2.7 Discord

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `discord.get` | `GET /v1/discord` | → `DiscordOverview{state, since, bot: {id, name}, guilds: [{id, name}], dm_allowed_users: [DiscordUser], channels: [DiscordChannel]}` | `admin` | Discord screen; Chat picker labels; Copy ID |
| `discord.available` | `GET /v1/discord/available-channels` | → `{channels: [{id, name, guild_id, kind}]}` (from the gateway cache) | `admin` | Discord ▸ Listen in a channel… |
| `discord.channels.update` | `PUT /v1/discord/channels/{channel_id}` | `DiscordChannelPatch{trigger: mention\|every_message, allowed_user_ids: […]\|null}` → `DiscordChannel` | `admin` | Discord ▸ Listen in a channel…; Edit channel… |
| `discord.channels.remove` | `DELETE /v1/discord/channels/{channel_id}` | → `{channel_id, sessions_kept}`. Removes it from `listenChannels` and `channelAllowedUserIds`; sessions are untouched | `admin` | Discord ▸ Stop listening |
| `discord.hire` | `POST /v1/discord/channels/{channel_id}/hire` | `HireRequest{names: [string]}` → `HireResult{threads: [DiscordThread]}` | `admin` | Discord ▸ Hire into a new thread… |
| `discord.fire` | `POST /v1/discord/threads/{thread_id}/fire` | `{}` → `FireResult{thread_id, discord_deleted, sessions_removed, archived_to}` | `admin` | Discord ▸ Fire |

- These routes go through the adapter's `chat::AdapterControl` (`overview`, `available_channels`, `hire`, `fire`; architecture §6). Core does the session and `agents/` work; the adapter does the transport calls.
- **`DiscordChannel`** = `{id, name, guild_id, trigger, allowed_users: {source: global|channel, users: [DiscordUser]}, launcher: ScopeLauncher, threads: [DiscordThread]}`. **`DiscordThread`** = `{id, name, parent_id, agent_dir, last_active_at, session: {launcher, session_id, status}|null}`. **`DiscordUser`** = `{id, name|null}`.
- **Copy ID.** Every object carries its snowflake `id` as a string. The app shows names and copies the `id`; it never renders snowflakes inline (#16). The API doesn't hide them, because the CLI, automation and debugging need them.
- **Hire** does what the TS classifier's `hire` did: one public thread per name under the channel (auto-archive 3 days), a confirmation post, and an agent dir `agents/<slug>-<thread id>/`.
- **Fire** deletes the Discord thread and removes its session (as TS did), then **moves `agents/<slug>-<id>/` to `agents/.archive/<slug>-<id>-<UTC stamp>/`** (#16). The archived dir is listed and restorable in §2.8. The Discord-side classifier `fire` does the same, which is a `divergences.toml` entry citing #16 and #21 in [#18](https://github.com/Winter-wub/claudeclaw/issues/18)'s registry.

### 2.8 Agents

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `agents.list` | `GET /v1/agents` | → `AgentList{agents: [AgentInfo]}` | `admin` | Agents ▸ list and detail |
| `agents.archived` | `GET /v1/archived-agents` | → `ArchivedAgentList{archived: [ArchivedAgent{entry, name, archived_at, reason: fired\|archived}]}` | `admin` | Agents ▸ Archived (includes fired threads) |
| `agents.create` | `POST /v1/agents` | `AgentCreate{name, persona?}` → `201 AgentInfo`; `409 agent_exists` | `admin` | Agents ▸ New agent |
| `agents.persona.get` | `GET /v1/agents/{name}/persona` | → `AgentPersona{text, revision}` (`agents/<name>/CLAUDE.md`) | `admin` | Agents ▸ Edit persona (raw) |
| `agents.persona.put` | `PUT /v1/agents/{name}/persona` | `{text}`, **`If-Match` required** → `AgentPersona` | `admin` | Agents ▸ Edit persona ▸ Save |
| `agents.archive` | `POST /v1/agents/{name}/archive` | `{}` → `AgentArchived{entry, archived_to, jobs_unscheduled}` | `admin` | Agents ▸ Archive |
| `agents.restore` | `POST /v1/archived-agents/{entry}/restore` | `{}` → `AgentInfo`; `409 agent_exists` | `admin` | Agents ▸ Archived ▸ Restore (also for fired threads) |
| `agents.purge` | `DELETE /v1/archived-agents/{entry}` | → `{deleted}`. Only archived entries can be deleted permanently | `admin` | Agents ▸ Archived ▸ Delete permanently (#16) |

- **`AgentInfo`** = `{name, kind: named|thread, dir, thread: {id, channel_id}|null, description, jobs, launcher: ScopeLauncher, session: {launcher, session_id, status}|null}`. `description` is the persona's first paragraph, cut to 200 chars.
- Permanent deletion is reachable only from the archive, so nothing is deleted in one step. Housekeeping never touches `agents/.archive/` (#44).
- Archiving moves the agent's `jobs/` with it, so those jobs stop being scheduled. The response counts them. Sessions are kept and resume on restore.

### 2.9 Logs, runs and audit

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `logs.tail` | `GET /v1/logs/{source}?limit=&before=&min_level=&event=&scope=&run_id=&q=` | `source` = `worker` \| `watchdog` \| **`daemon`** → `LogPage{lines: [LogEntry], next, scan_truncated}` | `logs:read` | Logs ▸ Daemon tab (history); CLI `logs` |
| `logs.stream` | `GET /v1/logs/{source}/stream?backfill=&min_level=&event=&scope=&run_id=` **[SSE]** | `hello{boot_id, last_seq}`, `log` (`id: <boot_id>:<seq>`, `LogEntry`), `gap{reason, missed?}`; `Last-Event-ID` resume | `logs:read` | Logs ▸ Daemon tab (live); CLI `logs --follow` |
| `logs.summary` | `GET /v1/logs/summary` | → `LogSummary{level, since_boot: {error, warn}, dropped_lines, capped_today, disk: {…}}` | `logs:read` | Logs ▸ badges; Status |
| `logs.export` | `GET /v1/logs/export?since=&until=&level=&max_bytes=&map=&app_version=` | → `LogExport{manifest: ExportManifest, text, map?}`; `409 export_busy`; `500 export_leak` (#28) | `logs:read` (not audited) | App ▸ Bug-report export while the Daemon answers (#40); CLI `logs export` |
| `runs.list` | `GET /v1/runs?job=&scope=&origin=&outcome=&before=&limit=` | → `RunPage{runs: [RunRecord], next, index: RunIndexState}` (running runs first, `state: "running"`) | `logs:read` | Logs ▸ Run logs tab; Status ▸ active runs; CLI `logs --source runs` |
| `runs.get` | `GET /v1/runs/{run_id}` | → `RunRecord` | `logs:read` | Logs ▸ run detail; notification click-through |
| `runs.log` | `GET /v1/runs/{run_id}/log?tail_lines=` | → `text/markdown`, known-secret scrubbed | `logs:read` + `runs:content` | Logs ▸ run view; CLI `logs --run` |
| `audit.list` | `GET /v1/audit?before=&limit=&principal=&action=` | → `AuditPage{entries: [AuditEntry{id, at, principal, action, target, outcome}], next}` | `logs:read` | Logs ▸ Audit tab; CLI `logs --source audit` |

- **`source = daemon`** is Worker and Watchdog merged by timestamp, server-side. Its cursor encodes both positions. #22 made `daemon` the CLI's default source, and without this both the CLI and the app's TS code would have to merge two paginated feeds.
- **`logs.export`** runs the same `store::export` as `claudeclaw logs export` (architecture §4): one reviewable Markdown file with Pseudonyms, never Run logs, the audit log or settings values, and it fails closed. Defaults: 24 h, 10 MiB, maximum 24 MiB. Only one export runs at a time. When a local Daemon is down, the app runs the bundled `claudeclaw logs export --offline --out -` instead (§4). The app appends its own `## Desktop app` section from the App log behind a verify pass (#40).
- **`RunRecord`** is #23 D4's record, one per spawned run, plus these additive fields:
  - `cache_read_tokens`, `cache_write_tokens` (#21), so Usage can show cache hit rates (§2.10);
  - `launcher_resolved` (#26);
  - `purpose: turn | compact | compact_retry` (#33); every run in a Turn has its own record, sharing `turn_id`, with `attempt` counting up;
  - `settle{stragglers, outcome, pipe_held, settle_ms}` (#37);
  - `awake_ms`, `suspended_ms`, `wake_grace` (#38);
  - `cost_status: priced | no_baseline | no_usage | refused`, `cost_basis: list | managed | unknown | null`, `provider`, `api_key_source`, `models[]: {model, tokens_in, tokens_out, cache_read_tokens, cache_write_tokens, cost_usd, cost_basis}` (#39). `cost_usd` is per run by difference against the Cost baseline and may be `null`;
  - `claude_code_version` (#42);
  - `lingered` (#47); a lingered run is not a timeout for Auto-compact or the Timeout breaker;
  - `outcome` gains `unknown`, for `origin: ts` records only; a TS exit 124 back-fills as `timeout` (#44).
- **`LogSummary.disk`** gains `inbox_files`, `inbox_bytes`, `inbox_foreign`, `run_index_bytes`, `outbox_bytes`, `archive_bytes`, `import_backup_bytes`, `free_bytes` and `housekeeping{last_at, next_at, deleted, freed_bytes}` (#44).
- `AuditEntry.action` is the Op column; `principal` is `local`, `local-offline`, `device:<id>`, `token:<id>`, `system:import` or `system:housekeeping` (#44); `target` is an identifier (job name, Scope key, Device ID, a Housekeeping kind), never a value.

### 2.10 Usage and cost

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `usage.get` | `GET /v1/usage?from=&to=&tz=&group_by=scope\|launcher\|model\|job` | → `UsageReport` | `admin` | Usage & cost (this daemon); **Usage ▸ All daemons** (the app calls each daemon and merges) |

- **`UsageReport`** = `{from, to, tz, currency: "USD", totals: UsageTotals, days: [UsageDay{date, runs, tokens_in, tokens_out, cost_usd}], rows: [UsageRow{key, label, kind, launcher, runs, tokens_in, tokens_out, cache_read_tokens, cache_write_tokens, cache_hit_pct, cost_usd, cost_trust}], coverage: {index_from, runs_without_usage, runs_no_baseline, runs_unknown_price, index: RunIndexState}}` (#39, #44).
- **Source: the Run index**, not Transcripts. It covers jobs, chat, heartbeat and API Turns in one place. `cost_usd` comes from Claude Code's own `result` event **by difference of the session-cumulative figure** against the Cost baseline (`total_cost_usd` is session-cumulative on `--resume`, #39), not from a fixed price table (TS used fixed Sonnet pricing). Every sum skips `null` costs, and the `coverage` counts make the gap visible. `group_by=model` uses `RunRecord.models[]`, so subagent models are counted.
- **`cost_trust`** (`api_list` / `managed` / `estimate`) is computed server-side on each `UsageRow` and on the totals. Rows are never "corrected"; every non-`direct` Launcher shows `estimate` (#39).
- Runs back-filled from TS logs carry no tokens (`cost_status: no_usage`); they are counted in `coverage.runs_without_usage`, never shown as zero cost.
- **`tz` is chosen by the caller**, so the app can ask every daemon for the same day boundaries before merging them for "All daemons".
- **All daemons is app-side.** The API has no cross-daemon route (each daemon knows only itself). The app fans out, sums `days` and the `coverage` counts, concatenates `rows` with a daemon column, and takes the weakest `cost_trust`. A daemon that doesn't answer is listed as "not included" with its name. The total is never silently partial.

### 2.11 First-start import (#17)

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `import.get` | `GET /v1/import` | → `ImportRecord{kind: fresh\|imported, version, steps: [{step, done_at, result}], report, backup_dir, plan: ImportPlan\|null}` (`daemon/state/import.json` + last plan) | `admin` | Status ▸ "Imported 1.x data" notice and report link; Settings ▸ Import |
| `import.plan` | `GET /v1/import/plan?only=launchers\|token\|state\|settings\|jobs&job=` | → `ImportPlan` with `plan_hash`. Writes nothing | `admin` | Settings ▸ Import ▸ Preview (re-cutover after a rollback) |
| `import.apply` | `POST /v1/import/apply` | `ImportApplyRequest{only?, jobs?, plan_hash?}` → `ImportResult{record: ImportRecord, report, backup_dir}`; `409 plan_changed` when `plan_hash` no longer matches | `admin` | Settings ▸ Import ▸ Apply; CLI `import`, `jobs import` while the Daemon runs |

- `plan_hash` means "apply exactly what I previewed": if the data changed between preview and apply, the user sees a new plan instead of a surprise.
- `only=settings` is the renamed-settings step (`watchdog.*` → `timeout_breaker`, #32). `ImportPlan` also carries a `claude_code` section (`statusline: install|migrate|unchanged|leave_foreign|unreadable`, `state_json: remove|absent`, #24).
- Before the first start there is no Daemon. The app then runs the bundled `claudeclaw import --dir <d> --dry-run --json` (#17 §7, #22), which prints the same `api::ImportPlan`.

### 2.12 Pairing, Devices and API tokens (#13)

| Op | Route | Request → Response | Who | Used by |
|---|---|---|---|---|
| `pairing.create` | `POST /v1/pairing` | `PairingCreate{host?}` (a hostname or IP literal; anything else is `422 invalid_host`, #41) → `PairingStart{pairing_id, code, expires_at, fingerprint, fingerprint_curl, listen[], pairing_string}`; `409 remote_disabled` | **[local]** | Settings ▸ Remote access ▸ Pair a device (local daemon); CLI `pair [--host <name>]` |
| `pairing.get` | `GET /v1/pairing/{pairing_id}?wait=<s≤60>` | → `PairingState{state: pending\|completed\|expired\|cancelled\|burned, device: DeviceInfo\|null}` (long-poll) | **[local]** | Same screen, waiting; CLI `pair` (loops until done or 10 min) |
| `pairing.cancel` | `DELETE /v1/pairing/{pairing_id}` | → `{cancelled}` | **[local]** | Pair ▸ Cancel; CLI `pair` Ctrl-C |
| `pair` | `POST /v1/pair` | `PairRequest{code, label, platform}` over TLS with the new Device cert → `PairResult{device: DeviceInfo, daemon: {name, version, fingerprint}}`; `401 pairing_code_invalid`; `410 pairing_expired` | `Pairing` (remote listener only) | App ▸ Add remote daemon (Pairing string, or host + code) |
| `devices.list` | `GET /v1/devices` | → `DeviceList{devices: [DeviceInfo{id, label, fingerprint, created_at, last_seen_at, connected, current}]}` | `admin` | Settings ▸ Paired devices; CLI `devices list` |
| `devices.update` | `PATCH /v1/devices/{device_id}` | `{label}` → `DeviceInfo` | `admin` | Paired devices ▸ Rename; CLI `devices rename` |
| `devices.revoke` | `DELETE /v1/devices/{device_id}` | → `{revoked, disconnected}` | `admin` | Paired devices ▸ Revoke; CLI `devices revoke` |
| `tokens.list` | `GET /v1/tokens` | → `TokenList{tokens: [ApiTokenInfo{id, name, allow[], created_at, last_used_at, legacy, source}]}` | `admin` | Settings ▸ API tokens; CLI `tokens list` |
| `tokens.create` | `POST /v1/tokens` | `TokenCreate{name, allow: [TokenPermission]}` → `201 TokenCreated{token: ApiTokenInfo, secret}`. `secret` appears only here | `admin` | Settings ▸ API tokens ▸ New; CLI `tokens create` |
| `tokens.revoke` | `DELETE /v1/tokens/{token_id}` | → `{revoked}` | `admin` | Settings ▸ API tokens ▸ Revoke; CLI `tokens revoke` |

- **Pairing codes** (#41) are 8 Crockford base32 characters, shown as `XXXX-XXXX`, valid for 10 minutes, and created only locally. Headless pairing goes over SSH to the Daemon's host.
- **Pairing string** (#41): `pairing_string` is `ccpair1.<code>.<fingerprint>.<port>[.<host>]`. The app parses it with the shared `api::PairingString` parser and pins the key before it sends the code, so no fingerprint is compared by eye. A pin mismatch is an app-side dialog error that sends nothing; it is not a Daemon state. Typing host + code instead stays trust-on-first-use with a visible fingerprint. CLI: `pair --no-wait --json` prints the full `PairingStart`, and the human-readable output ends with the Pairing string on its own line.
- **Brute force.** A Pairing code is burned after 5 wrong attempts in total (`state: burned`), on top of its 10-minute life. Failures are logged as `pairing.failed` and `api.auth_failed`.
- **Notices** (#41). Every `device.paired` and `device.revoked` raises an owner notice through `core::notify`, besides the `DaemonEvent`. Notices and fast revocation are the real control, because any admin Device can already get a code through a Turn.
- **Revoked Devices are kept as tombstones** (SPKI + `revoked_at`) in `devices.json`. A revoked cert then gets `401 device_revoked`, which #20's `revoked` state needs, rather than `unknown_device`.
- **The legacy token** (`legacy: true`, `source: "settings.apiToken"`) can be revoked here. The revocation is kept by hash, so #17's "kept in step" rule doesn't re-import the same value on the next settings load. It re-imports only if the value in `settings.json` changes.
- `current: true` marks the calling Device.

---

## 3. Screen and command coverage

Every v1 screen from #10 / #16 and every CLI command from #22 maps to routes above. Nothing a screen shows comes from somewhere else, except the app-side items in §4.

| Screen (#16) | Reads | Writes |
|---|---|---|
| **Daemon switcher, tray, banner** | `health.get`, `status.get`, `events.stream` | `daemon.stop` (remote and API-answering local); local Start and Attach are app-side |
| **Status & Jobs** | `status.get` (cards, heartbeat, active runs, crash timeline from `supervisor.crashes`), `jobs.list`, `jobs.check`, `import.get`, `runs.list?state=running` | `jobs.update` (enable), `jobs.run`, `daemon.restart`, `daemon.stop` |
| **Job editor** | `jobs.get`, `launchers.list` (picker), `jobs.preview` | `jobs.create`, `jobs.update`, `jobs.replace`, `jobs.delete` |
| **Chat** | `scopes.list` (picker and header), `sessions.get` (context), `sessions.messages` (history) | `turns.create` + `turns.events` |
| **Sessions** | `sessions.list`, `sessions.get`, `sessions.messages` | `sessions.reset`, `sessions.compact` (+ `turns.events`), `sessions.delete` |
| **Discord** | `discord.get`, `discord.available` | `discord.channels.update` / `.remove`, `discord.hire`, `discord.fire`, `launchers.set` / `.unset` |
| **Agents** | `agents.list`, `agents.archived`, `agents.persona.get`, `jobs.list?agent=` | `agents.create`, `agents.persona.put`, `agents.archive`, `agents.restore`, `agents.purge` |
| **Logs** (per daemon) | Daemon: `logs.tail` + `logs.stream` (`source=daemon`), `logs.summary`; Run logs: `runs.list`, `runs.get`, `runs.log`; Audit: `audit.list`; Bug-report export: `logs.export` | — |
| **Usage & cost** | `usage.get` (this daemon; All daemons = fan-out) | — |
| **Launchers** | `launchers.list`, `scopes.list` | `launchers.set`, `launchers.unset`, `launchers.test` |
| **Settings ▸ Daemon** | `settings.get`, `status.get` (`remote`, fingerprint, `run_path`, `run_locale`), `update.get`, `devices.list`, `tokens.list`, `import.get` | `settings.update`, `update.check`, `pairing.*` (local), `devices.update` / `.revoke`, `tokens.create` / `.revoke`, `import.plan` / `.apply` |
| **OS notifications** | `events.stream` (`notice`, `status.changed`), plus #20's offline detection | — |

| CLI command (#22) | Route(s) when the Daemon answers |
|---|---|
| `start -d`, `restart` (wait) | `health.get` |
| `stop` / `restart` | `daemon.stop` / `daemon.restart` |
| `update` (re-exec) | `daemon.reexec` **[local]** |
| `status` | `status.get` |
| `statusline` | `status.get` (offline fallback, #24) |
| `send` | `turns.create` + `turns.events` (or `?wait`) |
| `jobs list` / `check` / `audit` | `jobs.list` / `jobs.check` / `jobs.audit` (the last two always read disk; same types) |
| `jobs create` / `set` / `enable` / `disable` / `delete` / `run` | `jobs.create` / `jobs.update` ×3 / `jobs.delete` / `jobs.run` |
| `import`, `jobs import` | `import.plan` + `import.apply` (dry runs read disk) |
| `launcher list` / `set` / `unset` / `test` | `launchers.list` / `launchers.set` / `launchers.unset` / `launchers.test` |
| `sessions list` / `reset` / `compact` / `delete` | `sessions.list` / `sessions.reset` / `sessions.compact` (+ `turns.events`) / `sessions.delete` |
| `config get` / `set` / `unset` | `settings.get` / `settings.update` |
| `pair` | `pairing.create` → `pairing.get?wait` loop → `pairing.cancel` on Ctrl-C |
| `devices list` / `rename` / `revoke` | `devices.list` / `devices.update` / `devices.revoke` |
| `tokens list` / `create` / `revoke` | `tokens.list` / `tokens.create` / `tokens.revoke` |
| `logs [--source …] [--follow]`, `logs --run` | `logs.tail` / `logs.stream`, `audit.list`, `runs.list`, `runs.log` |
| `logs export` | `logs.export` (`--offline` runs `store::export` directly) |
| `housekeeping [--dry-run]` | `housekeeping.run` (an Offline write with no Daemon) |

The issue's own list is covered: Test launcher with its checks (`launchers.test`), audit log (`audit.list`), crash history (`status.get` → `supervisor.crashes`), per-(Scope, Launcher) sessions with in-use / parked status (`sessions.list`), the scope → effective-Launcher table (`scopes.list`, `launchers.list`), thread Hire/Fire (`discord.hire`, `discord.fire`), and the next-N-runs preview (`jobs.preview`, `JobDetail.next_fires`).

**Count:** 72 operations (#21's 70, plus `housekeeping.run` from #44 and `logs.export` from #28), 3 SSE families, 4 local-only routes (`daemon.reexec`, `pairing.create`, `pairing.get`, `pairing.cancel`) plus the local-only `remote.*` keys in `settings.update`, and 1 remote-only route (`pair`).

## 4. Not in the API (and why)

| Thing | Where it lives |
|---|---|
| Attach, per-daemon start at login, the daemon list, Start of a local daemon, "Restart anyway" / Clear give-up, Force restart | App process duties through the bundled binary (#9, #16, #20 D6, #45); `DaemonReachability` is computed client-side by `client::offline_status` |
| Offline Daemon logs and the offline Bug-report export | Two fixed-argv commands on the bundled binary, the only read paths outside the API: `claudeclaw logs --offline --json` (#20 D3) and `claudeclaw logs export --offline --out -` (#28, #40) |
| Give-up and Held-start state | `supervisor.json`, read offline; a gave-up Daemon has no API (#45) |
| Pre-start import preview | `claudeclaw import --dry-run --json` (#17 §7) |
| "Notify when finished" per job, Pause notifications, theme, language, window behaviour | App settings (#16) |
| Last-known snapshot | App cache (#20 D4) |
| The App log | The app's own `app_log_dir()`; it joins the Bug-report export app-side (#40) |
| Cross-daemon aggregation | App fan-out (§2.10) |
| Applying an update to the binary | CLI `update` or the Desktop app's updater; the API only checks and re-execs |
| Job rename, cancelling a Turn, a "run heartbeat now" button | Not in v1; each can be added later without breaking anything here |
| TS Web UI `/api/technical-info` | Covered by `status.get` + `settings.get` (redacted) |
| TS Web UI `/api/jobs/quick` | Covered by `jobs.create` |

## 5. Types added to `claudeclaw-api`

Beyond those already named by #11, #20, #22 and #23 (`SupervisorStatus`, `DaemonReachability`, `DaemonEvent`, `LogEntry`, `RunRecord`, `ImportPlan`, `JobsCheck`, `JobsAudit`, the status type), this doc names:

`ApiError`, `ErrorCode`, `TokenPermission`, `Deadline`, `Health`, `DaemonStatus`, `Problem`, `RunLocale`, `RunIndexState`, `StopAccepted`, `RestartAccepted`, `ReexecRequest`, `ReexecAccepted`, `UpdateInfo`, `HousekeepingReport`, `TurnRequest`, `AttachmentUpload`, `TurnAccepted`, `TurnRecord`, `TurnEvent`, `JobList`, `JobSummary`, `JobDetail`, `JobFields`, `JobCreate`, `JobPatch`, `JobRaw`, `JobDeleted`, `JobDiagnostic`, `ImportFinding`, `JobPreviewRequest`, `JobPreview`, `ScopeList`, `ScopeInfo`, `ScopeLauncher`, `LauncherOverview`, `LauncherInfo`, `LauncherSet`, `LauncherSetResult`, `LauncherTestRequest`, `LauncherTestResult`, `LauncherCheck`, `SessionList`, `SessionRecord`, `SessionReset`, `SessionCompacted`, `TranscriptPage`, `TranscriptMessage`, `SettingsView`, `Settings`, `SecretState`, `SettingsPatch`, `SettingsApplied`, `DiscordOverview`, `DiscordChannel`, `DiscordChannelPatch`, `DiscordThread`, `DiscordUser`, `HireRequest`, `HireResult`, `FireResult`, `AgentList`, `AgentInfo`, `ArchivedAgentList`, `ArchivedAgent`, `AgentCreate`, `AgentPersona`, `AgentArchived`, `LogPage`, `LogSummary`, `LogExport`, `ExportManifest`, `ExportSection`, `RunPage`, `AuditPage`, `AuditEntry`, `UsageReport`, `UsageTotals`, `UsageDay`, `UsageRow`, `ImportRecord`, `ImportApplyRequest`, `ImportResult`, `PairingCreate`, `PairingStart`, `PairingState`, `PairingString`, `PairRequest`, `PairResult`, `DeviceList`, `DeviceInfo`, `TokenList`, `ApiTokenInfo`, `TokenCreate`, `TokenCreated`, plus `UiLogEvent` and the `log_event::app::*` catalogue for the App log (#40).

A test in `claudeclaw-server` asserts that the router's `(method, path)` set equals the route constants in `api`, and that every route has a `Deadline`, so neither can grow without the other. `cargo xtask openapi --check` already keeps `openapi.json` current (architecture §4).

## 6. What this list changed in other specs

#21 needed changes outside this list. They are now part of those specs, so this section only points to them:

1. **Control pipe** `restart`, `reexec(path)` and `reexec_result(ok | err)`: [architecture §4](architecture.md#claudeclaw-binary).
2. **Chat seam** `chat::AdapterControl` (`overview`, `available_channels`, `hire`, `fire`): [architecture §6](architecture.md#6-chat-adapter-seam-discord-now-telegram-and-slack-later).
3. **Layout** `inbox/api/`, `agents/.archive/`, `daemon/state/launcher-tests.json`, and revocation tombstones in `devices.json` / `api-tokens.json`: [architecture §9.1](architecture.md#91-per-project-dir-one-daemon-per-dir-9).
4. **Logging (#23 D5)**: `source=daemon` merged server-side and the cache-token fields on `RunRecord` (§2.9).
5. **Parity (#18)**: the Discord classifier's `fire` now archives `agents/<slug>-<id>/` instead of leaving it; a `divergences.toml` entry citing #16 and #21.
6. **CLI (#22)**: Token permissions are final as in §1.3 (no rename). `pair` uses `pairing.create` + `pairing.get?wait` + `pairing.cancel`. `send --file` uploads the file as `AttachmentUpload`.

## 7. Decisions and alternatives

| # | Decision | Alternatives rejected |
|---|---|---|
| D1 | **Resource routes with action sub-paths** (`POST /v1/jobs/{job}/run`), `operationId` = audit action | *RPC-style `POST /v1/rpc/<method>`*: loses HTTP caching and status semantics, and OpenAPI tooling gets one opaque route. *Google-style `:run` suffixes*: matchit treats `:` specially in patterns, which is friction for no gain. *Separate audit action names*: a second vocabulary to keep in sync with the routes. |
| D2 | **Five Token permissions**: `send`, `jobs:run`, `status:read`, `logs:read`, `runs:content`; everything else admin-only | *`<resource>:<verb>` for every route* (`jobs:write`, `settings:write`…): no automation use case asks for it, and each permission is a security surface to test. More can be added in v1 without breaking. *Renaming `send` to `turns:send`*: the glossary and #22 already use `send`, and the legacy token's meaning is "send". *A per-Scope `send`*: useful later, but it makes a permission carry a parameter; v1 keeps `send` on `global` (TS's inject). |
| D3 | **Local-only: re-exec, Pairing codes, `remote.*` keys** | *Everything open to Devices* (#13 makes them full admin): a leaked Device key could then mint more Devices that survive its own revocation, aim the re-exec at another binary, or move the listener. All three are things a person does at the machine anyway. *Making Launcher and token writes local-only too*: job prompts already let Claude run commands on the host, so a Device setting a Launcher path is no escalation, and remote daemons need token management from the app. #41 kept codes local-only and added the Pairing string and owner notices instead of Device-minted codes. |
| D4 | **`revision` + `If-Match`; required for whole-file writes, optional for key-level ones** | *Last writer wins*: the app silently overwrites an edit made in an editor or by another Device, a silent failure. *Required everywhere*: CLI line edits would need a read first for no benefit, because they touch one key over a fresh read. *mtime-based*: coarse on some filesystems and changed by `touch`. |
| D5 | **Turns are two-step (POST → 202, then GET SSE that replays from the start), plus `?wait`**; `sessions.compact` uses the same shape (#40) | *POST that answers with SSE* (TS `/api/chat`): the Tauri bridge's `api_subscribe` is a GET, CLI `--no-wait` and detach need an ID first, and a dropped connection would lose the reply. *WebSocket*: #13 fixed SSE. *Only async*: `curl` automation and the legacy inject caller want one request. *A synchronous compact*: a manual compact can take up to an hour, far past any Deadline (#40). |
| D6 | **Attachments as base64 inside `TurnRequest`** (5 × 10 MB) | *multipart/form-data*: `openapi-fetch` and the `api_call(…, body)` bridge are JSON; multipart needs a second body path through Tauri. *A separate uploads resource*: two calls and orphan cleanup for files that are only ever used by one Turn. The 33 % overhead is fine at 10 MB. |
| D7 | **One content-free global event bus (`/v1/events`) with no resume**, plus per-Turn and log streams | *Per-screen streams*: many more routes and subscriptions for the same bus. *Resume with a ring*: events are hints to re-fetch, so after a gap re-fetching is correct and simpler. *Content in notices*: would leak reply text to `status:read` tokens and into the app's notification centre. |
| D8 | **Server-side job preview and cron** (`jobs.preview`) | *Cron in the app's TS*: a second cron implementation that must match croner, which is exactly the drift #16's AND/OR correction showed. |
| D9 | **Scope-centric writes** (`/v1/scopes/{scope}/launcher`, `/…/sessions/reset`) and **session-ID reads** (`/v1/sessions/{session_id}/messages`) | *Launcher in the path*: Launchers are absolute paths. *Everything under `/v1/sessions/{x}`*: a scope key and a session UUID would compete for one segment. *Job Launchers through the scope route*: two places to set one value, so `job:` Scopes answer `409 use_job_frontmatter`. |
| D10 | **Settings as JSON Merge Patch over the file's own key names** | *A route per section* (TS's `/api/settings/heartbeat`): many routes, and the CLI's `config set a.b` has to know which. *JSON Patch (RFC 6902)*: harder to write by hand and to validate key by key. *snake_case translation*: a mapping table to maintain, and `config set` paths would differ from the file. |
| D11 | **Usage from the Run index, with a caller-chosen `tz`; "All daemons" merged in the app**; per-run cost by difference against the Cost baseline (#39) | *Transcripts in the Config dir* (TS): misses Launchers that move the config dir, needs a price table, and is slow on big dirs. *Summing `total_cost_usd`*: it is session-cumulative on `--resume`, and 98.6 % of Zero's runs are resumed, so it would overstate ~447× (#39). *A cross-daemon API route*: a daemon would need credentials for other daemons. *Daemon-local day boundaries*: two daemons in different zones couldn't be summed. |
| D12 | **Archive first, purge from the archive only** (agents and fired threads) | *Delete on fire* (TS removed only the session and left the dir): #16 chose archive. *A `?permanent=true` flag on one delete route*: one click from data loss. |
| D13 | **`source=daemon` merged server-side** | *Merging in the client*: the CLI (Rust) and the app (TS) would each merge two paginated, cursor-based feeds. |
| D14 | **Revocation tombstones** for Devices and the legacy token | *Delete the record*: a revoked Device would read as unknown (no `device_revoked` for #20), and #17's sync would re-import a revoked legacy token on the next settings load. |
| D15 | **Pairing code burned after 5 wrong attempts** | *Only the 10-minute expiry*: a short code is guessable within 10 minutes on an open port. *Per-IP limits only*: trivial to spread over addresses. |

## 8. Review points

1. **Five Token permissions and admin-only for the rest** (D2). In particular, `send` reaches only the `global` Scope, and there is no `usage:read`.
2. **The local-only set** (D3): re-exec, Pairing codes and `remote.*`. A remote Device can't pair another Device, so adding a second laptop to a headless daemon needs SSH and `claudeclaw pair` there; #41 made that path paste-and-pin with the Pairing string. Device roles that withhold `turns.create` / `launchers.set` remain the only real boundary, deferred since #13.
3. **Fire archives, and so does the Discord classifier's fire** (§2.7, §6 item 5). That is a behaviour change from TS for the chat path, not only for the app.
4. **Attachments inline as base64 with a 64 MiB body limit** on one route (D6).
5. ~~**Usage cost comes from Claude Code's `result` event.**~~ Resolved by #39: cost is computed by difference against the Cost baseline and labelled with `cost_trust`; pre-cutover TS history still has no usage.
6. **Two new seams in the architecture** (§6 items 1 and 2): control-pipe `restart` / `reexec`, and `chat::AdapterControl`.
7. **No job rename and no Turn cancel in v1** (§4).
8. **`{job}` with `%2F` for agent jobs** (§1.7). The alternative is `/v1/agents/{agent}/jobs/{label}` duplicates of every job route.
9. ~~**Transcripts unreadable for Launchers that move Claude Code's config dir.**~~ Resolved by #39: the Config dir is detected per run, with `transcript_roots` as a fallback.
10. **`beacons[]` instead of #27's `health[]`** (§2.1). #27 named the stall-monitor list `health`, which would collide with `DaemonStatus.health: ok|degraded` from #22. The list is named `beacons` here; renaming either is a one-line change before v2.0.0.

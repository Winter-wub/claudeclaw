# ClaudeClaw 2.x architecture and crate picks

Resolves [#11 "Lock the architecture and crate picks"](https://github.com/Winter-wub/claudeclaw/issues/11) on the wayfinder map [#2](https://github.com/Winter-wub/claudeclaw/issues/2).

> **Delegated decision.** An agent made this on 2026-10-13 because the maintainer asked to "continue without asking". It is open to revision. The [Review points](#13-review-points) list the choices most likely to need a second look.

Terms follow `GLOSSARY.md`: Daemon, Watchdog, Worker, Launcher, Scope, Plugin shim, Desktop app, Management API, Device, API token, Shadow instance, Recorder launcher.

**Inputs this doc must honour:**

| Ticket | Decision |
|---|---|
| [#4](https://github.com/Winter-wub/claudeclaw/issues/4) | Crate survey. Tokio, twilight 0.17 (serenity as fallback), croner 4, process-wrap, axum 0.8 with SSE, serde-saphyr, tokio signals. |
| [#7](https://github.com/Winter-wub/claudeclaw/issues/7) | Code layout. A cargo workspace on `main`, beside the TS code. |
| [#8](https://github.com/Winter-wub/claudeclaw/issues/8) | Launcher model. A Launcher is just a path. Sessions are keyed by (Scope, Launcher). |
| [#9](https://github.com/Winter-wub/claudeclaw/issues/9) | Process model. Watchdog + Worker, a file lock, a timer-based scheduler and per-job state. |
| [#10](https://github.com/Winter-wub/claudeclaw/issues/10) | Desktop app. Tauri 2 with React/TS. |
| [#12](https://github.com/Winter-wub/claudeclaw/issues/12) | Job quirks. Standard cron, strict YAML, a Job import, and a `ts_compat` oracle. |
| [#13](https://github.com/Winter-wub/claudeclaw/issues/13) | Management API. HTTP/JSON + SSE, a local socket, pinned TLS, Devices, API tokens and an audit log. |
| [#14](https://github.com/Winter-wub/claudeclaw/issues/14) | Cutover. 10 rollback-compat rules on the state layout. |

**Reference material.** The TS contract lives on `research/ts-daemon-contract`. The crate and launcher surveys live on `research/rust-crates` and `research/claude-code-launchers`.

---

## 1. Shape at a glance

```
                       ┌────────────── claudeclaw (one binary) ──────────────┐
 Desktop app ──┐       │ Watchdog (parent)                                   │
 (Tauri)       │       │   holds daemon.lock, writes daemon.pid,             │
 CLI ──────────┼─ API ─┼─► restarts / re-execs ─► Worker (child)             │
 automation ───┘  │    │                            ├─ Management API server │
                  │    │                            ├─ Scheduler + heartbeat │
   local: api.sock / named pipe                     ├─ Conversations (queues per Scope)
   remote: TLS, both keys pinned                    ├─ Chat adapters (Discord; later Telegram, Slack)
                       │                            └─ Runner ─► Launcher ─► claude
                       └─────────────────────────────────────────────────────┘
                                         │ reads/writes
                         <project-dir>/.claude/claudeclaw/  (TS-shared files + daemon/ Rust-only dir)
```

- **One Rust workspace** builds two shipped artefacts:
  - the `claudeclaw` binary, which is the CLI, the Watchdog and the Worker, chosen by subcommand;
  - the Desktop app, which bundles that binary.
- **The Worker is the only process that runs Claude Code.**
- **The Desktop app links only the API types and the API client.** It never links the Daemon's state code, so it cannot write Daemon files even by accident (#10).

## 2. Toolchain and workspace conventions

| Item | Decision |
|---|---|
| Edition | 2024 |
| MSRV (`workspace.package.rust-version`) | **1.90**. Floor set by Tauri 2.12 (1.90); twilight 0.17, serde-saphyr 1.3 and uuid 1.28 need 1.89. |
| Pinned toolchain (`rust-toolchain.toml`) | A specific stable, bumped by PR. **1.95.0** at the time of writing, which is what the probe in §12 used. |
| Async runtime | **tokio 1.53**. The Worker is multi-thread. The Watchdog and CLI use `current_thread`. |
| Errors | `thiserror` in libraries, `anyhow` only in the binary and `xtask`. |
| Logging | `tracing` everywhere. No `println!` outside CLI output. |
| Dependencies | Declared once in `[workspace.dependencies]`. Crates use `dep.workspace = true`. |
| Default members | Every crate **except** `claudeclaw-desktop`, so a root `cargo build`/`cargo test` never needs WebKitGTK or Node. |
| TLS crypto provider | **ring**, everywhere. Installed once at process start with `rustls::crypto::ring::default_provider().install_default()`. Reqwest panics without it (seen in the probe). |
| Async traits | Native `async fn` in traits for static dispatch. Traits used as `dyn` return handles or `BoxFuture`. No `async-trait` crate. |

## 3. Workspace layout

```
Cargo.toml                    workspace, [workspace.dependencies], default-members
rust-toolchain.toml
crates/
  claudeclaw/                 bin "claudeclaw": CLI, Watchdog, Worker composition root
  api/                        claudeclaw-api: Management API wire types, SSE event types, openapi.json
  store/                      claudeclaw-store: on-disk layout, atomic IO, settings, sessions, job state, logs, lock/pid
  jobs/                       claudeclaw-jobs: job files (envelope, YAML, value grammar, line edits), cron
  runner/                     claudeclaw-runner: Launcher checks, argv, spawn/kill, stream-json parser, result judging
  scheduler/                  claudeclaw-scheduler: timers, catch-up, in-flight guard, retries, heartbeat
  chat/                       claudeclaw-chat: the chat-adapter seam (traits + types, no transport deps)
  core/                       claudeclaw-core: Scopes, Launcher resolution, sessions, Conversations, notifications
  discord/                    claudeclaw-discord: Discord adapter (twilight)
  server/                     claudeclaw-server: Management API server (axum), auth, pairing, audit, SSE
  client/                     claudeclaw-client: Management API client (socket / pipe / pinned TLS)
  ts-compat/                  claudeclaw-ts-compat: TS cron + frontmatter oracle, Job import, 1.x divergence lint
  desktop/                    claudeclaw-desktop: Tauri 2 backend (Cargo.toml, tauri.conf.json, src/)
    ui/                       React + TS + Vite frontend; src/api/schema.d.ts generated from openapi.json
xtask/                        cargo xtask: openapi export/check, TS type generation check, dependency-rule check
examples/launchers/           ccs, ccr, record (Recorder launcher), per #8 and #14
.claude-plugin/ commands/ skills/   Plugin shim (replaces the TS plugin at cutover, #7)
Formula/ Casks/               Homebrew tap (#7)
src/                          TS daemon, frozen to bug fixes until main switches
```

Package names are prefixed `claudeclaw-`. Directory names are short. Crate names follow the glossary: there is no `bot`, `service` or `supervisor` crate.

### 3.1 Dependency rules

The arrows mean "may depend on". Anything not listed is forbidden, and `cargo xtask deps` fails CI on a forbidden edge.

```
api        → (nothing in the workspace)
store      → api
jobs       → (nothing in the workspace)
runner     → (nothing in the workspace)
chat       → (nothing in the workspace)
scheduler  → jobs, store
core       → store, jobs, runner, scheduler, chat, api
discord    → chat
server     → core, api
client     → api
ts-compat  → jobs, store
claudeclaw → everything above except desktop
desktop    → client, api
```

What these rules buy:
- **Adapters see only `chat`.** A Discord bug cannot reach into sessions or the store, and Telegram and Slack plug in against the same narrow seam.
- **`desktop` sees only `client` and `api`.** The Desktop app can't touch Daemon files (#10).
- **Nothing depends on `ts-compat`.** Only the binary registers it, so deleting it is one directory and three lines (§8).
- **`runner`, `jobs` and `chat` are leaves.** They are where the parity fixtures of [#18](https://github.com/Winter-wub/claudeclaw/issues/18) attach without a Daemon.

## 4. Crate boundaries

### `claudeclaw` (binary)

- **Owns:**
  - the `clap` CLI;
  - the Watchdog role (`claudeclaw start`; a bare `claudeclaw` also starts);
  - the Worker role (a hidden `claudeclaw worker`, spawned only by the Watchdog);
  - the composition root: build `core`, register adapters and `ts-compat`, start `server`;
  - self-update.
- **Watchdog.** It stays deliberately small, matching the glossary ("only starts, restarts and re-executes the Worker"):
  - **Lock and pid.** It takes `daemon.lock` with `std::fs::File::try_lock` (stable since 1.89; the probe showed a second lock is refused). It checks and then writes `daemon.pid` (#14 rule 10).
  - **Control pipe.** It spawns the Worker with stdin/stdout pipes that carry line-delimited JSON. The Worker reports `ready`, `stopping(intentional)` and `fatal(reason)`. The Watchdog sends `shutdown(reason)`.
  - **Orphan protection.** EOF on the Worker's stdin means the Watchdog died, and the Worker shuts down gracefully. This keeps a Worker from being orphaned on any OS.
  - **Status file.** It writes `daemon/state/supervisor.json` (state, Worker pid, recent crash times, last exit, given-up flag) on every transition.
  - **Restarts.** It applies the #9 backoff (1s → 60s; more than 5 crashes in 10 min means it gives up).
  - **Give-up notice.** On give-up it DMs the owner through `claudeclaw-discord`'s REST-only `notify_owner` (no gateway, no Claude) and exits non-zero.
  - **Update.** On update it re-execs the new binary with the lock fd's `FD_CLOEXEC` cleared, so the lock is never released (Unix). On Windows it spawns the new binary, which waits up to 30 s for the lock, and then exits.
- **Exit codes:**

  | Code | Meaning |
  |---|---|
  | 0 | Stopped through the control channel |
  | 75 (`EX_TEMPFAIL`) | External signal or recoverable failure, so restart |
  | 70 (`EX_SOFTWARE`) | Watchdog gave up |
  | 73 (`EX_CANTCREAT`) | Lock held by another Daemon. Prints its pid. |

### `claudeclaw-api`

- **Owns:**
  - every Management API request/response type, and the SSE event enum (`DaemonEvent`);
  - the error envelope, the route path constants and the `Scope` wire form;
  - `supervisor.json`'s shape, because the client reads it for a stopped local Daemon (see `client`).
- Types derive `serde` + `utoipa::ToSchema`.
- `crates/api/openapi.json` is generated and committed. `cargo xtask openapi --check` fails CI if it is stale. Snowflakes are `String` on the wire.
- **No** tokio, axum or IO.

### `claudeclaw-store`

The only crate that knows paths under `<project-dir>`. It is **synchronous** (`std::fs`, small files), and async callers wrap it in `spawn_blocking`. It owns:
- **`Layout`**: every path in §9, plus the socket-path rule.
- **Atomic writes**: `tempfile::NamedTempFile::persist` in the same dir, then `fsync`. The permission modes from §9 are applied before the rename.
- **Settings**:
  - A typed read view over the raw JSON.
  - Writes are **patches applied to a freshly read `serde_json::Value`** (`preserve_order`). The file is never re-serialised from a typed struct. This keeps unknown and TS-only keys, key order, and integers above 2^53 exact (verified in §12), as #14 rules 1–2 require.
  - Snowflakes the Daemon writes are JSON strings (rule 4).
  - Secrets are `Secret<String>`, whose `Debug`/`Display` print `<redacted>`.
- **Sessions**:
  - the Rust (Scope, Launcher) store in `daemon/state/sessions.json`;
  - the TS mirror (`session.json`, `sessions.json`, `agents/<agent>/session.json`) for rule 7;
  - backups (`session_<N>.backup`) and fallback sessions, in their TS shapes.
- **Job state**: `daemon/state/jobs.json` (#9).
- **Run logs** in the TS pattern (rule 5).
- **The audit log** (#13).
- **Lock and pid helpers.**
- **`store::rollback`**: the cutover-only behaviour (§8).

### `claudeclaw-jobs`

Pure job-file logic from #12. No tokio.
- **Reading**: envelope split, serde-saphyr into `BTreeMap<String, Option<String>>`, and the strict value grammar.
- **Cron**: croner 4 configured as #12 §1 says.
- **Validation**: `JobDiagnostic` (error/warning, line/column).
- **Edits**: `LineEdit` operations (set key, insert after, comment out), applied to text. Writes never re-serialise.
- **Lint hook**: a `Lint` trait, so `ts-compat` can add the 1.x divergence lint without `jobs` knowing about TS.
- `jobs` does not read the filesystem. `store` hands it bytes.

### `claudeclaw-runner`

The Claude Code runner and stream parser. It knows nothing about Scopes, sessions or chat.
- **`launcher`**:
  - `Launcher = Direct | Path(AbsPath)`;
  - `check_runnable()`: the file exists, is a regular file and is executable (Unix mode bits / Windows `PATHEXT`). Errors never include the path's arguments or contents (#8);
  - the **Test launcher** procedure: one small run that checks the system prompt arrived and the stream is clean.
- **`argv`**:
  - A typed `ClaudeArgs` builder (prompt, output format, `--resume`, `--append-system-prompt`, `--model`, security args, permission mode).
  - Built-in guards: always `--print`, never `-p`; always `stream-json --verbose` on the main path.
  - The executable is **not** part of `ClaudeArgs`. It is prepended once, at spawn. That makes TS trap 7 (double-wrapped retries) unrepresentable.
- **`spawn`**:
  - `tokio::process::Command` + `process-wrap` 10: `ProcessGroup` on Unix, `JobObject` on Windows, `KillOnDrop`;
  - child env hygiene (strip `CLAUDECODE`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST`);
  - timeout, then SIGTERM, then SIGKILL after 5 s, on the whole group;
  - a 10 MB stdout/stderr cap.
- **`stream`**: a **pure** NDJSON parser with no tokio in the module.
  - Input is `LinesCodec::new_with_max_length` lines.
  - Each line becomes a `#[serde(tag = "type")]` enum with a catch-all variant.
  - Non-JSON lines are kept as `StreamItem::Noise` for the run log, not dropped.
  - `judge()` returns `RunOutcome`: no `result` event means failed, even on exit 0. Errors come from `result.is_error` / `api_error_status` (#5, #8).
  - #18's golden transcripts and Recorder launcher fixtures test this module directly.
- **Output**: `RunEvents`, a stream of text deltas, tool use, tool results, `session_id` and the final `RunOutcome`.

### `claudeclaw-scheduler`

The timer-based scheduler from #9 and #12.
- **Inputs**: the job set (`jobs`), job state (`store`), and the time zone offset.
- **Output**: calls to a `Dispatch` trait that `core` implements.
- **Owns:**
  - next-fire computation, and catch-up through `find_previous_occurrence` within the 6 h window;
  - the in-flight guard and retries (persisted);
  - the one-shot finish (`enabled: false`, from #12 §3);
  - interrupted-job re-run (#9);
  - the heartbeat timer with exclude windows and the rate-limit skip rules.
- **Timing**:
  - Deadlines are computed on the **wall clock** (`chrono::Utc::now()` + `FixedOffset`).
  - Each sleep is capped at 30 s. Tokio's monotonic clock does not advance during system sleep on macOS or Linux, so after a wake or a clock jump the schedule is re-checked within 30 s. That is #9's "re-check after sleep/wake", without a fixed 60 s tick.
- **Tests** run on tokio's paused clock with a fake wall clock.

### `claudeclaw-chat`

The adapter seam. See §6. Only types and traits, plus small helpers that every chat transport needs: message splitting by a size limit, and `[react:x]` directive extraction. No tokio runtime and no transport crates.

### `claudeclaw-core`

The domain services the Worker composes. It owns:
- **`scope`**:
  - `Scope` (Global, Channel, Thread, Job, Agent; each chat Scope tagged with its transport);
  - the **Launcher resolution chain** (thread → parent channel → global, plus job frontmatter `launcher:`). It is used by every code path, including `/compact` (#8);
  - the Launcher picker list ("direct + every path already used by a Scope").
- **`sessions`**:
  - the (Scope, Launcher) session lifecycle;
  - recovery policy (stale session, thinking-signature, rate-limit fallback, auto-compact on timeout);
  - rotation and summaries;
  - orphan pruning (30 days, #12 §6).
  - These are the decisions. The mechanisms are in `runner`.
- **`conversations`**:
  - one serial queue per Scope, a tokio task per active Scope that exits when idle;
  - turning a `Turn` into a run: prompt shaping (clock prefix, `<untrusted-…>` wrapping, the `--append-system-prompt` text from `CLAUDE.md`), model choice (including agentic routing), and the timeout category;
  - interrupted-chat replies on shutdown (#9).
- **`notify`**: owner notifications (job results, heartbeat output, invalid jobs at one per distinct content, Launcher refusals), fanned out to each adapter's `Notifier` and to the event bus for the Desktop app.
- **`events`**: the event bus, a `tokio::sync::broadcast` of `api::DaemonEvent` that feeds SSE.
- **`settings`**: hot reload by polling (§7).

### `claudeclaw-discord`

The Discord adapter, built on twilight 0.17 (`twilight-gateway`, `twilight-http`, `twilight-model`) plus `reqwest` for attachment downloads.
- **Owns**:
  - the gateway loop, slash-command registration and interactions;
  - thread tracking and thread hire/fire, where the intent classifier is a utility run through the seam;
  - trigger rules and allow-lists;
  - attachment intake to `inbox/discord/` (every attachment handed over as a file, 25 MB cap, as in #1);
  - tool-category reactions, the streaming preview, 2000-char splitting and image upload from the outbox.
- **Exports** `notify_owner(token, user_ids, text)`, a REST-only call for the Watchdog's give-up DM.
- **Depends only on `chat`.**

### `claudeclaw-server`

The Management API server (#13): axum 0.8 routers, with paths declared through `utoipa-axum`.
- **Listeners**:
  - **Local.** A Unix socket bound at a temp name, `chmod 0600`, then renamed into place. Clients therefore never see a socket with wider permissions. On Windows, a named pipe created with `first_pipe_instance(true)`, `reject_remote_clients(true)` and an owner-only DACL (`create_with_security_attributes_raw` + `windows-sys`).
  - **Remote**, opt-in. `tokio-rustls` with a custom `ClientCertVerifier`: client certs are optional, any self-signed cert proves key possession. The HTTP layer then authorises by the cert's SPKI-SHA-256 against `devices.json`.
- **Principals.** The same router serves every listener, behind an auth layer that yields one `Principal`:
  - `Local`: any caller on the socket or pipe;
  - `Device(id)`: a pinned client cert;
  - `ApiToken(id, scopes)`: a bearer token, SHA-256-hashed at rest and compared with `subtle`;
  - `Pairing`: an unknown cert, allowed only on `/v1/pair`.
- **Pairing codes** live in memory only, for 10 min.
- **Audit**: every mutating handler records to the audit log through a tower layer, which never logs bodies.
- **Secrets are write-only**: responses report set/unset only.
- **Tracing** comes from `tower-http`.

### `claudeclaw-client`

Used by the CLI and by the Desktop app's backend.
- **Transport**: reqwest 0.13 over one of:
  - `unix_socket` (Unix) or `windows_named_pipe` (Windows), both present in 0.13.5;
  - TLS with a preconfigured rustls `ClientConfig`. Its `ServerCertVerifier` pins the Daemon's SPKI-SHA-256 fingerprint, and it presents the Device cert when paired.
- **Also provides**:
  - a small SSE line parser;
  - `discover(project_dir)`, which computes the socket or pipe address with the §9 rule;
  - `offline_status(project_dir)`, which reads `supervisor.json` and `daemon.pid` read-only when the local socket is down. That lets the app and `claudeclaw status` tell "restarting", "gave up" and "stopped" apart without the API.

### `claudeclaw-ts-compat`

See §8.

### `claudeclaw-desktop`

The Tauri 2 backend, with the React/TS/Vite frontend in `ui/`.
- **Links** only `client` + `api`.
- **Tauri commands:**
  - `daemons_*`: the list of local and remote Daemons, stored in the app data dir;
  - `api_call(daemon, method, path, query, body)`;
  - `api_subscribe(daemon, path, Channel<SseEvent>)`;
  - `daemon_start` / `daemon_stop` / `daemon_attach`: process duties for local Daemons (#9, #10);
  - `pair(host, code)`.
- **Local Daemon start.** The bundled `claudeclaw` binary (Tauri `externalBin`) is spawned **detached in its own session/process group by the app's own code**, not by `tauri-plugin-shell`. Quitting the app therefore leaves the Daemon running (#10).
- **Device keys**: ECDSA P-256 from `rcgen`, private key in the OS keychain via `keyring` 4.
- **Plugins**: `tauri-plugin-notification`, `-autostart` (the app is the only autostart, #9), `-single-instance`, `-updater` (minisign-verified, which matches #6), `-dialog` (Launcher file picker). Tray uses Tauri's built-in `tray-icon`.
- **Generated TS client**:
  - `openapi-typescript` 7 turns `crates/api/openapi.json` into `ui/src/api/schema.d.ts` (committed; CI regenerates and diffs it).
  - The UI calls the API through `openapi-fetch` with a custom `fetch` that routes through `api_call`.
  - One typed contract therefore covers local and remote Daemons, and the webview never opens a socket or holds a key.

### `xtask`

- `openapi`: export or check `openapi.json`.
- `deps`: enforce §3.1.
- `ts-types --check`: run `openapi-typescript` and diff.

## 5. Inside the Worker

| Task (tokio) | Owner crate | Notes |
|---|---|---|
| Control link to the Watchdog | `claudeclaw` | stdin/stdout JSON lines. EOF triggers graceful shutdown. |
| Settings and job reload | `core::settings` | Polls every 30 s (TS parity) on mtime + size + hash, and reloads right after the Daemon's own writes |
| Scheduler + heartbeat | `scheduler` | Dispatches into `core::conversations` |
| Conversations | `core` | One task per active Scope, plus an outcome sink that updates sessions, mirrors, run logs and notifications |
| Chat adapters | `discord` (later `telegram`, `slack`) | One task each, behind `chat::Adapter` |
| Management API | `server` | Local listener always; remote listener when enabled |
| Update check | `claudeclaw` | Once a day; reports only (#6) |
| Housekeeping | `core` | Daily orphan-session prune (#12 §6) |

- **Shutdown** uses one `CancellationToken` and one `TaskTracker`:
  1. stop intake (adapters, scheduler, API mutations);
  2. wait up to 30 s for runs;
  3. kill the remaining process groups;
  4. persist `interrupted` job state;
  5. send interrupted-chat replies;
  6. report `stopping` to the Watchdog.
- **Signals** come from `tokio::signal` (`unix::SignalKind::terminate/interrupt/hangup`; `ctrl_c` and `windows::ctrl_*` on Windows).
- **sd-notify 0.5** sends `READY=1`/`STOPPING=1` under the example systemd unit on Linux. It is a no-op elsewhere.

**Run pipeline:**
1. A `Turn` comes in from an adapter, the scheduler or the API, carrying a Scope, an origin, prompt parts, attachments and overrides.
2. `core` resolves the Launcher for the Scope. A refused Launcher becomes an immediate failed outcome and a notification, with no fallback (#8).
3. `core` looks up the session for (Scope, Launcher) and builds `ClaudeArgs`.
4. `runner` runs it and streams `RunEvents` back to the caller.
5. The outcome updates sessions and the TS mirror, writes the run log, and notifies.

## 6. Chat adapter seam: Discord now, Telegram and Slack later

```rust
// claudeclaw-chat (sketch; names are binding, signatures are indicative)
pub enum Transport { Discord, Telegram, Slack }

pub enum ChatScope {                       // how an adapter names where a message came from
    Direct { transport: Transport },       // DM, which maps to the Global Scope (TS parity)
    Channel { transport: Transport, id: String },
    Thread  { transport: Transport, id: String, parent: String },
}

pub struct Turn {                          // one inbound message, already authorised by the adapter
    pub scope: ChatScope,
    pub author: Author,                    // display name only; IDs stay in the adapter
    pub text: String,                      // raw user text; core does the untrusted-wrapping
    pub context_lines: Vec<String>,        // "[In reply to …]", forwarded text, channel tag
    pub attachments: Vec<LocalFile>,       // already downloaded into inbox/<transport>/
    pub agent_dir: Option<String>,         // thread cwd under agents/ (TS parity)
}

pub trait ChatHost: Send + Sync {          // implemented by core, given to every adapter
    fn converse(&self, turn: Turn) -> RunHandle;        // queued on the Scope's serial queue
    fn utility(&self, prompt: String) -> UtilityHandle; // sessionless run (thread-intent classifier)
    fn sessions(&self) -> &dyn SessionControl;          // reset, compact, status, context usage
    fn launchers(&self) -> &dyn LauncherControl;        // show, set, inherit, Test launcher
    fn settings(&self) -> SettingsWatch;                 // transport settings + hot reload
}

pub struct RunHandle {                     // events stream while Claude works, then one outcome
    pub events: tokio::sync::mpsc::Receiver<RunEvent>,  // TextDelta, ToolUse{name,summary}, ToolResult, AgentSpawn/Done
    pub outcome: tokio::sync::oneshot::Receiver<ChatOutcome>,
}

pub trait Notifier: Send + Sync {          // implemented by each adapter, registered with core
    fn notify_owners(&self, msg: OwnerNotice) -> BoxFuture<'_, Result<(), NotifyError>>;
}

pub trait Adapter: Send + 'static {
    const TRANSPORT: Transport;
    fn notifier(&self) -> Arc<dyn Notifier>;
    async fn run(self, host: Arc<dyn ChatHost>, shutdown: CancellationToken) -> Result<(), AdapterError>;
}
```

**Split of duties:**

| Adapter owns (transport-specific) | Core owns (transport-neutral) |
|---|---|
| Connection, reconnect and resume; rate limits | Scope → session → Launcher resolution |
| Who may talk (allow-lists, trigger rules) | Prompt shaping, untrusted-wrapping, clock prefix |
| Downloading attachments into `inbox/<transport>/` | Serial queue per Scope, runs, recovery, rate-limit state |
| Rendering: reactions per tool category, streaming preview, size splitting, file upload, `[react:x]` | Run logs, session mirror, notifications policy |
| Slash commands / bot commands, mapped onto `SessionControl` / `LauncherControl` | Validation of a typed Launcher path (runnability, allow-listed user check passed in by the adapter) |

**Scope keys.**
- Rust-only state uses unambiguous typed keys: `discord:<id>`, `job:<name>`, `agent:<name>`, and later `tg:<…>` and `slk:<…>`.
- `store::rollback` translates them to TS keys for the mirror and for `runtime.launchers` reads. Discord channels and threads become bare snowflakes, jobs become the bare name, agents stay `agent:<name>`, and `tg:`/`slk:` keep their TS prefixes.
- This removes TS's ambiguity between a job name and a channel ID (trap 5) without changing any TS file.

**Phase 2.**
- Telegram and Slack become `crates/telegram` and `crates/slack`, each implementing `Adapter` against `chat` only.
- Each sits behind a cargo feature of the binary (`discord` is on by default; `telegram` and `slack` come later).
- Their crate picks are deferred to the phase-2 tickets. Nothing in `core` changes to add them.

## 7. Config and hot reload

- **The single config file stays `.claude/claudeclaw/settings.json`** (drop-in).
- Rust-only settings are **new top-level keys** that TS ignores and preserves (#14 rule 1):
  - `launcher`: `{ "default": "direct" | "<abs path>", "scopes": { "<ts scope key>": "<abs path>" } }`;
  - `remote`: `{ "enabled": false, "listen": "<addr>", "port": <n> }`.
  - TS keys (`runtime.launcher(s)`, `web.*`, `apiToken`, …) are read for import only and never rewritten (rules 2–3).
- **Settings precedence** (highest first):
  1. env (`DISCORD_TOKEN`, as TS);
  2. `settings.json`;
  3. built-in defaults.
- **Reload.** The Worker re-reads every 30 s and right after its own writes.
  - A parse error keeps the previous settings and is reported once per distinct content. It is never silent.
  - A changed Discord token restarts only the adapter.
  - `remote.*` changes rebind the remote listener.
- **Alternative rejected:** a file watcher (`notify` 8). Editors' rename-on-save, network filesystems and FSEvents coalescing add edge cases. TS's 30 s poll is proven, and the Daemon's own writes reload immediately anyway.

## 8. Where the TS-compat code lives

Two places, with different lifetimes. Each can be removed in one PR.

**`crates/ts-compat` (`claudeclaw-ts-compat`)**, as #12 asked:

| Module | What | Used by | Removed |
|---|---|---|---|
| `oracle::cron` | Port of TS `cron.ts` (AND matching, modulo steps, ignored single-value steps, never-matching names) | `import`, `lint` | When `main` switches (#12) |
| `oracle::frontmatter` | Port of TS `jobs.ts` line parser | `import`, `lint` | When `main` switches |
| `import` | The one-time Job import: plan, rewrite with `jobs::LineEdit`, backup through `store`, report, audit | Worker first start; `claudeclaw jobs import [--dry-run]` | When `main` switches. See review point 4. |
| `lint` | The 1.x divergence lint, as a `jobs::Lint` | job loading | When `main` switches |

- **Dependency direction is `ts-compat → jobs, store`, never the reverse.** The binary registers it behind the cargo feature `ts-compat`, which is on by default until the switch.
- **Removal** is: delete the directory, drop the feature and its registration lines, and delete the CLI subcommand.
- The `oracle` modules double as #18's parity fixtures for cron and frontmatter readings.

**`crates/store/src/rollback.rs` (`store::rollback`).** The #14 rules that shape *writes* until `main` switches:

| Rule | Mechanism in `store::rollback` |
|---|---|
| 1–3 additive, preserve, import-never-move | Settings patch writer (only Rust keys, never removes); import copies |
| 4 snowflakes as strings | Patch writer emits strings for any snowflake it writes |
| 5 TS run-log names | `RunLog::create(name, start)` → `logs/<name>-<YYYY-MM-DDTHH-MM-SS-mmmZ>.log` (agent jobs in `logs/<agent>/`), TS body layout, Rust additions appended as extra lines/sections |
| 6 merge job state every start | `JobState::load_merged()` = newer of `daemon/state/jobs.json` and newest TS-pattern log per job (including `logs/<agent>/`, fixing trap 6) |
| 7 mirror active session | `SessionMirror::write(scope, session)` into `session.json` / `sessions.json.threads[<ts key>]` / `agents/<agent>/session.json` in TS shape; TS entry adopted as current Launcher's session on read if it differs |
| 8 one-shots stop TS too | `enabled: false` edit from #12 (in `jobs`, verified TS-readable) |
| 9 rewritten cron same under both | Enforced by `ts-compat::import` |
| 10 guard the other daemon | `PidGuard`: refuse if `daemon.pid` names a live process that is not this Daemon; write Watchdog pid as `"<pid>\n"` |

**What stays after the switch and what goes.**
- **Stays.** Reading the TS *formats* (settings, session files, job files) is the permanent drop-in contract, not compat code. It stays in `store` and `jobs`.
- **Goes.** At the switch, the maintainer deletes `store::rollback` or keeps parts of it on purpose (for example, the run-log naming). Any break in TS readability goes in the release notes (#14 §6.4).

## 9. On-disk layout

### 9.1 Per project dir (one Daemon per dir, #9)

Owner key:
- **S**: shared with TS, kept in TS shape;
- **R**: Rust-only, which TS never reads;
- **T**: TS-only, which Rust leaves untouched.

```
<project-dir>/
├── CLAUDE.md                                S  read every run; created once if absent (TS behaviour)
├── agents/<agent>/                          S  agent cwd; jobs/*.md; session.json (mirror, rule 7)
└── .claude/
    ├── settings.json                        T  Claude Code's own project settings: Rust does not write statusLine (§13 point 8)
    └── claudeclaw/
        ├── settings.json                    S  0600 enforced; Rust adds `launcher`, `remote` keys only
        ├── session.json, sessions.json      S  TS-shape mirror of each Scope's current-Launcher session
        ├── session_fallback.json, fallback-sessions/, session_<N>.backup   S  same shapes as TS
        ├── pending-resume.json              S  consumed as TS does
        ├── permission-mode.json             S  read
        ├── jobs/*.md                        S  line edits + atomic rename only; never re-serialised
        ├── prompts/HEARTBEAT.md             S  read
        ├── inbox/discord/                   S  attachments
        ├── outbox/discord/                  S  image upload root
        ├── logs/<name>-<iso>.log            S  run logs, TS pattern (rule 5); logs/<agent>/ for agent jobs
        ├── logs/daemon.log                  T  TS's stdout redirect; Rust never writes it
        ├── state.json, active-runs, web.token   T  not written by Rust (§13 point 8)
        ├── daemon.pid                       S  "<watchdog pid>\n" (rule 10)
        ├── daemon.lock                      R  OS file lock held by the Watchdog (#9)
        ├── api.sock                         R  local Management API, 0600 (#13); see 9.3
        ├── import-backup/<ts>/, import-report-<ts>.md   R  Job import (#12)
        └── daemon/                          R  0700; everything only the Rust Daemon owns
            ├── state/
            │   ├── jobs.json                per-job state: last run, result, in-flight, retry, interrupted (#9)
            │   ├── sessions.json            (Scope, Launcher) → session; orphan clocks (#12 §6)
            │   ├── supervisor.json          Watchdog status (state, Worker pid, crash times, gave-up)
            │   └── notices.json             content hashes already notified (one notice per distinct content)
            ├── secrets/                     0700; files 0600
            │   ├── devices.json             paired Device public keys (SPKI SHA-256 + label + created/last seen)
            │   ├── api-tokens.json          API token id, name, scopes, SHA-256 hash (legacy apiToken imported here)
            │   └── tls/daemon.key, daemon.crt   self-generated (rcgen, ECDSA P-256)
            ├── logs/
            │   ├── worker.<YYYY-MM-DD>.jsonl    tracing JSON, daily rotation
            │   └── watchdog.<YYYY-MM-DD>.jsonl  tracing JSON, daily rotation (includes Worker stderr/panics)
            └── audit/<YYYY-MM>.jsonl        append-only audit log (#13): who, what, when; never secrets or prompts
```

**Why a single `daemon/` subdir.**
- One permission boundary (0700) covers secrets and state.
- `ls` shows exactly what Rust owns.
- No name can collide with a TS file, or with a `logs/<agent>/` dir: an agent called `audit` or `daemon` would otherwise clash with Rust's own logs.
- The paths that #9, #12 and #13 already fixed (`daemon.lock`, the socket in `.claude/claudeclaw/`, `import-backup/`) stay where those tickets put them.

**Write discipline.**
- Every write is atomic (temp file + rename in the same dir).
- `settings.json` is patched over a fresh read.
- Job files are line-edited.
- JSONL logs are append-only.
- Modes: `settings.json` and every file under `daemon/secrets/` are 0600; `daemon/` is 0700. On Windows, files inherit the user-profile ACL. The named pipe gets an owner-only DACL.

**Retention.**
- Run logs are never rotated (TS parity; the catch-up seed reads them).
- `daemon/logs/` keeps 14 days by default (`tracing-appender` `max_log_files`).
- The audit log is kept.
- A later logging ticket may change these defaults.

### 9.2 Outside the project dir

| Path | Owner | What |
|---|---|---|
| `dirs::data_local_dir()/claudeclaw/known-daemons.json` | CLI/Daemon | Project dirs that have run a Daemon (written at Watchdog start). It replaces TS's lossy decoding of `~/.claude/projects` names for `status --all` / `stop --all`. Entries are checked against the lock, never trusted. |
| `dirs::cache_dir()/claudeclaw/update-check.json` | Daemon | Last daily Releases check (#6) |
| Tauri `app_data_dir()/daemons.json` | Desktop app | Local dirs and remote Daemons (host, port, pinned fingerprint, label) |
| OS keychain (`keyring`) | Desktop app | One Device private key per remote Daemon (#13) |
| `~/.claude/projects/<slug>/*.jsonl` | Claude Code | Read-only, for context, usage and session history (TS parity) |

### 9.3 The socket path rule

On macOS, `bind` fails for a socket path of 104 bytes or more ("path must be shorter than SUN_LEN"; verified in §12). On Linux the limit is 108. So `<project-dir>/.claude/claudeclaw/api.sock` (25 bytes plus the dir) breaks for project paths longer than about 78 bytes.

**Rule**, computed identically by `store::Layout` and `client::discover`:
1. Use `<project-dir>/.claude/claudeclaw/api.sock` if its absolute path is under 100 bytes.
2. Otherwise use `<runtime>/claudeclaw-<uid>/<first 16 hex of SHA-256(canonical project dir)>.sock`, where `<runtime>` is `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else `/tmp`. The dir is 0700 and is checked to be owned by the user.

On Windows the pipe is always `\\.\pipe\claudeclaw-<same 16 hex>`.

There is no pointer file, so there is nothing to go stale.

## 10. Crate picks (versions as of 2026-10-13, from crates.io)

Requirements are caret (`"x.y"`). `Cargo.lock` is committed. Daemon-side features were resolved together in the probe (§12).

| Area | Crate | Version | Features / notes |
|---|---|---|---|
| Runtime | `tokio` | 1.53 | `rt-multi-thread, macros, process, signal, net, fs, time, sync, io-util` |
| | `tokio-util` | 0.7.20 | `codec` (`LinesCodec` with max length), `rt` (`CancellationToken`, `TaskTracker`) |
| | `tokio-stream` | 0.1.19 | `sync` (`BroadcastStream` → SSE) |
| | `futures-util` | 0.3.34 | `BoxFuture`, stream combinators |
| Serde | `serde` | 1.0.229 | `derive` |
| | `serde_json` | 1.0.154 | `preserve_order` (key order kept on patch writes) |
| | `serde-saphyr` | 1.3.0 | job frontmatter (#12) |
| Time / cron | `chrono` | 0.4.45 | `FixedOffset` (#4) |
| | `croner` | 4.0.1 | `chrono`; config per #12 §1 |
| Processes | `process-wrap` | 10.0.1 | `tokio1, process-group, process-session, kill-on-drop, job-object` |
| | `nix` | 0.31 | Unix only: `signal, process, fs` (`killpg`, `setsid`, `fcntl` for the lock fd on re-exec) |
| | `windows-sys` | 0.61 | Windows only: pipe security descriptor, job objects |
| Discord | `twilight-gateway`, `twilight-http`, `twilight-model` | 0.17.1 | `default-features = false`, `rustls-platform-verifier`, `zlib` (gateway) / `decompression` (http) |
| HTTP client | `reqwest` | 0.13.5 | `default-features = false`, `rustls-no-provider, json, stream, http2` (attachments, update download, `client` crate incl. `unix_socket` / `windows_named_pipe`) |
| HTTP server | `axum` | 0.8.9 | `macros`; serves `tokio::net::UnixListener` directly (verified) |
| | `tower` | 0.5.3 | layers |
| | `tower-http` | 0.7.1 | `trace, timeout, limit` |
| | `hyper`, `hyper-util` | 1.12, 0.1.21 | named-pipe and TLS accept loops (`server-auto`) |
| OpenAPI | `utoipa` | 6.0.0 | `axum_extras, chrono, preserve_order`; emits OpenAPI 3.1 (verified) |
| | `utoipa-axum` | 0.3.0 | `OpenApiRouter`, `routes!` |
| TLS / keys | `rustls` | 0.23.45 | `default-features = false`, `ring, std, tls12, logging` |
| | `tokio-rustls` | 0.26.6 | `ring` |
| | `rcgen` | 0.14.10 | `default-features = false`, `ring, crypto, pem` (Daemon cert, Device certs) |
| | `sha2` | 0.11.1 | SPKI fingerprints, API token hashes, socket hash |
| | `subtle` | 2.6.1 | constant-time compares |
| | `getrandom` | 0.4.3 | pairing codes, API tokens |
| Updates | `minisign-verify` | 0.3.0 | verify `SHA256SUMS.minisig` with the embedded key (#6) |
| Observability | `tracing` | 0.1.44 | |
| | `tracing-subscriber` | 0.3.23 | `json, env-filter` |
| | `tracing-appender` | 0.2.5 | daily rolling files, non-blocking writer |
| | `sd-notify` | 0.5.0 | Linux only (#4) |
| CLI | `clap` | 4.6.7 | `derive` |
| Misc | `tempfile` | 3.27.0 | atomic writes |
| | `dirs` | 7.0.0 | user dirs (§9.2) |
| | `uuid` | 1.28.0 | `v7, serde` (run IDs, audit IDs) |
| | `thiserror`, `anyhow` | 2.0.21, 1.0.104 | |
| Desktop | `tauri` / `tauri-build` | 2.12.3 / 2.7.2 | `tray-icon` |
| | `tauri-plugin-notification`, `-autostart`, `-single-instance`, `-updater`, `-dialog` | 2.6.0, 2.7.0, 2.5.3, 2.13.3, 2.8.2 | |
| | `keyring` | 4.2.0 | per-OS native stores (Keychain, Windows Credential Manager, Secret Service) |
| Desktop UI (npm) | `@tauri-apps/api` 2.12, React 19, Vite 8, Tailwind 4, shadcn/ui | | majors fixed here; exact pins and look go to [#16](https://github.com/Winter-wub/claudeclaw/issues/16) |
| | `openapi-typescript` / `openapi-fetch` | 7.13 / 0.17 | generated types + typed fetch over `api_call` |

**Not picked, on purpose.** The binary needs none of these:
- `serenity`/`poise` (twilight was chosen, #4);
- `tokio-cron-scheduler` (#4);
- `serde_yaml` and its forks (#4);
- `notify` (§7);
- `fs4` (std `File::lock` replaces it);
- `interprocess` (tokio's native pipes plus a DACL suffice);
- `self_update` / `self-replace` (the Windows rename is about 30 lines, and `self-replace`'s last release was 2024-09);
- `async-trait` (native async traits);
- `openssl`/`native-tls` (one TLS stack, rustls on ring).

Test tooling (snapshot, property and replay crates) is left to [#18](https://github.com/Winter-wub/claudeclaw/issues/18).

## 11. Decisions and alternatives

| # | Decision | Alternatives rejected |
|---|---|---|
| D1 | **13-crate workspace with enforced dependency rules** (§3) | *One daemon crate with modules.* Boundaries would be by convention only: an adapter could call the store, and the app could link state code. That breaks #10's "never writes daemon files" by accident, and `ts-compat` removal becomes a hunt. *Many micro-crates* (one per adapter feature, per API area): more `Cargo.toml` churn with no boundary benefit. |
| D2 | **Worker serves the Management API. The Watchdog stays minimal** and publishes `supervisor.json`, which the client reads when the socket is down | *Watchdog owns the listeners and proxies to the Worker.* The API would stay up during a crash loop, but the Watchdog would take on TLS, auth and HTTP, which contradicts the glossary ("only starts, restarts and re-executes") and widens the code that must never crash. *Watchdog passes listening fds to each Worker.* Not portable to Windows named pipes. |
| D3 | **Watchdog ↔ Worker over the Worker's stdin/stdout** (JSON lines; EOF = parent gone) | *A second private socket.* More code, and no free orphan detection. *Signals only.* Can't tell an intentional stop from a crash on Windows. |
| D4 | **Wall-clock deadlines with sleeps capped at 30 s** | *`sleep_until` on tokio's `Instant` alone.* The monotonic clock pauses during system sleep, so a job due during sleep would fire late by the sleep's length. *TS's 60 s tick.* Wastes wake-ups and gives up to 60 s jitter (#14 measured TS up to 60 s late). |
| D5 | **Chat seam in `claudeclaw-chat`, adapters depend only on it** (§6) | *An adapter trait inside `core`.* Adapters would then see all of core. *A generic "plugin" interface now.* The OpenClaw-compatible plugin system is phase 2 and unspecified, and this seam can host it later. |
| D6 | **Typed Scope keys in Rust state, TS keys only in the mirror** | *Reuse TS's bare keys everywhere.* Job names and snowflakes share one namespace (trap 5). |
| D7 | **Settings: raw-`Value` patch writes, Rust keys at the top level of `settings.json`** | *A separate Rust config file.* Two places for users to look, and the Desktop app's settings would split. *Typed re-serialisation.* Drops unknown keys and order (rule 2). |
| D8 | **Rust-only files under `.claude/claudeclaw/daemon/`**, except paths earlier tickets fixed | *Rust files scattered beside TS files.* Name collisions, no single 0700 boundary. *Outside the project dir* (XDG state). Breaks "one Daemon per project dir" portability and the scrubbed-copy Shadow instance workflow (#14). |
| D9 | **Socket path: in-dir if short, hashed runtime path otherwise** | *Always in-dir.* Fails on long paths (verified). *Always hashed.* Contradicts #13's "socket in `.claude/claudeclaw/`" for the common case. *A pointer file.* It can go stale. |
| D10 | **Local transport with tokio-native UDS / named pipe**, and reqwest 0.13's built-in `unix_socket` / `windows_named_pipe` on the client | *`interprocess` crate.* An extra abstraction where tokio already has both. *hyper-util custom connectors.* Reqwest 0.13.5 already does this. |
| D11 | **Remote TLS on rustls/ring with custom verifiers; Device = TLS client cert pinned by SPKI hash** | *Bearer Device keys over server-only TLS.* Weaker: a leaked bearer token works anywhere. *aws-lc-rs provider.* Needs cmake/NASM on Windows CI for no gain here; the probe confirms nothing pulls it in. *native-tls.* Three TLS stacks across OSes, and no pinning hooks. |
| D12 | **OpenAPI via utoipa 6 → committed `openapi.json` → `openapi-typescript` + `openapi-fetch` in the app** | *ts-rs / specta / tauri-specta.* They generate TS types but not OpenAPI, which #13 requires for remote automation. *aide.* Smaller user base, last release 2025-08. *A hand-written OpenAPI.* Drifts. *openapi-generator / orval.* Heavy runtime code generation. |
| D13 | **Desktop UI calls the API through one Tauri `api_call` / `api_subscribe` bridge** | *Webview `fetch` to the Daemon.* Webviews can't open Unix sockets or named pipes, and the Device key would have to live in JS. *One Tauri command per endpoint.* Duplicates the API surface by hand. |
| D14 | **App starts the Daemon detached with its own code** | *`tauri-plugin-shell` sidecar.* Ties the child to the app's lifetime, while #10 wants the Daemon to keep running after the app quits. |
| D15 | **`ts-compat` as its own crate + `store::rollback` module** | *A `ts_compat` module inside `jobs`.* Deleting it touches `jobs`, and nothing stops other code from calling the oracle. *Everything in one crate.* Mixes the oracle (deleted at the switch) with drop-in readers (permanent). |
| D16 | **Polling settings reload** (§7) | *`notify` file watcher.* See §7. |
| D17 | **std `File::try_lock`** | *`fs4`.* Redundant since Rust 1.89. |
| D18 | **No self-update crate** | *`self_update`.* It brings its own release discovery and archive handling; #6 needs only download, minisign verify and a rename. |

## 12. Evidence

A throwaway probe (outside the repo, not committed) used rustc 1.95.0 on macOS arm64 against the daemon-side dependency set in §10, on 2026-10-13.

| Claim | Result |
|---|---|
| The whole daemon dependency set resolves and builds together, with the §10 features (twilight on `rustls-platform-verifier`) | 308 packages locked; `cargo build` clean. One version each of `tokio`, `axum`, `hyper`, `rustls`. |
| No second crypto stack | `cargo tree -i aws-lc-rs` / `aws-lc-sys` / `openssl-sys`: no match. `ring` is used by `rustls`, `rcgen`, `reqwest`, `twilight-http`. |
| Reqwest with `rustls-no-provider` needs an installed provider | Panics at `Client::build` until `rustls::crypto::ring::default_provider().install_default()` |
| axum 0.8.9 serves HTTP and SSE on a Unix socket; reqwest 0.13.5 talks to it via `unix_socket` | `GET /v1/health` → `ok`; SSE body `event: tick\ndata: 0 …` |
| Socket can be restricted to 0600 | mode `600` after chmod (the spec binds at a temp name then renames, so the final path never has wider permissions) |
| macOS `sun_path` limit | 100 and 103 bytes bind; 104 and 108 fail with "path must be shorter than SUN_LEN" |
| `std::fs::File::try_lock` | second lock on the same file: "operation would block" |
| `serde_json` (`preserve_order`) raw round trip | unknown keys, key order and a 19-digit snowflake number survive; new key appended last |
| croner 4.0.1 with `FixedOffset` (+07:00) | `0 9 3-31/3 * *` at 2026-10-13 12:00 → prev 2026-10-12 09:00, next 2026-10-15 09:00 |
| utoipa 6 + utoipa-axum 0.3 on axum 0.8 | `OpenApiRouter` emits `"openapi": "3.1.0"` |
| Tokio Windows pipe APIs exist | `ServerOptions::first_pipe_instance`, `reject_remote_clients`, `create_with_security_attributes_raw` in tokio 1.53.2 source |
| reqwest named-pipe client | `ClientBuilder::windows_named_pipe` in reqwest 0.13.5 source (`cfg(target_os = "windows")`) |

**Not verified:**
- Windows and Linux builds.
- The Tauri crates (MSRV 1.90; not compiled).
- Lock-fd inheritance across `exec`.
- The named-pipe DACL at runtime.
- twilight against a live gateway.

These belong to the first implementation PRs and to #18's suite.

## 13. Review points

1. **13 crates** (D1). That is the most structure of any choice here. Merging `scheduler` into `core` and `jobs` into `store` would leave 11 crates, but `ts-compat` and parity tests then reach deeper.
2. **The Worker serves the API, not the Watchdog** (D2). During a crash-loop backoff, remote apps see "unreachable", and the give-up is reported by Discord DM plus `supervisor.json` locally. A Watchdog-owned API would avoid that gap, at the cost of a bigger Watchdog.
3. **Rust-only state under `.claude/claudeclaw/daemon/`** (D8) and the new top-level `launcher` / `remote` keys in `settings.json` (D7).
4. **Job import is deleted with `ts-compat` when `main` switches**, as #12 says. 1.x users who upgrade **after** v2.0.0 would then get no import, and #12 rejected that path for being "loud but not drop-in". Keeping `oracle` + `import` through 2.x and deleting only `lint` is the likely fix. It is flagged for a decision rather than changed here.
5. **Device = TLS client certificate pinned by SPKI** (D11). It locks the Device key format to X.509 + ECDSA P-256.
6. **ring instead of aws-lc-rs** (D11). Revisit if FIPS or a post-quantum key exchange becomes a requirement.
7. **The 30 s sleep cap** (D4) instead of OS wake notifications.
8. **No statusline in Rust v2.0.** `.claude/statusline.cjs` and `state.json` aren't written; a follow-up should decide whether status moves to the API only. Today's TS statusline goes away at cutover.
9. **`known-daemons.json` outside the project dir** (§9.2). It is a small new global file, needed only for `status --all` / `stop --all`.
10. **twilight's `rustls-platform-verifier`** (OS trust store) instead of bundled webpki roots.

## Amendments

### From [Decide what the app can show when a daemon is not answering](https://github.com/Winter-wub/claudeclaw/issues/20)

- **`supervisor.json` / `api::SupervisorStatus`** takes the #20 D2 shape: `schema`, `state`, `watchdog`, `worker.boot_id`, `next_restart_at`, `stop.by`, `gave_up`, and `crashes[]` (the last 20) with a redacted `stderr_tail`. It carries across Watchdog starts, and `/v1/status` embeds it along with `boot_id`.
- **Watchdog**
  - Retries `try_lock` on `daemon.lock` for up to 1s before exiting 73. Without the retry, a concurrent read-only lock probe from the app made a starting Watchdog exit 73 by mistake; with it, the Watchdog got the lock after ~222ms.
  - Redacts known secret values in stderr tails.
- **`client::offline_status`** adds a read-only, non-creating shared-lock probe and returns `api::DaemonReachability` (the 9 local / 7 remote states of #20 D1).
- **Desktop crate**
  - One new Tauri command, `daemon_offline_logs(daemon)`, which runs the bundled `claudeclaw logs --offline --json` with a fixed argv. This is the only read path outside the Management API.
  - Offline process actions go through the bundled `claudeclaw start` / `stop`.
- **Example systemd unit:** `Restart=on-failure`, `RestartPreventExitStatus=70 73`.

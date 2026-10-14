# ClaudeClaw 2.x architecture and crate picks

Resolves [#11 "Lock the architecture and crate picks"](https://github.com/Winter-wub/claudeclaw/issues/11) on the wayfinder map [#2](https://github.com/Winter-wub/claudeclaw/issues/2), and carries every later decision that changes it. Each later decision is folded into the section it changes and cited inline as (#N), so this doc reads as the one current spec. The [Decision index](#decision-index) at the end lists every closed ticket on the map.

> **Delegated decision.** An agent made the #11 decisions on 2026-10-13 because the maintainer asked to "continue without asking". They are open to revision. The [Review points](#13-review-points) list the choices most likely to need a second look. Later tickets say in the index whether they were delegated or the maintainer's.

Terms follow `GLOSSARY.md`: Daemon, Watchdog, Worker, Chat adapter, Turn, Launcher, Scope, Refused run, Run PATH, Run locale, System-prompt file, Awake time, Resume, Wake grace, Hang kill, Stall, Run registry, Straggler, Settle, Reap, Give-up, Held start, Service manager, Crash record, Change scan, Housekeeping, Inbox, Back-fill, Config dir, Cost baseline, Plugin shim, Desktop app, App log, Management API, Device, API token, Pairing code, Pairing string, Deadline, Detach, Shadow instance, Recorder launcher, Verification register, Known gap.

The Management API's resources are specified separately in [`management-api.md`](management-api.md).

**Inputs #11 had to honour:**

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
 CLI ──────────┼─ API ─┼─► restarts / re-execs / reaps ─► Worker (child)     │
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
- **The Worker is the only process that runs Claude Code.** The Watchdog only starts, restarts and re-executes the Worker, and reaps its leftover runs (#29).
- **The Desktop app links only the API types and the API client.** It never links the Daemon's state code, so it cannot write Daemon files even by accident (#10).

## 2. Toolchain and workspace conventions

| Item | Decision |
|---|---|
| Edition | 2024 |
| MSRV (`workspace.package.rust-version`) | **1.90**. Floor set by Tauri 2.12 (1.90); twilight 0.17, serde-saphyr 1.3 and uuid 1.28 need 1.89. |
| Pinned toolchain (`rust-toolchain.toml`) | A specific stable, bumped by PR. **1.95.0** at the time of writing, which is what the probe in §12 used. |
| Async runtime | **tokio 1.53**. The Worker is multi-thread with `worker_threads(max(4, available_parallelism))`, so a small VPS can't produce a false hang (#27). The Watchdog and CLI use `current_thread`. |
| Blocking calls | `clippy::disallowed_methods` bans `std::fs::*` and `std::thread::sleep` in the async crates (#27). `store` stays synchronous and is called through `spawn_blocking`. |
| Errors | `thiserror` in libraries, `anyhow` only in the binary and `xtask`. |
| Logging | `tracing` everywhere. No `println!` outside CLI output. |
| Secrets | `secrecy::SecretString` (#23). Content types (`Turn`, `ClaudeArgs`, `StreamItem`, `RunEvent`, `LocalFile`, job bodies) get a hand-written `Debug` that prints only sizes; `#[derive(Debug)]` on them is a review blocker. |
| Dependencies | Declared once in `[workspace.dependencies]`. Crates use `dep.workspace = true`. `deny.toml` bans `openssl-sys`, `aws-lc-sys` and `native-tls` (#35). |
| Default members | Every crate **except** `claudeclaw-desktop`, so a root `cargo build`/`cargo test` never needs WebKitGTK or Node. |
| TLS crypto provider | **ring**, everywhere. Installed once at process start with `rustls::crypto::ring::default_provider().install_default()`. Reqwest panics without it (seen in the probe). |
| Async traits | Native `async fn` in traits for static dispatch. Traits used as `dyn` return handles or `BoxFuture`. No `async-trait` crate. |
| Test tooling | `proptest`, `similar-asserts`, tokio's paused clock and axum's `ws` feature. No snapshot crate (#18). |

## 3. Workspace layout

```
Cargo.toml                    workspace, [workspace.dependencies], default-members
rust-toolchain.toml
deny.toml                     cargo-deny bans (#35)
CHANGELOG.md                  release notes source (#35)
crates/
  claudeclaw/                 bin "claudeclaw": CLI, Watchdog, Worker composition root
    keys/*.pub                the two embedded minisign public keys (#35)
  api/                        claudeclaw-api: Management API wire types, SSE event types, openapi.json
  store/                      claudeclaw-store: on-disk layout, atomic IO, settings, sessions, job state, logs, lock/pid
  jobs/                       claudeclaw-jobs: job files (envelope, YAML, value grammar, line edits), cron
  runner/                     claudeclaw-runner: Launcher resolution, argv, spawn/settle/kill, stream-json parser, result judging
  scheduler/                  claudeclaw-scheduler: timers, catch-up, in-flight guard, retries, heartbeat
  chat/                       claudeclaw-chat: the chat-adapter seam (traits + types, no transport deps)
  core/                       claudeclaw-core: Scopes, Launcher resolution, sessions, Conversations, notifications
  discord/                    claudeclaw-discord: Discord adapter (twilight)
  server/                     claudeclaw-server: Management API server (axum), auth, pairing, audit, SSE
  client/                     claudeclaw-client: Management API client (socket / pipe / pinned TLS)
  ts-compat/                  claudeclaw-ts-compat: TS cron + frontmatter oracle, First-start import, 1.x divergence lint
  desktop/                    claudeclaw-desktop: Tauri 2 backend (Cargo.toml, tauri.conf.json, src/)
    ui/                       React + TS + Vite frontend; src/api/schema.d.ts generated from openapi.json
  parity/                     claudeclaw-parity: claudeclaw-recorder + differential harness; publish = false (#18)
parity/                       corpus, scenarios, generated fixtures, divergences.toml, capture-check.json, bun generators (#18, #35)
verification/checks.toml      the Verification register (#36)
scripts/release/              sign.sh, brew.sh (#35)
xtask/                        cargo xtask: openapi, ts-types, deps, release, verify
examples/launchers/           ccs, ccr, record (Recorder launcher), config-dir.{sh,cmd} (#8, #14, #39)
.claude-plugin/ commands/ skills/   Plugin shim (replaces the TS plugin at cutover, #7)
Formula/ Casks/               Homebrew tap (#7)
src/                          TS daemon, frozen to bug fixes until main switches
```

Package names are prefixed `claudeclaw-`. Directory names are short. Crate names follow the glossary: there is no `bot`, `service` or `supervisor` crate. There are 13 shipped crates plus `parity`, which is never released (#18).

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
claudeclaw → everything above except desktop and parity
desktop    → client, api
parity     → core, runner, store, jobs, ts-compat, chat, discord, api      (#18)
```

`xtask deps` also forbids Tauri's `tracing` feature in `desktop`, so the App log stays content-free and in one format (#40).

What these rules buy:
- **Adapters see only `chat`.** A Discord bug cannot reach into sessions or the store, and Telegram and Slack plug in against the same narrow seam.
- **`desktop` sees only `client` and `api`.** The Desktop app can't touch Daemon files (#10).
- **Nothing depends on `ts-compat` or `parity`.** Only the binary registers `ts-compat`, so deleting it is one directory and three lines (§8).
- **`runner`, `jobs` and `chat` are leaves.** They are where the parity fixtures of [#18](https://github.com/Winter-wub/claudeclaw/issues/18) attach without a Daemon.

## 4. Crate boundaries

### `claudeclaw` (binary)

- **Owns:**
  - the `clap` CLI (the command table is #22's; it is local-only, never a Device);
  - the Watchdog role (`claudeclaw start`; a bare `claudeclaw` also starts);
  - the Worker role (a hidden `claudeclaw worker`, spawned only by the Watchdog);
  - the hidden `__env-probe <nonce>` subcommand for the Run PATH capture (#26) and `__verify-release` (#35);
  - `claudeclaw statusline` (§4 "Statusline" below, #24);
  - the composition root: start the Awake clock first (#38), then build `core`, register adapters and `ts-compat`, start `server`;
  - self-update, verified against the two embedded minisign public keys in `crates/claudeclaw/keys/` (#35);
  - a `test-hooks` cargo feature for CI, never enabled in release builds (#36).
- **CLI target dir.** The dir comes from `--dir`, then `CLAUDECLAW_DIR`, then cwd, with no walk-up (#22). Mutations go through the API while the Daemon answers; with no Daemon, file-only changes are Offline writes under `daemon.lock`, which records its holder (CLI, pid, command) while the CLI holds it (#22).

**Watchdog.** It stays deliberately small, matching the glossary: it only starts, restarts and re-executes the Worker, and reaps its leftover runs.
- **Start sequence** (#45):
  1. **Take `daemon.lock`** with `std::fs::File::try_lock` (stable since 1.89; the probe showed a second lock is refused). It retries for up to 1 s in 50 ms steps before exiting 73, because a concurrent read-only lock probe from the app otherwise made a starting Watchdog exit 73 by mistake (#20; with the retry it got the lock after ~222 ms). When the lock's holder record names an Offline write, it waits up to 10 s instead (#45). It checks and then writes `daemon.pid` (#14 rule 10).
  2. **Reap** leftover runs, as any lock holder with no live Worker does (#29). A new Watchdog waits up to 35 s for an orphaned Worker of a previous Watchdog, then kills it, so two Workers never overlap.
  3. **Read `supervisor.json`.** If it records a Give-up and there is no `--clear-give-up`, this is a **Held start**: update `gave_up.held_starts {count, last_at, last_by}`, log `watchdog.start_held`, print the cause and the clear command to stderr (`reason: give_up_held` with `--json`), release the lock and exit 70 (0 under launchd). No Worker, no First-start import and no adapters run. An unreadable `supervisor.json` counts as no hold; it is renamed to `supervisor.json.unreadable-<ts>` and logged as `watchdog.supervisor_unreadable`.
  4. **Reminder DM**, only for starts with `--service-manager`: one `notify_owner` DM per Give-up per 24 h (`gave_up.reminded_at`). A plain CLI start doesn't DM; the app's login start doesn't either, because the app already notifies.
- **Control pipe.** It spawns the Worker with stdin/stdout pipes that carry line-delimited JSON. Unknown message types are logged and ignored (#27).

  | Direction | Messages |
  |---|---|
  | Worker → Watchdog | `ready`, `stopping(intentional)`, `fatal(reason)`, `draining{intentional}`, `pong{seq}`, `stalled{…}` (#27), `restart` (replace the Worker; not a crash), `reexec(path)` (#21) |
  | Watchdog → Worker | `shutdown(reason)`, `ping{seq}` (#27), `reexec_result(ok \| err)`, sent after `<path> --version` and before anything stops, so `daemon.reexec` can return `422 exe_check_failed` while the old Worker still serves (#21) |

- **Hang detection** (#27). The Watchdog pings 5 s after the last pong, with at most one ping outstanding. In the Worker the control link runs on two OS threads, `control-r` and `control-w`, started before any other work; a pong is written only after a probe task spawned with `Handle::spawn` has run. The Watchdog ticks every 1 s and accrues silence in steps capped at 2 s, so sleep never counts. After 45 s of awake silence it SIGKILLs / `TerminateProcess`es the Worker pid: a Hang kill, recorded as `cause: hung` and counted as a crash. `CLAUDECLAW_HANG_KILL=0` in the Watchdog's env disables it; `supervisor.json` then carries `hang_kill: false` and status shows it.
- **Orphan protection.** EOF on the Worker's stdin means the Watchdog died, and the Worker shuts down gracefully. 45 s after EOF it calls `std::process::abort()` (the orphan deadline, #27). This keeps a Worker from being orphaned on any OS.
- **Drains.** After `draining` or its own `shutdown`, the Watchdog gives the Worker a 45 s exit deadline of accrued time (30 s for runs + 5 s TERM→KILL + 10 s to persist and reply); an overrun is SIGKILLed and recorded as `shutdown_overrun`, not counted (#27). On SIGTERM the Watchdog drains the Worker, waits for it (≤35 s) and its Reap, then exits 75 (#27, #29). The lock is never released while a Worker lives (#29).
- **Status file.** It writes `daemon/state/supervisor.json` (`api::SupervisorStatus`, the #20 D2 shape) on every transition. It carries across Watchdog starts, and `/v1/status` embeds it along with `boot_id`:
  - `schema` (1; every later change is additive), `state`, `watchdog` (pid, the OS start-time stamp that `stop` checks before signalling (#36), `service_manager` (#45)), `worker.boot_id`, `next_restart_at`, `stop.by` (incl. `api`, `cli`, `clear_give_up`), `hang_kill`;
  - `gave_up` (with `held_starts` and `reminded_at`) and `give_ups[]` (#45);
  - `crashes[]`, the Crash record: the last 20 crashes with time, exit, reported reason, `cause` (`exit | fatal | stall | hung | shutdown_overrun | abort`; on Windows exit status `0xC0000409` is `abort`, #36), `counted` (#27), per-crash `reaped` counts (#29) and a redacted `stderr_tail` (tails on the last 5). The Watchdog redacts known secret values in stderr tails (#20).
- **Restarts.** It applies the #9 backoff (1 s → 60 s). More than 5 counted crashes in 10 min is a **Give-up**. After an unplanned restart, the new Worker sends one owner notice through `core::notify` once it reaches `ready` ("Restarted after a hang at …; N jobs will re-run, M chats were interrupted"), at most one per 10 min (#27).
- **Give-up.** It records the Give-up in `supervisor.json`, DMs the owner through `claudeclaw-discord`'s REST-only `notify_owner` (no gateway, no Claude) and exits 70 (0 under launchd). Every later start is a Held start until a person clears it with `start --clear-give-up` or the offline `claudeclaw clear-give-up` (#45). A Give-up has no expiry, including on upgrade.
- **Reaping** (#29). Whoever holds `daemon.lock` with no live Worker reaps first: the Watchdog after every Worker exit and at start-up, and `claudeclaw stop` on every path (even with nothing running). It reads the Run registry (`daemon/state/runs.json`), which records every spawned process with an OS start-time stamp (`proc_pidinfo` on macOS; `/proc` `starttime` + `boot_id` on Linux). On Unix only process groups proved ours by that stamp get TERM, then 5 s, then KILL; unprovable leftovers are reported (`leftover_runs`), not killed. On Windows process-wrap's kill-on-close Job objects end runs in the kernel. A Reap of a `settling` entry never makes its job or chat `interrupted` (#37).
- **Update.** On update it re-execs the new binary with the lock fd's `FD_CLOEXEC` cleared, so the lock is never released (Unix). On Windows it spawns the new binary, which waits up to 30 s for the lock, and then exits. The CLI's `update` re-execs the unresolved `PATH` location from `argv[0]` (the brew symlink), never `current_exe()` (#22, #36).
- **Service managers** (#45). `--service-manager launchd|systemd` names the Service manager. Under launchd the final stops (Give-up, Held start, lock held) exit 0, because `KeepAlive {SuccessfulExit:false}` would otherwise relaunch every 10 s forever. Example unit files:
  - plist: `KeepAlive {SuccessfulExit:false}`, `ExitTimeOut 45` (launchd's default is 5 s);
  - systemd: `Restart=on-failure`, `RestartPreventExitStatus=70 73`, `KillMode=mixed`, `TimeoutStopSec=45s`; `sd-notify` sends `READY=1`/`STOPPING=1`.
- **Watchdog exit codes:**

  | Code | Meaning |
  |---|---|
  | 0 | Stopped through the control channel. Under `--service-manager launchd`, also every final stop below (Give-up, Held start, lock held) (#45). |
  | 75 (`EX_TEMPFAIL`) | External signal or recoverable failure, so restart. On SIGTERM, after draining the Worker (#27). |
  | 70 (`EX_SOFTWARE`) | Gave up, or a Held start (#45) |
  | 73 (`EX_CANTCREAT`) | Lock held by another Daemon (after the 1 s retry). Prints its pid. |

- **CLI exit codes** (#22): 0 / 1 error / 2 usage / 3 not running / 4 not responding / 5 gave up / 6 foreign daemon / 124 wait timeout. `start` keeps the Watchdog's 0 / 70 / 73 / 75.

**Statusline** (#24). Kept as `claudeclaw statusline`, which Claude Code runs in an interactive session opened in a Daemon's project dir.
- It reads `/v1/status`, falling back to `offline_status`. It always exits 0, within a 500 ms budget, because Claude Code blanks the line on a non-zero exit (~4 ms measured, against TS's ~27 ms).
- Its dir order puts stdin's `workspace.project_dir` before `CLAUDECLAW_DIR`.
- `statusline install [--force]` and `statusline uninstall` patch only the `statusLine` key of `.claude/settings.json` (with `refreshInterval: 30`), never over a foreign value (`statusline_foreign`, a CLI-only error). The First-start import installs it once for fresh dirs too.

### `claudeclaw-api`

- **Owns:**
  - every Management API request/response type, and the SSE event enum (`DaemonEvent`);
  - the error envelope, the route path constants and the `Scope` wire form;
  - the `api::Deadline` table beside the route constants, shared by the app, the CLI and the server (#40), and `x-claudeclaw-deadline` in `openapi.json`;
  - `SupervisorStatus` (the shape of `supervisor.json`) and `DaemonReachability` (the 9 local / 7 remote not-answering states, #20), because the client reads them for a stopped local Daemon (see `client`);
  - `RunRegistry`, the shape of `runs.json` (`worker`, `state`, `stamp`, `kill`, `purpose`, `origin`, `leftovers`, `reaped`; #29);
  - the `PairingString` parser, shared by the CLI and the app; its only output is `{code, fingerprint, port, host?}` (#41);
  - the Bug-report export types `ExportManifest`, `LogExport`, `ExportSection`, `SETTINGS_VALUE_ALLOW` (#28), and the two Field class tables (#69):
    - `FIELD_CLASSES` for Daemon log and Run index fields, which `store::export` scrubs by;
    - `APP_FIELD_CLASSES` for App log fields, whose class can only be `Structural` or `Daemon`, because the App log is never scrubbed, only declared content-free (#40 D3);
  - `PseudonymKind`, including `daemon`, used only in the Desktop app section (#40, #69);
  - the event catalogues `log_event::daemon::*` and `log_event::app::*`, each event declared with its field list, and `UiLogEvent` (#40, #69);
  - `RunLocale` (#43).
- Types derive `serde` + `utoipa::ToSchema`.
- `crates/api/openapi.json` is generated and committed. `cargo xtask openapi --check` fails CI if it is stale. Snowflakes are `String` on the wire.
- **CI checks** (#28, #69):
  - a `claudeclaw-api` test fails if a declared Daemon log field has no `FIELD_CLASSES` entry, or an App log field no `APP_FIELD_CLASSES` entry;
  - the canary tests add a tracing layer that fails when an event emits a field outside its declared list. The Daemon side runs in the parity canary, the App side in #40's app leg.
- **No** tokio, axum or IO.

### `claudeclaw-store`

The only crate that knows paths under `<project-dir>`. It is **synchronous** (`std::fs`, small files), and async callers wrap it in `spawn_blocking`. It owns:
- **`Layout`**: every path in §9, plus the socket-path rule, and `Layout::claude_settings()` for `.claude/settings.json` (#24).
- **Atomic writes**: `tempfile::NamedTempFile::persist` in the same dir, then `fsync`. The permission modes from §9 are applied before the rename.
- **Settings**:
  - A typed read view over the raw JSON.
  - Writes are **patches applied to a freshly read `serde_json::Value`** (`preserve_order`). The file is never re-serialised from a typed struct. This keeps unknown and TS-only keys, key order, and integers above 2^53 exact (verified in §12), as #14 rules 1–2 require.
  - Snowflakes the Daemon writes are JSON strings (rule 4).
  - Secrets are `secrecy::SecretString` (#23).
  - The single-key `statusLine` patch for `.claude/settings.json` (#24).
- **`scan`** — the **Change scan** (#46), with no `notify` watcher: `FsView`, per-OS stat keys and directory listing. Every 2 s a stat pass (ctime is part of the key) and every 30 s a SHA-256 pass run over the job dirs and `settings.json`.
  - A change is applied only once it is stable across two passes; a file counts as removed only after two passes without it.
  - Dot-prefixed `.md` files are ignored (they are not Jobs).
  - The Daemon's own writes re-stat before the rename (`jobs::write`'s check), then update the snapshot directly.
  - It records when a job's current schedule took effect (the file's mtime, clamped between the previous pass and the detection), which catch-up needs (#50).
- **Sessions**:
  - the Rust (Scope, Launcher) store in `daemon/state/sessions.json`, including per session `transcript`, `context` and `cost_state` (#39), `timeout_breaker` (#32) and `auto_compact` (#33);
  - the TS mirror (`session.json`, `sessions.json`, `agents/<agent>/session.json`) for rule 7;
  - backups (`session_<N>.backup`) and fallback sessions, in their TS shapes.
- **Job state**: `daemon/state/jobs.json` (#9), with `revision` and `orphaned_since` per job (#46).
- **Run logs** in the TS pattern (rule 5), and the **Run index** `daemon/runs/<YYYY-MM>.jsonl` (#23), with its Back-fill (§5) and month compaction (#44).
- **Run registry** file `daemon/state/runs.json` (#22, #29); `core::runs` maintains it through a spawn hook, and `stop --force` and every Reap read it. An entry's `state` gains `settling` at leader exit and the entry is removed once settled (#37).
- **The audit log** (#13).
- **`logs`**: reading Daemon logs for `claudeclaw logs --offline` as `api::LogEntry` NDJSON (#20, #23).
- **`export`**: the Bug-report export (#28), shared by `claudeclaw logs export` and `GET /v1/logs/export`. One reviewable Markdown file: versions (including the Claude Code version, #42), status, the settings key shape, job schedules, the Crash record, Run index and 24 h of Daemon log. Identifiers become `<kind>:#n` Pseudonyms, stable within one export and unlinkable across exports. Run logs, the audit log and settings values are never included. It fails closed (`export_leak`) if an identifier survives. Defaults: 24 h, 10 MiB, maximum 24 MiB.
- **Lock and pid helpers**, including the holder record for Offline writes (#22).
- **`store::rollback`**: the cutover-only behaviour (§8).

### `claudeclaw-jobs`

Pure job-file logic from #12. No tokio.
- **Reading**: envelope split, serde-saphyr into `BTreeMap<String, Option<String>>`, and the strict value grammar.
- **Cron**: croner 4 configured as #12 §1 says.
- **Validation**: `JobDiagnostic` (error/warning, line/column), with a stable `kind` such as `schedule.invalid` or `key.unknown`, so the Bug-report export can drop the message and keep the meaning (#28).
- **Edits**: `LineEdit` operations (set key, insert after, comment out), applied to text. Writes never re-serialise.
- **Lint hook**: a `Lint` trait, so `ts-compat` can add the 1.x divergence lint without `jobs` knowing about TS.
- `jobs` does not read the filesystem. `store` hands it bytes.

### `claudeclaw-runner`

The Claude Code runner and stream parser. It knows nothing about Scopes, sessions or chat.
- **`launcher`**:
  - `Launcher = Direct | Path(AbsPath)`. A Launcher carries no name and no env of its own; any env lives in the user's script (#8). `direct` never imports a `CLAUDE_CONFIG_DIR` set outside the Daemon; a non-default Config dir comes from a Launcher script such as `examples/launchers/config-dir.{sh,cmd}` (#48).
  - `resolve(launcher, run_path)` (formerly `check_runnable`, #26): the file exists, is a regular file and is executable, using `which::which_in` on the Run PATH. `direct` and shebang interpreters are resolved by the runner itself, and the runner always spawns the absolute path. On a miss it rebuilds the Run PATH (at most once per 60 s) before refusing. Errors never include the path's arguments or contents (#8).
  - **Windows** (#25): Launchers are `.exe`/`.com`/`.cmd`/`.bat` only, with the `%` and UNC rules, no PowerShell and no `PATHEXT` search. `direct` looks for `claude.exe`, then `claude.cmd`, then `claude.bat` on the Run PATH. Spawns add `CREATE_NO_WINDOW`.
  - the **Test launcher** procedure: one small run through the same runner. Its checks are `runnable`, `print_stream_json`, `clean_stdout`, `system_prompt`, `result_event` (#16, #22), `prompt_stdin` (#25), `claude_code_version` (#42), the advisory `transcript` check (#39), and a `stragglers` warning (#37). The `runnable` detail shows the Run PATH source and the Run locale (#43).
- **`argv`**:
  - A typed `ClaudeArgs` builder (output format, `--resume`, `--append-system-prompt-file`, `--model`, security args, permission mode).
  - Built-in guards: always `--print`, never `-p`; always `stream-json --verbose` on the main path; `argv_unsafe` and `argv_too_long` (#25).
  - **On every OS the prompt is written to the child's stdin**, and the appended system-prompt text goes in a per-run 0600 **System-prompt file** under `daemon/tmp/`, passed as `--append-system-prompt-file` (#25). Argv holds only short, single-line tokens, which makes `.cmd`/`.bat` Launchers safe and removes argv size limits. There is no inline fallback.
  - The executable is **not** part of `ClaudeArgs`. It is prepended once, at spawn. That makes TS trap 7 (double-wrapped retries) unrepresentable.
- **`env`** — the run environment (#26, #43, #47). The Worker builds `RunEnv{path, lc_ctype}` and every spawn applies it; the Worker's own env is never mutated.
  - **Run PATH**: `prepend ++ source ++ inherited-not-present`, dropping empty and relative entries. Sources are `auto | login_shell | registry | inherit`. The login-shell capture runs `<passwd shell> -l -i -c "'<exe>' __env-probe <nonce>"`, with cwd home, stdin null and a 10 s timeout that kills the process group (`getpwuid_r` from `nix`); Windows reads the registry. Only PATH is imported. It is rebuilt at Worker start, on a `run_path` change, before Test launcher, and on a miss (at most once per 60 s). `import --dry-run` builds it the same way, so the dry run may run the user's rc files, but ClaudeClaw itself still creates nothing.
  - **Run locale**: if neither `LC_ALL` nor a UTF-8 ctype is inherited, every spawn gets `LC_CTYPE=C.UTF-8`, checked with `locale charmap`, falling back to `UTF-8` then `en_US.UTF-8`. `LANG` and collation are left untouched, and nothing is imported from the login shell. Windows: no-op. `run_locale: inherit` restores the 1.x C locale.
  - Stripped: `CLAUDECODE`, `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST`, and for `direct` runs also `CLAUDE_CONFIG_DIR`, so a value set outside the Daemon (a shell, a service unit) never changes a `direct` run's Config dir. A non-default Config dir comes from a Launcher script (#48).
  - Set: `CLAUDECLAW_DIR` (so a `claudeclaw` CLI called from inside a run targets the right Daemon, #22), `CLAUDECLAW_RUN_ID` (#29) and `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` (#47).
- **`clock`** — the Worker-wide Awake clock (#38): `AwakeClock`, `SuspendClock` (macOS `CLOCK_MONOTONIC`, Linux `CLOCK_BOOTTIME`, Windows `QueryInterruptTime`) and the Wake grace. It ticks every 1 s in steps capped at 2 s. A Resume is a jump of 10 s or more on the suspend-counting clock. After a Resume a running run gets at least 120 s more, capped at 120 s extra over its lifetime.
- **`spawn`**:
  - `tokio::process::Command` + `process-wrap` 10: `ProcessGroup` on Unix, `JobObject` on Windows, `KillOnDrop` (required in the wrapper stack for kill-on-close on Windows, asserted by a unit test, #36);
  - a 10 MB stdout/stderr cap;
  - **timeout** on Awake time, never wall time; every run deadline, compact included, is set on the Awake clock (#38). A `result` read before the timeout wins: the run counts as successful, not as a timeout (#47). Otherwise TERM, then KILL after 5 s, on the whole group. **TERM before KILL is an invariant**, because SIGKILL orphans background tasks (#47).
  - **Settle** (#37), not configurable: leader exit is detected with `inner_mut().wait()`, never the wrapper's `wait()`, which never reaches Stragglers. After a normal leader exit, Stragglers in the run's process group (Job object on Windows) get a 2 s grace, then TERM, 5 s, KILL (`killpg`; `TerminateJobObject` on Windows). After a normal exit or a timeout kill, the pipes are capped at 2 s. The reply goes out at leader exit; the Scope's next Turn waits until the run is settled (≤14 s). The System-prompt file is deleted at settle (or at a Reap). Bash-tool commands already run in their own process group on macOS, so Settle doesn't reach them and its default is unchanged; on Windows the Job object still stops them (#47).
  - **Exit 126/127 with no JSON line** is a Refused run (`launcher_exec_failed`, #26).
- **`reap`**: the Reap mechanism the Watchdog and `stop` use (#29).
- **`stream`**: a **pure** NDJSON parser with no tokio in the module.
  - Input is `LinesCodec::new_with_max_length` lines.
  - Each line becomes a `#[serde(tag = "type")]` enum with a catch-all variant.
  - Non-JSON lines are kept as `StreamItem::Noise` for the run log, not dropped.
  - It reads the `init` event's `claude_code_version` (#42) and emits a `TranscriptHint` from `init.memory_paths.auto`, with a shape check (#39).
  - `judge()` returns `RunOutcome`: no `result` event means failed, even on exit 0. Errors come from `result.is_error` / `api_error_status` (#5, #8). It recognises `session_not_found` (#39). `unknown option` from Claude Code is a Refused run (`claude_code_unsupported_flag`, #42).
  - #18's golden transcripts and Recorder launcher fixtures test this module directly.
- **Minimum Claude Code** (#42): `MIN_CLAUDE_CODE = 2.1.214`. It counts `--append-system-prompt-file` (2.0.33), `api_error_status` (2.1.110), `refreshInterval` (2.1.97) and upstream stream-json fixes for multibyte corruption and missing results. The version comes from `init`; there is no `--version` probe. Below the minimum, runs still go ahead with a `claude_code_outdated` warning.
- **Output**: `RunEvents`, a stream of text deltas, tool use, tool results, `session_id` and the final `RunOutcome`, which carries `result.usage`, `modelUsage` (cost, tokens, `costBasis`, `provider`, `contextWindow`), `init.apiKeySource`, the last main-thread assistant `usage` (#39), `awake_ms`, `suspended_ms` and `wake_grace` (#38).

### `claudeclaw-scheduler`

The timer-based scheduler from #9 and #12.
- **Inputs**: the job set (`jobs`), a `JobChange` stream from the Change scan (#46), job state (`store`), the time zone offset, and a `watch::Receiver<AwakeTime>` (#38).
- **Output**: calls to a `Dispatch` trait that `core` implements.
- **Owns:**
  - next-fire computation, and catch-up through `find_previous_occurrence` within the 6 h window;
  - **catch-up only covers slots after the job's current schedule took effect** (#50). A daily 09:00 job created or edited at 10:00 first runs the next day at 09:00. A One-shot job whose single slot passed only shortly before it took effect still runs (a build-time grace, ~10 min). Catch-up for slots missed while the Daemon was down is unchanged. Run now is the explicit way to run a job immediately;
  - the in-flight guard and retries (persisted); frontmatter changes for a job in flight are held back until the guard has run (#46);
  - the One-shot finish (`enabled: false`, from #12 §3);
  - interrupted-job re-run (#9), only when the Reap proved the run dead (#29);
  - the heartbeat timer with exclude windows and the rate-limit skip rules.
- **Timing**:
  - Deadlines are computed on the **wall clock** (`chrono::Utc::now()` + `FixedOffset`). Run timeouts are not the scheduler's; they use Awake time (§4 `runner`).
  - Each sleep is capped at 30 s, and due jobs are re-checked on each Resume (#38). Tokio's monotonic clock does not advance during system sleep on macOS or Linux, so after a wake or a clock jump the schedule is re-checked within 30 s. That is #9's "re-check after sleep/wake", without a fixed 60 s tick.
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
  - the (Scope, Launcher) session lifecycle, and per session `transcript`, `context` and `cost_state` (#39), with the root-moved check (notice `launcher_root_moved`);
  - recovery policy: stale session, thinking-signature, rate-limit fallback, and **timeout recovery**. For a resumed session's timed-out Turn, the skip order is new session → Timeout breaker → `auto_compact.enabled` → budget (#33). Otherwise Auto-compact runs `/compact` with its own timeout, `min(auto_compact.timeout_secs, run timeout)`, then re-runs the Turn once, **at most once per Failing stretch** (#33). A skip logs `run.recovery_skipped` with its reason (`new_session`, `timeout_breaker`, `disabled`, `budget_spent`). The Turn's timeout reply carries one line saying what Auto-compact did;
  - the **Timeout breaker** (#32), opt-in and off by default: one per (Scope, Launcher) session, persisted. Only the runner's own timeout kill counts (a Launcher's exit 124 doesn't, nor does a `lingered` run, #47). Once open, it skips Auto-compact and retry but never refuses or delays a Turn, and every opening raises an owner notice. It closes on the session's next successful Turn (TS reset after one cycle instead). `max_failing_seconds` and `spent_at` stay on the wall clock (#38);
  - rotation and summaries;
  - orphan pruning: an Orphaned job's sessions and job state are kept for 30 days, then pruned (#12 §6, #46); thread Scopes whose Discord channel answers `Unknown Channel` are orphaned the same way, and their TS mirror entry is removed after 30 days (#44).
  - These are the decisions. The mechanisms are in `runner`.
- **`conversations`**:
  - one serial queue per Scope, a tokio task per active Scope that exits when idle; the next Turn starts only once the previous run is settled (#37);
  - turning a `Turn` into a run: prompt shaping (clock prefix, `<untrusted-…>` wrapping, the System-prompt file text from `CLAUDE.md`), model choice (including agentic routing), and the timeout category;
  - interrupted-chat replies on shutdown (#9) and after a crash once the Reap proved the run dead, which also clear stuck reactions (#29). Both go through `Notifier::interrupted(origin)`: the adapter replies to the origin message and removes its own progress reactions. After a crash the origin comes from the Run registry's `origin`, which `core` fills from `Turn.origin` at spawn (#79).
- **`runs`**: the Run registry, maintained through a spawn hook (#29).
- **`health`** (#27): a stall monitor that watches beacons: `scheduler`, `adapter:<transport>` and `outcome_sink` (5 min each), and `turn:<scope>` (each run's own timeout + 120 s Wake grace + 5 s + 2 min, re-armed by every run in the Turn, #33, #38). It ticks every 5 s and accrues quiet time in steps capped at 10 s. A Stall logs `worker.stalled`, sends `fatal("stall: …")`, shuts down gracefully and exits 75; the Watchdog counts it.
- **`housekeeping`** (#44): one daily Housekeeping task. It runs 10 min after Worker start, then every 24 h of wall clock, and after a Resume if a pass is overdue; after a First-start import the first pass waits 24 h. A pass deletes **only files the Daemon wrote itself, under a listed rule**, in this order: Inbox (age 30 days, a maintainer decision, then a total cap; files younger than 24 h or attached to a queued or running Turn are protected), opt-in Run logs, Run-index compaction, opt-in audit, session backups (keep the newest 20), orphaned Scope state, and `notices.json` entries unseen for 30 days. Counts go to the Daemon log (`housekeeping.done`) and one audit entry per kind (`principal: system:housekeeping`). `claudeclaw housekeeping [--dry-run]` and `POST /v1/housekeeping` preview or run a pass.
- **`disk`** guard (#44): free space on the project dir's volume is checked every 10 min and before each attachment write. Below `disk.warnFreeMiB` the problem `disk_low` is raised (health `degraded`, one owner notice). Below `disk.minFreeMiB` attachment intake is refused (Discord runs the Turn without the file; the API answers `507 insufficient_storage`). Runs are never refused, and the guard never deletes anything.
- **`notify`**: owner notifications (job results, heartbeat output, invalid jobs at one per distinct content, Launcher refusals, `device.paired` / `device.revoked` (#41), the unplanned-restart notice (#27), breaker openings (#32), `job_frontmatter_restored` (#46), `disk_low` (#44), `run_locale_unavailable` (#43), `launcher_root_moved` (#39)), fanned out to each adapter's `Notifier` and to the event bus for the Desktop app.
- **`events`**: the event bus, a `tokio::sync::broadcast` of `api::DaemonEvent` that feeds SSE.
- **`settings`**: reload through the Change scan (§7).

### `claudeclaw-discord`

The Discord adapter, built on twilight 0.17 (`twilight-gateway`, `twilight-http`, `twilight-model`) plus `reqwest` for attachment downloads.
- **Owns**:
  - the gateway loop, slash-command registration and interactions;
  - thread tracking and thread Hire/Fire, where the intent classifier is a utility run through the seam. Fire, from Discord or the app, moves the thread's agent folder to `agents/.archive/` (#16, #21);
  - trigger rules and allow-lists;
  - attachment intake to `inbox/discord/` (every attachment handed over as a file, 25 MB cap, as in #1; refused below the disk floor, #44);
  - tool-category reactions, the streaming preview, 2000-char splitting and image upload from the outbox;
  - the `chat::AdapterControl` implementation (§6, #21).
- Its constructor takes `DiscordEndpoints { gateway, rest }` so the Parity suite's replay can redirect it. It is not a settings key (#18).
- **Exports** `notify_owner(token, user_ids, text)`, a REST-only call for the Watchdog's Give-up and Held-start DMs.
- **Depends only on `chat`.**

### `claudeclaw-server`

The Management API server (#13, resources in [`management-api.md`](management-api.md)): axum 0.8 routers, with paths declared through `utoipa-axum`.
- **Listeners**:
  - **Local.** A Unix socket bound at a temp name, `chmod 0600`, then renamed into place. Clients therefore never see a socket with wider permissions. On Windows, a named pipe created with `first_pipe_instance(true)`, `reject_remote_clients(true)` and an owner-only DACL (`create_with_security_attributes_raw` + `windows-sys`).
  - **Remote**, opt-in. `tokio-rustls` with a custom `ClientCertVerifier`: client certs are optional, any self-signed cert proves key possession. The HTTP layer then authorises by the cert's SPKI-SHA-256 against `devices.json`.
- **Principals.** The same router serves every listener, behind an auth layer that yields one `Principal`:
  - `Local`: any caller on the socket or pipe;
  - `Device(id)`: a pinned client cert;
  - `ApiToken(id, scopes)`: a bearer token, SHA-256-hashed at rest and compared with `subtle`;
  - `Pairing`: an unknown cert, allowed only on `/v1/pair`.
- **Pairing codes** (#41): 8 Crockford base32 characters from `getrandom`, shown as `XXXX-XXXX`, in memory only, valid for 10 min and burned after 5 wrong attempts. Codes are only created locally. `pair` also prints the **Pairing string** `ccpair1.<code>.<fingerprint>.<port>[.<host>]`, which the app pins before it connects.
- **Deadlines** (#40): a deadline middleware enforces each route's `api::Deadline` as a backstop (`504 deadline_exceeded`); streams use an idle limit instead.
- **Detach** (#40): run-starting handlers detach, because hyper drops a handler as soon as its client disconnects (logged as `api.client_gone`). Cancel therefore means "stop waiting", and in v1 no client action kills a run.
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
  - `offline_status(project_dir)`, which reads `supervisor.json` and `daemon.pid` read-only and runs a non-creating shared-lock probe when the local socket is down. It returns `api::DaemonReachability` (#20), so the app and `claudeclaw status` tell "restarting", "gave up" and "stopped" apart without the API;
  - `releases`: release lookup plus the channel rule (the Update channel is implied by the running version), shared by the CLI, the Worker and the Desktop backend (#35);
  - the per-command Deadlines from `api::Deadline` (#40).

### `claudeclaw-ts-compat`

See §8.

### `claudeclaw-desktop`

The Tauri 2 backend, with the React/TS/Vite frontend in `ui/`.
- **Links** only `client` + `api`.
- **Bundle identifier** `io.github.winter-wub.claudeclaw`, set as `identifier` in `tauri.conf.json` (#40). `tauri.conf.json` omits `version` (#35).
- **Tauri commands:**
  - `daemons_*`: the list of local and remote Daemons, stored in the app data dir;
  - `api_call(daemon, call_id, method, path, query, body)`, `api_subscribe(daemon, call_id, path, Channel<SseEvent>)` and `api_cancel(call_id)`, each bounded by its `api::Deadline` (#40);
  - `app_log` for the App log (#40);
  - `daemon_start` / `daemon_stop` / `daemon_attach`: process duties for local Daemons (#9, #10);
  - `pair(PairTarget)`, where `PairTarget` is a Pairing string plus an optional host, or a host, port and code. For a Pairing string the pin is set before connecting, and a pin mismatch is an app-side error that sends nothing (#41);
  - two fixed-argv offline commands on the bundled binary, the only read paths outside the Management API (#20, #28): `daemon_offline_logs(daemon)` runs `claudeclaw logs --offline --json`, and `daemon_offline_export` runs `claudeclaw logs export --offline --out -` (stdout, #40). Both are read-only.
- **Offline process actions** go through the bundled `claudeclaw start` / `stop` (#20). Restart anyway runs `start -d --clear-give-up`; for a local Daemon with a recorded Service manager it becomes Clear give-up (#45). Local Restart anyway is never automatic.
- **Local Daemon start.** The bundled `claudeclaw` binary (Tauri `externalBin`) is spawned **detached in its own session/process group by the app's own code**, not by `tauri-plugin-shell`. Quitting the app therefore leaves the Daemon running (#10). The env it spawns with drops AppImage vars and `$APPDIR` entries, and on macOS `__CFBundleIdentifier` (#26).
- **App log** (#40): content-free `tracing` JSONL in Tauri's `app_log_dir()`, 14 daily files capped at 32 MiB a day, with no in-app viewer. JS errors log only the error name and stack frames. `tauri-plugin-log` is not used. The App log joins the Bug-report export as an app-appended `## Desktop app` section behind a fail-closed verify pass, with a `daemon` Pseudonym kind; the app also offers an app-only export.
- **Device keys**: ECDSA P-256 from `rcgen`, private key in the OS keychain via `keyring` 4.
- **Plugins**: `tauri-plugin-notification`, `-autostart` (the app is the only autostart, #9), `-single-instance`, `-updater`, `-dialog` (Launcher file picker). Tray uses Tauri's built-in `tray-icon`.
- **Updater** (#35): minisign-verified with `requireSignedVersion: true`; a failed check retries with the backup key.
- **Self-test** (#36): hidden `--selftest spawn-env | daemon | verify-update` modes run headless in CI, inside a temp dir.
- **Generated TS client**:
  - `openapi-typescript` 7 turns `crates/api/openapi.json` into `ui/src/api/schema.d.ts` (committed; CI regenerates and diffs it).
  - The UI calls the API through `openapi-fetch` with a custom `fetch` that routes through `api_call`.
  - One typed contract therefore covers local and remote Daemons, and the webview never opens a socket or holds a key.

### `claudeclaw-parity`

Never released (`publish = false`, #18). It holds the `claudeclaw-recorder` binary (the Recorder launcher) and the differential test harness. The top-level `parity/` directory holds the corpus, scenarios, generated fixtures, the Divergence registry (`divergences.toml`), `capture-check.json` and the bun generators that run the unmodified TS code to produce expected outputs. Every Rust≠TS difference must match a `divergences.toml` entry citing a closed ticket. Rows decided so far come from #16/#21 (Fire archives the agent folder, on the Discord path too), #25, #33 (five rows: the `auto_compact` settings key, once per Failing stretch, the capped compact timeout, the retry's own Run log, the outcome line in the reply), #37, #39, #43, #44 (Inbox deletion, the session-backup cap, the TS mirror entry removed for orphaned threads), #46, #47 and #50.

### `xtask`

- `openapi`: export or check `openapi.json`.
- `deps`: enforce §3.1.
- `ts-types --check`: run `openapi-typescript` and diff.
- `release prepare|tag` (#35).
- `verify`: enforce the Verification register (#36).

## 5. Inside the Worker

| Task (tokio) | Owner crate | Notes |
|---|---|---|
| Awake clock | `runner::clock` | Started before anything else (#38) |
| Control link to the Watchdog | `claudeclaw` | Two OS threads, `control-r` / `control-w`, carrying stdin/stdout JSON lines. EOF triggers graceful shutdown, and `abort()` after 45 s (#27). |
| Change scan | `core::settings` over `store::scan` | 2 s stat pass + 30 s hash pass over the job dirs and `settings.json`, with the stability rule; the Daemon's own writes are applied directly (#46) |
| Scheduler + heartbeat | `scheduler` | Dispatches into `core::conversations` |
| Conversations | `core` | One task per active Scope, plus an outcome sink that updates sessions, mirrors, run logs, the Run index and notifications |
| Chat adapters | `discord` (later `telegram`, `slack`) | One task each, behind `chat::Adapter` |
| Management API | `server` | Local listener always; remote listener when enabled |
| Stall monitor | `core::health` | Beacons, 5 s tick (#27) |
| Back-fill | `store` | In the background after `ready`, newest month first, month by month, with `RunIndexState` progress and no cap (#44). Adds an `origin: ts` record for every TS-pattern Run log not yet indexed; a TS exit 124 back-fills as `timeout`. |
| Housekeeping and disk guard | `core::housekeeping` | Daily pass, 10 min disk check (#44) |
| Update check | `claudeclaw` | Once a day; reports only (#6) |

- **Shutdown** uses one `CancellationToken` and one `TaskTracker`:
  1. stop intake (adapters, scheduler, API mutations);
  2. wait up to 30 s for runs;
  3. kill the remaining process groups, including `settling` ones, whose grace is skipped (#37);
  4. persist `interrupted` job state;
  5. send interrupted-chat replies;
  6. report `stopping` to the Watchdog.
- **Signals** come from `tokio::signal` (`unix::SignalKind::terminate/interrupt/hangup`; `ctrl_c` and `windows::ctrl_*` on Windows).
- **sd-notify 0.5** sends `READY=1`/`STOPPING=1` under the example systemd unit on Linux. It is a no-op elsewhere.
- **First start on a TS data dir** (#17): a copy-only First-start import runs in the Worker before anything else, while the API already serves (`health.state: starting`, mutations `503 daemon_starting`). See §8.

**Run pipeline:**
1. A `Turn` comes in from an adapter, the scheduler or the API, carrying a Scope, an origin, prompt parts, attachments and overrides.
2. `core` resolves the Launcher for the Scope on the Run PATH. A Refused run (missing, not executable, fails to start, exit 126/127 with no JSON, or `unknown option`) becomes an immediate failed outcome and a notification, with no fallback (#8, #26, #42).
3. `core` looks up the session for (Scope, Launcher) and builds `ClaudeArgs`, the stdin prompt and the System-prompt file.
4. `runner` runs it with the run environment and streams `RunEvents` back to the caller.
5. The outcome updates sessions and the TS mirror, writes the Run log and a Run-index record, and notifies. The run is Settled before the Scope's next Turn starts.

**Run index** (#23). A Rust-only `daemon/runs/<YYYY-MM>.jsonl` holds one `RunRecord` per spawned run, back-filled from TS run logs. Every run in a Turn appends its own record — the first attempt, the compact and the retry — sharing the Turn's `turn_id`, with `attempt` counting up and `purpose: turn | compact | compact_retry` (#33). The auto-compact retry writes its own Run log; compact runs write none (#33). `RunRecord` fields added by later tickets are listed in [`management-api.md` §2.9](management-api.md#29-logs-runs-and-audit).

**Cost** (#39). `total_cost_usd` is session-cumulative on `--resume`, so per-run `cost_usd` = reported cumulative − the session's Cost baseline. Usage rows carry `cost_trust` (`api_list` / `managed` / `estimate`) and are never "corrected". Usage is no longer read from Transcripts, and context comes only from the Daemon's own stream.

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
    pub origin: MessageRef,                // the message this Turn answers; core keeps it in the Run registry (#79)
    pub author: Author,                    // display name only; IDs stay in the adapter
    pub text: String,                      // raw user text; core does the untrusted-wrapping
    pub context_lines: Vec<String>,        // "[In reply to …]", forwarded text, channel tag
    pub attachments: Vec<LocalFile>,       // already downloaded into inbox/<transport>/
    pub agent_dir: Option<String>,         // thread cwd under agents/ (TS parity)
}

pub struct MessageRef { pub channel: String, pub message: String }  // opaque transport IDs; = api ChatOrigin minus the transport (#79)

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
    fn interrupted(&self, origin: MessageRef) -> BoxFuture<'_, Result<(), NotifyError>>; // #9 reply + clear own reactions (#29, #79)
}

pub trait AdapterControl: Send + Sync {    // implemented by each adapter, registered beside Notifier (#21)
    fn overview(&self) -> BoxFuture<'_, Result<AdapterOverview, AdapterError>>;     // guilds, channels, threads, names from the gateway cache
    fn available_channels(&self) -> BoxFuture<'_, Result<Vec<ChannelInfo>, AdapterError>>;
    fn hire(&self, channel: String, names: Vec<String>) -> BoxFuture<'_, Result<Vec<ThreadInfo>, AdapterError>>;
    fn fire(&self, thread: String) -> BoxFuture<'_, Result<FireDone, AdapterError>>;
}

pub trait Adapter: Send + 'static {
    const TRANSPORT: Transport;
    fn notifier(&self) -> Arc<dyn Notifier>;
    fn control(&self) -> Arc<dyn AdapterControl>;
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
| Transport calls for Hire/Fire and the channel overview (`AdapterControl`) | The session and `agents/` work for Hire/Fire, including the archive move |

**Scope keys.**
- Rust-only state uses unambiguous typed keys: `discord:<id>`, `job:<name>`, `agent:<name>`, and later `tg:<…>` and `slk:<…>`.
- `store::rollback` translates them to TS keys for the mirror and for `runtime.launchers` reads. Discord channels and threads become bare snowflakes, jobs become the bare name, agents stay `agent:<name>`, and `tg:`/`slk:` keep their TS prefixes. An agent's job (`job:<agent>/<name>`) mirrors to `threads["agent:<agent>"]`, the key TS runs every job of that agent under, so the agent's jobs share one TS entry (last writer wins), as in TS. `agents/<agent>/session.json` is the agent's own session, used for agent runs with no thread.
- This removes TS's ambiguity between a job name and a channel ID (trap 5) without changing any TS file.

**Phase 2.**
- Telegram and Slack become `crates/telegram` and `crates/slack`, each implementing `Adapter` against `chat` only.
- Each sits behind a cargo feature of the binary (`discord` is on by default; `telegram` and `slack` come later).
- Their crate picks are deferred to the phase-2 tickets. Nothing in `core` changes to add them.
- Each must declare its Inbox filename pattern, or Housekeeping won't delete its files (#44).

## 7. Config and hot reload

- **The single config file stays `.claude/claudeclaw/settings.json`** (drop-in).
- Rust-only settings are **new top-level keys** that TS ignores and preserves (#14 rule 1):

  | Key | Shape and default | Ticket |
  |---|---|---|
  | `launcher` | `{ "default": "direct" \| "<abs path>", "scopes": { "<ts scope key>": "direct" \| "<abs path>" } }` | #11, #17 |
  | `remote` | `{ "enabled": false, "listen": "<addr>", "port": <n> }` | #11 |
  | `run_path` | `{ "source": "auto" \| "login_shell" \| "registry" \| "inherit", "prepend": [] }` | #26 |
  | `run_locale` | `"auto"` (default) \| `"inherit"` | #43 |
  | `timeout_breaker` | `{ max_consecutive_timeouts, max_failing_seconds }`; opt-in, off by default | #32 |
  | `auto_compact` | `{ "enabled": true, "timeout_secs": 300 }` | #33 |
  | `transcript_roots` | `[abs dir]`, default `[]`; the fallback for locating Transcripts | #39 |
  | `inbox` | `{ "keepDays": 30, "maxMiB": 1024 }`; `null` means keep forever / no cap | #44 |
  | `disk` | `{ "warnFreeMiB": 2048, "minFreeMiB": 256 }` | #44 |
  | `logging` | `daemonKeepFiles` (14), `daemonMaxDailyMiB` (256), `daemonMaxTotalMiB` (1024, #44), opt-in `runLogKeepDays` and `auditKeepMonths` | #23, #44 |

  - An invalid field takes its default and is reported once as `settings.invalid` (#33).
  - TS keys (`runtime.launcher(s)`, `web.*`, `apiToken`, `watchdog.*`, …) are read for import only and never rewritten (rules 2–3, #32). Reading `watchdog.*` logs `settings.legacy_key`.
- **Settings precedence** (highest first):
  1. env (`DISCORD_TOKEN`, as TS);
  2. `settings.json`;
  3. built-in defaults.
- **Reload** goes through the Change scan (#46): a change to `settings.json` applies once it is stable across two passes, and the Daemon's own writes apply immediately.
  - A parse error keeps the previous settings and is reported once per distinct content. It is never silent.
  - A changed Discord token restarts only the adapter.
  - `remote.*` changes rebind the remote listener.
  - A `run_path` change rebuilds the Run PATH.
- **Alternative rejected:** a file watcher (`notify` 8). Probes showed FSEvents coalescing event kinds, a bind mount hiding deletes, and a 58 ms truncated window during an in-place save (#46). The scan's 30 s hash pass bounds any miss.

## 8. Where the TS-compat code lives

Two places, with different lifetimes. Each can be removed in one PR.

**`crates/ts-compat` (`claudeclaw-ts-compat`)**, as #12 asked:

| Module | What | Used by | Removed |
|---|---|---|---|
| `oracle::cron` | Port of TS `cron.ts` (AND matching, modulo steps, ignored single-value steps, never-matching names) | `import`, `lint` | End of 2.x (#17) |
| `oracle::frontmatter` | Port of TS `jobs.ts` line parser | `import`, `lint` | End of 2.x (#17) |
| `import` | The First-start import (#17), with the Job import as its last step: plan, rewrite with `jobs::LineEdit`, backup through `store`, report, audit | Worker first start; `claudeclaw import [--dry-run]`, `claudeclaw jobs import [--dry-run]` | End of 2.x (#17). 3.0 refuses a 1.x dir. |
| `leftovers` | TS-value migration: installs or switches the `statusLine` key to `claudeclaw statusline` (a foreign value is left and reported), and deletes a leftover `state.json` at every Worker start (#24) | Worker start, import | End of 2.x, like the import (#24) |
| `lint` | The 1.x divergence lint, as a `jobs::Lint` | job loading | When `main` switches |

- **First-start import** (#17). Triggered by a missing `daemon/state/import.json`. Copy-only, in this order: Launchers (a 1.x ccs scope becomes a generated script in `daemon/launchers/`, `ccs-<slug>.cmd` on Windows (#25); anything that failed in 1.x becomes a Refused run), the legacy `apiToken` as a hashed `send` token kept in step with `settings.json` for all of 2.x, job state seeded from TS logs (`logs/<agent>/` included) and merged on every start, renamed settings (`watchdog.*` → `timeout_breaker`, `--only settings`, #32), then the Job import. One backup, one report, one notification. The report also carries behaviour-change info lines (bounded Auto-compact #33, Awake-time timeouts #38, Inbox retention and the 24 h deferral of the first Housekeeping pass #44) and writes nothing for them. `claudeclaw import --dry-run --json` previews it all and prints `api::ImportPlan`, including a `claude_code` section (`statusline`, `state_json`, #24).
- **Dependency direction is `ts-compat → jobs, store`, never the reverse.** The binary registers it behind the cargo feature `ts-compat`, which is on by default.
- **Removal** is: delete the directory, drop the feature and its registration lines, and delete the CLI subcommands.
- The `oracle` modules double as #18's parity fixtures for cron and frontmatter readings.

**`crates/store/src/rollback.rs` (`store::rollback`).** The #14 rules that shape *writes* until `main` switches:

| Rule | Mechanism in `store::rollback` |
|---|---|
| 1–3 additive, preserve, import-never-move | Settings patch writer (only Rust keys, never removes); import copies |
| 4 snowflakes as strings | Patch writer emits strings for any snowflake it writes |
| 5 TS run-log names | `RunLog::create(name, start)` → `logs/<name>-<YYYY-MM-DDTHH-MM-SS-mmmZ>.log` (agent jobs in `logs/<agent>/`), TS body layout, Rust additions appended as extra lines/sections |
| 6 merge job state every start | `JobState::load_merged()` = newer of `daemon/state/jobs.json` and newest TS-pattern log per job (including `logs/<agent>/`, fixing trap 6); the seed reads `Exit code:` from the newest log (#17) |
| 7 mirror active session | `SessionMirror::write(scope, session)` into `session.json` / `sessions.json.threads[<ts key>]` / `agents/<agent>/session.json` in TS shape; TS entry adopted as current Launcher's session on read if it differs |
| 8 one-shots stop TS too | `enabled: false` edit from #12 (in `jobs`, verified TS-readable) |
| 9 rewritten cron same under both | Enforced by `ts-compat::import` |
| 10 guard the other daemon | `PidGuard`: refuse if `daemon.pid` names a live process that is not this Daemon; write Watchdog pid as `"<pid>\n"` |

**What stays after the switch and what goes.**
- **Stays.** Reading the TS *formats* (settings, session files, job files) is the permanent drop-in contract, not compat code. It stays in `store` and `jobs`.
- **Goes.** At the switch, the maintainer deletes `store::rollback` or keeps parts of it on purpose (for example, the run-log naming). Any break in TS readability goes in the release notes (#14 §6.4). The `settings.legacy_key` notice outlives the switch (#32).

## 9. On-disk layout

### 9.1 Per project dir (one Daemon per dir, #9)

Owner key:
- **S**: shared with TS, kept in TS shape;
- **R**: Rust-only, which TS never reads;
- **T**: TS-only, which Rust leaves untouched;
- **C**: Claude Code's own file; Rust patches only the keys named (#24).

```
<project-dir>/
├── CLAUDE.md                                S  read every run; created once if absent (TS behaviour)
├── agents/<agent>/                          S  agent cwd; jobs/*.md; session.json (mirror, rule 7)
├── agents/.archive/<slug>-<id>-<UTC stamp>/ S  fired threads and archived agents (#16, #21); explicit purge only
└── .claude/
    ├── settings.json                        C  Claude Code's project settings; Rust patches only `statusLine` (#24)
    ├── statusline.cjs                       T  never written or deleted by Rust (#24)
    └── claudeclaw/
        ├── settings.json                    S  0600 enforced; Rust adds its own top-level keys only (§7)
        ├── session.json, sessions.json      S  TS-shape mirror of each Scope's current-Launcher session
        ├── session_fallback.json, fallback-sessions/, session_<N>.backup   S  same shapes as TS; newest 20 backups kept (#44)
        ├── pending-resume.json              S  consumed as TS does
        ├── permission-mode.json             S  read
        ├── jobs/*.md                        S  line edits + atomic rename only; never re-serialised
        ├── prompts/HEARTBEAT.md             S  read
        ├── inbox/discord/                   S  attachments, TS names `<snowflake>-<epoch ms><ext>`
        ├── inbox/api/                       S  API and CLI attachments, `<turn_id>-<index><ext>` (#21, #44)
        ├── outbox/discord/                  S  image upload root; never deleted
        ├── logs/<name>-<iso>.log            S  run logs, TS pattern (rule 5); logs/<agent>/ for agent jobs
        ├── logs/daemon.log                  T  TS's stdout redirect; Rust never writes it
        ├── state.json                       T  not written; a leftover is deleted at Worker start (#24)
        ├── active-runs, web.token           T  not written by Rust
        ├── daemon.pid                       S  "<watchdog pid>\n" (rule 10)
        ├── daemon.lock                      R  OS file lock held by the Watchdog (#9); holder record for Offline writes (#22)
        ├── api.sock                         R  local Management API, 0600 (#13); see 9.3
        ├── import-backup/<ts>/, import-report-<ts>.md   R  First-start import (#12, #17); 0700 with 0600 files; kept for all of 2.x
        └── daemon/                          R  0700; everything only the Rust Daemon owns
            ├── state/
            │   ├── jobs.json                per-job state: last run, result, in-flight, retry, interrupted (#9); revision, orphaned_since (#46)
            │   ├── sessions.json            (Scope, Launcher) → session; orphan clocks (#12 §6); transcript, context, cost_state (#39);
            │   │                            timeout_breaker {session_id, consecutive_timeouts, failing_since, opened_at} (#32);
            │   │                            auto_compact {session_id, spent_at} (#33)
            │   ├── supervisor.json          Watchdog status, the Crash record (§4, #20)
            │   ├── runs.json                Run registry (#22, #29)
            │   ├── import.json              First-start import record (#17)
            │   ├── launcher-tests.json      last Test launcher result per Launcher (#21); dropped when it leaves the picker
            │   └── notices.json             content hashes already notified (one notice per distinct content); unseen 30 days → dropped
            ├── launchers/                   Launcher scripts generated by the First-start import (#17)
            ├── tmp/                         per-run System-prompt files, 0600; deleted at Settle, emptied at Worker start (#25)
            ├── runs/<YYYY-MM>.jsonl         Run index (#23); old months compacted, not deleted (#44)
            ├── secrets/                     0700; files 0600
            │   ├── devices.json             paired Device public keys (SPKI SHA-256 + label + created/last seen); revoked tombstones (#21)
            │   ├── api-tokens.json          API token id, name, scopes, SHA-256 hash (legacy apiToken imported here); a revoked legacy token's hash (#21)
            │   └── tls/daemon.key, daemon.crt   self-generated (rcgen, ECDSA P-256)
            ├── logs/
            │   ├── worker.<YYYY-MM-DD>.jsonl    Daemon log: content-free tracing JSONL, daily rotation
            │   └── watchdog.<YYYY-MM-DD>.jsonl  Daemon log, daily rotation (includes Worker stderr/panics)
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
- Job files are line-edited. API edits survive the in-flight frontmatter guard; edits made outside the Daemon to a running job's frontmatter are restored, with a `job_frontmatter_restored` notice (#46).
- JSONL logs are append-only.
- Modes: `settings.json` and every file under `daemon/secrets/` are 0600; `daemon/` is 0700. On Windows, files inherit the user-profile ACL. The named pipe gets an owner-only DACL.

**Logs** (#23). Three logs. **Daemon logs** are content-free tracing JSONL with fixed level meanings, stable `event` names and span correlation; a binary-owned log sink stamps `seq`, reports dropped lines as `log.dropped` (the Worker channel is lossy at 128k lines), and mirrors WARN+ to the Worker's stderr so crash tails show the lead-up (#20). **Run logs** keep the TS layout and may hold content but never secrets. The **audit log** is the third.

Daemon-log event names are stable. Each event is declared with its fields in `log_event::daemon`, and each field needs a `FIELD_CLASSES` entry (§4 `api`, #69). Naming rules (#74):
- Durations are `*_ms`, never seconds.
- One event name means one thing, written by one process.
- A name may appear in both the Daemon and the App log only with the same meaning and fields (`log.capped`, `update.available`). The App's local process duties are `local.*` and its pairing events `device_pairing.*`.
- The sink writes a tracing span's own name as `span.span_name`. `name` is a job name at any depth (`FIELD_CLASSES`). `attempt` is a span field only.
- A raw stderr line is `text`; `line` is always a line number.
- An owner notice is named for what the owner sees, and its event for the component that writes it (`launcher_root_moved` vs `session.root_moved`).

Later tickets added:
- #24: `compat.statusline_migrated`, `compat.state_json_removed`, `compat.statusline_unreadable` (WARN, #74);
- #25: `run.stdin_write_failed`;
- #26: `run_path.captured {source, shell, dirs, duration_ms}`, `run_path.changed {added, removed}`, `run_path.capture_failed {kind}`;
- #27: `worker.stalled {beacon, quiet_ms}` (ERROR), the Watchdog's `watchdog.stall_report {report_seq, waited_ms, workers, alive_tasks, global_queue}` (WARN, #74), `worker.ping_late`, `worker.hung {silence_ms}`, `worker.shutdown_overrun`, `watchdog.hang_kill_disabled` (WARN);
- #29: `reap.start`, `run.reaped` (`already_gone` and `not_ours` at INFO, #74), `reap.done` with the `ReapCounts` fields, `reap.orphan_worker {waited_ms, killed}`, `reap.leftover_after_stop {count}` (WARN, #74), `reap.registry_unreadable {cause}` (ERROR, #74);
- #32, #33: `session.breaker_open {limit, consecutive_timeouts, failing_ms}` (WARN), `session.breaker_closed` (INFO), `run.recovery_skipped` (INFO, with its reason and `spent_ms_ago`), `settings.legacy_key {key, replacement}` (WARN, besides the owner notice); `run.start` / `run.finish` gain `purpose`;
- #36: `logs.prune_incomplete {kept, expected}` (WARN), and a content-free `console` field on `daemon.start`;
- #37: `run.settle`;
- #38: `system.resumed`; `run.finish` gains `awake_ms`, `suspended_ms` and `wake_grace`;
- #39: `transcript.not_found` (INFO), `session.root_moved` (WARN), `usage.baseline_reset` (WARN);
- #40: `api.client_gone`, and the App log's `log_event::app::*` catalogue;
- #42: `run.finish` gains `claude_code_version`; `runner.cc_version_unparsed`, `runner.claude_code_outdated`;
- #43: `run_locale.chosen {source, lc_ctype, charmap, verified}` (INFO), `run_locale.not_utf8` (WARN), `run_locale.unavailable` (WARN);
- #44: `runindex.backfill_start`, `runindex.backfill_done`, `runindex.backfill_failed`, `runindex.unparsed`, `runindex.torn`, `inbox.pruned`, `inbox.over_cap`, `disk.low`, `disk.ok`, `housekeeping.done`, `log.capped{hard}` (`hard` always present, #74);
- #45: `watchdog.start_held`, `watchdog.give_up_cleared`, `watchdog.exit`, `watchdog.supervisor_unreadable` (WARN);
- #46: `job.file_changed {job, change, source, revision}`, `job.file_unstable {job, unstable_ms}`, `job.frontmatter_restored {job, keys}`, `job.state_carried {from, to}`, `scan.slow {pass_ms, interval_ms}`, `scan.dir_unavailable {dir_kind, errno}`;
- #47: `run.lingered`;
- #74: `worker.panic {location}` (ERROR, from the panic hook); `run.refused {reason}`, `discord.disconnected {close_code}`, `api.request` gains `route` (the route template), and `worker.stderr`'s raw line is `text`.

**Retention** (#23, #44). The Daemon deletes only files it wrote, under a rule listed here or in §4 `core::housekeeping`.
- **Run logs** are kept forever by default (TS parity; the catch-up seed reads them); `logging.runLogKeepDays` is opt-in, and Housekeeping then keeps the newest Run log per name.
- **Run index**: once a month ended more than `runLogKeepDays` ago, its file is rewritten to keep only records whose Run log still exists, and deleted if none are left. With no `runLogKeepDays` it is kept forever.
- **Daemon logs** keep 14 files per process, with a soft 256 MiB daily cap (WARN+ keeps writing), a hard cap at 2× (everything dropped and counted, one `log.capped{hard: true}` line), and a 1 GiB total budget enforced by the sink at startup and each rotation.
- **Audit log**: kept forever by default; `logging.auditKeepMonths` is opt-in.
- **Inbox**: 30 days after download, then a 1 GiB total cap (§4 `core::housekeeping`). Age comes from the name (Discord epoch ms; the API `turn_id`'s UUID v7 time), never mtime. Files not matching the Daemon's patterns are never deleted and are counted as `inbox_foreign`.
- **Never touched**: Transcripts, `outbox/`, `agents/` (including `.archive/`), `import-backup/`, rotation summaries, `whisper/` and TS-only files.

### 9.2 Outside the project dir

| Path | Owner | What |
|---|---|---|
| `dirs::data_local_dir()/claudeclaw/known-daemons.json` | CLI/Daemon | Project dirs that have run a Daemon (written at Watchdog start). It replaces TS's lossy decoding of `~/.claude/projects` names for `status --all` / `stop --all`. Entries are checked against the lock, never trusted; an entry whose dir no longer exists is dropped when read (#44). |
| `dirs::cache_dir()/claudeclaw/update-check.json` | Daemon | Last daily Releases check (#6) |
| Tauri `app_data_dir()/daemons.json` | Desktop app | Local dirs and remote Daemons (host, port, pinned fingerprint, label) |
| Tauri `app_log_dir()` | Desktop app | The App log (#40) |
| OS keychain (`keyring`) | Desktop app | One Device private key per remote Daemon (#13) |
| `<config dir>/projects/<project dir>/*.jsonl` | Claude Code | Transcripts, read-only. Each run's Config dir is located from `init.memory_paths`, with a lazy search plus `transcript_roots` as fallback (#39). |

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
| | `which` | 8.0.6 | `runner`: Launcher, `direct`, interpreter and `locale` resolution on the Run PATH (#26, #43) |
| | `nix` | 0.31 | Unix only: `signal, process, fs, user` (`killpg`, `setsid`, `fcntl` for the lock fd on re-exec, `getpwuid_r` (#26), `statvfs` (#44)) |
| | `libc` | | Unix only: the suspend-counting clocks (#38) |
| | `windows-sys` | 0.61 | Windows only: pipe security descriptor, job objects, `Win32_System_Registry` and `Win32_System_Environment` (#26), `QueryInterruptTime` (#38), `GetDiskFreeSpaceExW` (#44) |
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
| | `sha2` | 0.11.1 | SPKI fingerprints, API token hashes, socket hash, Change scan hash pass |
| | `subtle` | 2.6.1 | constant-time compares |
| | `getrandom` | 0.4.3 | Pairing codes, API tokens |
| | `secrecy` | 0.10.3 | `SecretString` for every secret (#23) |
| Updates | `minisign-verify` | 0.3.0 | verify `SHA256SUMS.minisig` with the embedded keys (#6, #35) |
| Observability | `tracing` | 0.1.44 | |
| | `tracing-subscriber` | 0.3.23 | `json, env-filter` |
| | `tracing-appender` | 0.2.5 | daily rolling files; the binary's own sink adds `seq`, drops and caps (#23) |
| | `sd-notify` | 0.5.0 | Linux only (#4) |
| CLI | `clap` | 4.6.7 | `derive` |
| Misc | `tempfile` | 3.27.0 | atomic writes |
| | `dirs` | 7.0.0 | user dirs (§9.2) |
| | `uuid` | 1.28.0 | `v7, serde` (run IDs, Turn IDs, audit IDs) |
| | `thiserror`, `anyhow` | 2.0.21, 1.0.104 | |
| Test | `proptest`, `similar-asserts` | | #18; axum's `ws` feature for gateway replay |
| Desktop | `tauri` / `tauri-build` | 2.12.3 / 2.7.2 | `tray-icon`; never the `tracing` feature (#40) |
| | `tauri-plugin-notification`, `-autostart`, `-single-instance`, `-updater`, `-dialog` | 2.6.0, 2.7.0, 2.5.3, 2.13.3, 2.8.2 | |
| | `keyring` | 4.2.0 | per-OS native stores (Keychain, Windows Credential Manager, Secret Service) |
| Desktop UI (npm) | `@tauri-apps/api` 2.12, React 19, Vite 8, Tailwind 4, shadcn/ui | | majors fixed here; exact pins and look go to [#16](https://github.com/Winter-wub/claudeclaw/issues/16) |
| | `openapi-typescript` / `openapi-fetch` | 7.13 / 0.17 | generated types + typed fetch over `api_call` |

**Not picked, on purpose.** The binary needs none of these:
- `serenity`/`poise` (twilight was chosen, #4);
- `tokio-cron-scheduler` (#4);
- `serde_yaml` and its forks (#4);
- `notify` (§7, #46);
- `fs4` (std `File::lock` replaces it);
- `interprocess` (tokio's native pipes plus a DACL suffice);
- `self_update` / `self-replace` (the Windows rename is about 30 lines, and `self-replace`'s last release was 2024-09);
- `async-trait` (native async traits);
- `openssl`/`native-tls` (one TLS stack, rustls on ring; enforced by `deny.toml`, #35);
- `tauri-plugin-log` (#40).

## 11. Decisions and alternatives

These are #11's own decisions. Later tickets' alternatives are recorded on the tickets themselves.

| # | Decision | Alternatives rejected |
|---|---|---|
| D1 | **13-crate workspace with enforced dependency rules** (§3), plus the unreleased `parity` member (#18) | *One daemon crate with modules.* Boundaries would be by convention only: an adapter could call the store, and the app could link state code. That breaks #10's "never writes daemon files" by accident, and `ts-compat` removal becomes a hunt. *Many micro-crates* (one per adapter feature, per API area): more `Cargo.toml` churn with no boundary benefit. |
| D2 | **Worker serves the Management API. The Watchdog stays minimal** and publishes `supervisor.json`, which the client reads when the socket is down | *Watchdog owns the listeners and proxies to the Worker.* The API would stay up during a crash loop, but the Watchdog would take on TLS, auth and HTTP, which contradicts the glossary ("only starts, restarts and re-executes") and widens the code that must never crash. *Watchdog passes listening fds to each Worker.* Not portable to Windows named pipes. |
| D3 | **Watchdog ↔ Worker over the Worker's stdin/stdout** (JSON lines; EOF = parent gone) | *A second private socket.* More code, and no free orphan detection. *Signals only.* Can't tell an intentional stop from a crash on Windows. |
| D4 | **Wall-clock schedule deadlines with sleeps capped at 30 s**, re-checked on a Resume (#38). Run timeouts use Awake time instead. | *`sleep_until` on tokio's `Instant` alone.* The monotonic clock pauses during system sleep, so a job due during sleep would fire late by the sleep's length. *TS's 60 s tick.* Wastes wake-ups and gives up to 60 s jitter (#14 measured TS up to 60 s late). |
| D5 | **Chat seam in `claudeclaw-chat`, adapters depend only on it** (§6) | *An adapter trait inside `core`.* Adapters would then see all of core. *A generic "plugin" interface now.* The OpenClaw-compatible plugin system is phase 2 and unspecified, and this seam can host it later. |
| D6 | **Typed Scope keys in Rust state, TS keys only in the mirror** | *Reuse TS's bare keys everywhere.* Job names and snowflakes share one namespace (trap 5). |
| D7 | **Settings: raw-`Value` patch writes, Rust keys at the top level of `settings.json`** | *A separate Rust config file.* Two places for users to look, and the Desktop app's settings would split. *Typed re-serialisation.* Drops unknown keys and order (rule 2). |
| D8 | **Rust-only files under `.claude/claudeclaw/daemon/`**, except paths earlier tickets fixed | *Rust files scattered beside TS files.* Name collisions, no single 0700 boundary. *Outside the project dir* (XDG state). Breaks "one Daemon per project dir" portability and the scrubbed-copy Shadow instance workflow (#14). |
| D9 | **Socket path: in-dir if short, hashed runtime path otherwise** | *Always in-dir.* Fails on long paths (verified). *Always hashed.* Contradicts #13's "socket in `.claude/claudeclaw/`" for the common case. *A pointer file.* It can go stale. |
| D10 | **Local transport with tokio-native UDS / named pipe**, and reqwest 0.13's built-in `unix_socket` / `windows_named_pipe` on the client | *`interprocess` crate.* An extra abstraction where tokio already has both. *hyper-util custom connectors.* Reqwest 0.13.5 already does this. |
| D11 | **Remote TLS on rustls/ring with custom verifiers; Device = TLS client cert pinned by SPKI hash** | *Bearer Device keys over server-only TLS.* Weaker: a leaked bearer token works anywhere. *aws-lc-rs provider.* Needs cmake/NASM on Windows CI for no gain here; the probe confirms nothing pulls it in. *native-tls.* Three TLS stacks across OSes, and no pinning hooks. |
| D12 | **OpenAPI via utoipa 6 → committed `openapi.json` → `openapi-typescript` + `openapi-fetch` in the app** | *ts-rs / specta / tauri-specta.* They generate TS types but not OpenAPI, which #13 requires for remote automation. *aide.* Smaller user base, last release 2025-08. *A hand-written OpenAPI.* Drifts. *openapi-generator / orval.* Heavy runtime code generation. |
| D13 | **Desktop UI calls the API through one Tauri `api_call` / `api_subscribe` bridge** | *Webview `fetch` to the Daemon.* Webviews can't open Unix sockets or named pipes, and the Device key would have to live in JS. *One Tauri command per endpoint.* Duplicates the API surface by hand. |
| D14 | **App starts the Daemon detached with its own code**, with a scrubbed spawn env (#26) | *`tauri-plugin-shell` sidecar.* Ties the child to the app's lifetime, while #10 wants the Daemon to keep running after the app quits. |
| D15 | **`ts-compat` as its own crate + `store::rollback` module** | *A `ts_compat` module inside `jobs`.* Deleting it touches `jobs`, and nothing stops other code from calling the oracle. *Everything in one crate.* Mixes the oracle (deleted at the end of 2.x) with drop-in readers (permanent). |
| D16 | **Polling settings and job reload**, now the Change scan (§7, #46) | *`notify` file watcher.* See §7. |
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
| A starting Watchdog racing the app's shared-lock probe (#20) | Without a retry it exits 73 by mistake; with the 1 s retry it gets the lock after ~222 ms |
| `serde_json` (`preserve_order`) raw round trip | unknown keys, key order and a 19-digit snowflake number survive; new key appended last |
| croner 4.0.1 with `FixedOffset` (+07:00) | `0 9 3-31/3 * *` at 2026-10-13 12:00 → prev 2026-10-12 09:00, next 2026-10-15 09:00 |
| utoipa 6 + utoipa-axum 0.3 on axum 0.8 | `OpenApiRouter` emits `"openapi": "3.1.0"` |
| Tokio Windows pipe APIs exist | `ServerOptions::first_pipe_instance`, `reject_remote_clients`, `create_with_security_attributes_raw` in tokio 1.53.2 source |
| reqwest named-pipe client | `ClientBuilder::windows_named_pipe` in reqwest 0.13.5 source (`cfg(target_os = "windows")`) |
| Claude Code's Bash tool on macOS (#47, 2 real calls) | Every Bash-tool command, foreground or `run_in_background`, runs in its own process group, and `&` children are reparented to launchd. `claude --print` stays alive after its `result` while a background task runs. |
| process-wrap kill-on-close (#36) | Needs `KillOnDrop` in the wrapper stack |
| Linux `current_exe()` (#36) | Resolves brew symlinks, hence `update` uses `argv[0]` on `PATH` |
| Hosted arm64 macOS runners (#36) | Rosetta is present, so `verify-published` runs the x86_64 archive there |
| Run-index Back-fill (#44) | On a live-shaped copy of 5,755 TS logs: 0.9–2.7 s once, 12 ms for the re-check on every start (macOS, warm cache) |

**Not verified by a probe** is tracked row by row in the Verification register (§14): Windows and Linux builds, the Tauri crates, lock-fd inheritance across `exec`, the named-pipe DACL at runtime, twilight against a live gateway, Linux sleep, the example systemd unit, and the Change scan on Windows and network filesystems.

## 13. Review points

1. **13 crates** (D1). That is the most structure of any choice here. Merging `scheduler` into `core` and `jobs` into `store` would leave 11 crates, but `ts-compat` and parity tests then reach deeper.
2. **The Worker serves the API, not the Watchdog** (D2). During a crash-loop backoff, remote apps see "unreachable", and the Give-up is reported by Discord DM plus `supervisor.json` locally. A Watchdog-owned API would avoid that gap, at the cost of a bigger Watchdog.
3. **Rust-only state under `.claude/claudeclaw/daemon/`** (D8) and the new top-level keys in `settings.json` (D7, §7).
4. ~~**Job import is deleted with `ts-compat` when `main` switches.**~~ Resolved by #17: `oracle::*` and `import` stay for all of 2.x, and 3.0 refuses a 1.x dir. Only `lint` goes when `main` switches.
5. **Device = TLS client certificate pinned by SPKI** (D11). It locks the Device key format to X.509 + ECDSA P-256.
6. **ring instead of aws-lc-rs** (D11). Revisit if FIPS or a post-quantum key exchange becomes a requirement.
7. **The 30 s sleep cap** (D4) instead of OS wake notifications. #38 added clock-gap Resume detection on top, still without OS power events.
8. ~~**No statusline in Rust v2.0.**~~ Superseded by #24: the statusline is kept as `claudeclaw statusline` (§4).
9. **`known-daemons.json` outside the project dir** (§9.2). It is a small new global file, needed only for `status --all` / `stop --all`.
10. **twilight's `rustls-platform-verifier`** (OS trust store) instead of bundled webpki roots.

## 14. Build, release and verification

**Distribution** (#6, #35). GitHub Releases + a Homebrew tap (macOS/Linux) + a Windows zip, all three OSes at launch. "Unsigned" means no Developer ID or Authenticode signature; macOS artefacts are still ad-hoc signed. The CLI ships on 5 targets, with a static musl binary on Linux; the Desktop app ships as a universal `.dmg`, an NSIS per-user installer, and `.deb` + AppImage. Rust versions start at 2.0.0; tags are alpha / rc / stable, and the Update channel follows the running version. Formula and cask exist only from v2.0.0; RCs install from archives.

**CI** (#35). Four SHA-pinned GitHub Actions workflows: `ci`, `parity`, `release` and `audit`. Only the `ci-ok` and `parity-ok` aggregates are required, through a `main` ruleset. `parity-rc` is called by `release.yml` on a Release candidate (gate G1), not tag-triggered. `cc-compat` is a nightly `parity.yml` job that installs real Claude Code from npm at the minimum and at latest, and drives the runner's real argv against a Fake API (#36, #42).

**Release key** (#35). One minisign key, stored as the `RELEASE_MINISIGN_KEY` secret in an approval-gated `release` environment (the maintainer's choice). It signs `SHA256SUMS` and the Tauri updater artefacts. A backup key is kept offline, and both public keys are embedded in `crates/claudeclaw/keys/`. Release tooling: `xtask release prepare|tag`, `scripts/release/{sign,brew}.sh` and `CHANGELOG.md`. Releases are immutable and upgrade-tested. Each release attaches `desktop-ui-sourcemaps-<version>.tar.gz` (#40).

**Packaging traps** (#35):
- `.deb` versions use `~rc.N`, because dpkg sorts `-rc.1` above the final release.
- The `.deb` must `Depends: ca-certificates`, because the static musl binary has no OS CA store.
- NSIS compares the full semver when upgrading.
- Every release artefact (`build-cli` and `build-desktop`) is built with the same `--remap-path-prefix` flags: the workspace becomes `claudeclaw/` and the cargo registry `cargo/`. Panic locations, Crash-record `stderr_tail`s and Daemon log lines therefore never carry the build machine's paths, and the bundled-binary SHA-256 check still holds (#69).

**Verification register** (#36). A committed `verification/checks.toml`, enforced by `cargo xtask verify` and `release.yml`, which refuses RC and stable tags while a blocking check is unwritten. Every behaviour that needs proof beyond unit tests is one row, with where it runs, what proves it, and its gate (merge / rc / Day 0 / none). A row is one of:
- a CI check (Windows on `windows-2025`; Linux sleep in a QEMU q35 S3 guest);
- Shadow evidence (macOS sleep, app use, live Discord, dark-wake counts);
- a Known gap, printed in every release's notes.

Windows ships CI-verified only. Real sleep and the app GUI pass on Windows, and Linux desktop sessions, are Known gaps; there is no manual gate (the maintainer's choice).

## Decision index

One line per closed ticket on the wayfinder map [#2](https://github.com/Winter-wub/claudeclaw/issues/2). The map's "Decisions so far" holds the gist of each.

- [#3](https://github.com/Winter-wub/claudeclaw/issues/3) Inventory the TS daemon's on-disk and Claude Code contract — §8, §9.1 (TS traps, shared files)
- [#4](https://github.com/Winter-wub/claudeclaw/issues/4) Survey Rust crates for the daemon — §10
- [#5](https://github.com/Winter-wub/claudeclaw/issues/5) Survey how Claude Code launchers behave — §4 `runner` (`stream`, `judge`)
- [#6](https://github.com/Winter-wub/claudeclaw/issues/6) Choose the distribution form — §14
- [#7](https://github.com/Winter-wub/claudeclaw/issues/7) Decide where the Rust code lives in the fork — §3
- [#8](https://github.com/Winter-wub/claudeclaw/issues/8) Design the launcher model — §4 `runner`, `core::scope`; §5 run pipeline
- [#9](https://github.com/Winter-wub/claudeclaw/issues/9) Design the daemon process model and supervision — §4 Watchdog, `scheduler`; §5
- [#10](https://github.com/Winter-wub/claudeclaw/issues/10) Design the desktop management app — §4 `claudeclaw-desktop`
- [#11](https://github.com/Winter-wub/claudeclaw/issues/11) Lock the architecture and crate picks — this doc
- [#12](https://github.com/Winter-wub/claudeclaw/issues/12) Decide which TS job and cron quirks the Rust daemon keeps — §4 `jobs`, §8
- [#13](https://github.com/Winter-wub/claudeclaw/issues/13) Design the management API and remote pairing security — §4 `server`, `client`; [`management-api.md`](management-api.md)
- [#14](https://github.com/Winter-wub/claudeclaw/issues/14) Plan Zero's cutover from the TS to the Rust daemon — §8 `store::rollback`
- [#16](https://github.com/Winter-wub/claudeclaw/issues/16) Prototype the desktop app's screens and navigation — §4 `claudeclaw-desktop`; [`management-api.md`](management-api.md) §3
- [#17](https://github.com/Winter-wub/claudeclaw/issues/17) Specify the Rust daemon's first start on a TS data dir — §8 First-start import; §13 point 4
- [#18](https://github.com/Winter-wub/claudeclaw/issues/18) Choose the parity test strategy — §3, §4 `claudeclaw-parity`
- [#20](https://github.com/Winter-wub/claudeclaw/issues/20) Decide what the app can show when a daemon is not answering — §4 Watchdog status file, `client::offline_status`, desktop offline commands
- [#21](https://github.com/Winter-wub/claudeclaw/issues/21) List the Management API resources the app's screens need — [`management-api.md`](management-api.md); §4 control pipe, §6 `AdapterControl`
- [#22](https://github.com/Winter-wub/claudeclaw/issues/22) Specify the claudeclaw CLI surface — §4 `claudeclaw` (CLI, exit codes, Offline writes)
- [#23](https://github.com/Winter-wub/claudeclaw/issues/23) Specify logging and observability — §5 Run index, §9.1 Logs and Retention
- [#24](https://github.com/Winter-wub/claudeclaw/issues/24) Decide whether the Rust daemon keeps the Claude Code statusline — §4 Statusline, §8 `leftovers`, §9.1
- [#25](https://github.com/Winter-wub/claudeclaw/issues/25) Specify Launcher scripts on Windows — §4 `runner` (`argv`, Windows Launchers)
- [#26](https://github.com/Winter-wub/claudeclaw/issues/26) Decide the daemon's PATH when the desktop app starts it — §4 `runner::env` (Run PATH)
- [#27](https://github.com/Winter-wub/claudeclaw/issues/27) Decide whether the Watchdog detects a hung Worker — §4 Watchdog hang detection, `core::health`
- [#28](https://github.com/Winter-wub/claudeclaw/issues/28) Decide on a scrubbed log export for bug reports — §4 `store::export`
- [#29](https://github.com/Winter-wub/claudeclaw/issues/29) Decide how the Watchdog reaps a crashed Worker's Claude runs — §4 Watchdog reaping, `runner::reap`
- [#32](https://github.com/Winter-wub/claudeclaw/issues/32) Decide whether the Rust daemon keeps TS's runaway-session breaker — §4 `core::sessions` (Timeout breaker), §7
- [#33](https://github.com/Winter-wub/claudeclaw/issues/33) Decide whether auto-compact on timeout stays unbounded by default — §4 `core::sessions` (Auto-compact), §5 Run index, §7
- [#35](https://github.com/Winter-wub/claudeclaw/issues/35) Design the CI and release pipeline — §14
- [#36](https://github.com/Winter-wub/claudeclaw/issues/36) Plan verification of what no probe has covered yet — §14 Verification register, §12
- [#37](https://github.com/Winter-wub/claudeclaw/issues/37) Decide the straggler policy on a run's normal exit — §4 `runner::spawn` (Settle)
- [#38](https://github.com/Winter-wub/claudeclaw/issues/38) Decide how run timeouts treat system sleep — §4 `runner::clock`, `scheduler`
- [#39](https://github.com/Winter-wub/claudeclaw/issues/39) Decide how the daemon handles Launchers that change Claude Code's config dir or provider — §4 `runner::stream`, `core::sessions`; §5 Cost; §9.2
- [#40](https://github.com/Winter-wub/claudeclaw/issues/40) Specify the desktop app's own diagnostics and long API calls — §4 `claudeclaw-desktop` (App log), `server` (Deadlines, Detach)
- [#41](https://github.com/Winter-wub/claudeclaw/issues/41) Decide how to pair another Device with a headless daemon — §4 `server` (Pairing codes, Pairing string), desktop `pair`
- [#42](https://github.com/Winter-wub/claudeclaw/issues/42) Decide the minimum Claude Code version and how it is checked — §4 `runner` (Minimum Claude Code)
- [#43](https://github.com/Winter-wub/claudeclaw/issues/43) Decide the locale for GUI-started daemons — §4 `runner::env` (Run locale), §7
- [#44](https://github.com/Winter-wub/claudeclaw/issues/44) Bound the daemon's on-disk growth — §4 `core::housekeeping`, `disk`; §9.1 Retention
- [#45](https://github.com/Winter-wub/claudeclaw/issues/45) Decide what a Give-up means on headless macOS — §4 Watchdog start sequence, Service managers, exit codes
- [#46](https://github.com/Winter-wub/claudeclaw/issues/46) Decide how the Worker detects job-file changes — §4 `store::scan` (Change scan), §7
- [#47](https://github.com/Winter-wub/claudeclaw/issues/47) Verify whether Claude Code's Bash tool runs commands in their own process group — §4 `runner::spawn`, §12
- [#48](https://github.com/Winter-wub/claudeclaw/issues/48) Decide whether direct runs follow a CLAUDE_CONFIG_DIR set outside the daemon — §4 `runner::launcher`; [`management-api.md`](management-api.md) §2.1
- [#50](https://github.com/Winter-wub/claudeclaw/issues/50) Decide whether a new or rescheduled job catches up a slot that passed before it existed — §4 `scheduler`
- [#69](https://github.com/Winter-wub/claudeclaw/issues/69) Decide how FIELD_CLASSES covers the App log's fields _(maintainer decision)_ — §4 `claudeclaw-api` (Field class tables, catalogues, CI checks), §9 Logs, §14 Packaging traps
- [#70](https://github.com/Winter-wub/claudeclaw/issues/70) Decide the one spelling of the clear-give-up value _(maintainer decision)_ — §4 Watchdog status file (`stop.by`, `give_ups[]`); [`management-api.md`](management-api.md) §1.1
- [#74](https://github.com/Winter-wub/claudeclaw/issues/74) Settle the Daemon log catalogue's unnamed fields and clashes _(maintainer decision)_ — §9.1 Logs
- [#79](https://github.com/Winter-wub/claudeclaw/issues/79) Decide how core clears a chat's stuck reactions after a crash _(maintainer decision)_ — §4 `core::conversations`, §6

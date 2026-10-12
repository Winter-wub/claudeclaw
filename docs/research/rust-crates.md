# Rust crate survey for the ClaudeClaw daemon

Resolves issue #4 ("Survey Rust crates for the daemon"). Snapshot taken **2026-10-13**.
Release dates and download counts come from the crates.io API (`/api/v1/crates/<name>`). Repo activity comes from the GitHub API. "Recent DL" is crates.io's 90-day download count.

## Recommendation

| Need | Pick | One-line reason |
|---|---|---|
| Async runtime | `tokio` 1.53 | Every other pick assumes it. |
| Discord gateway + REST | `twilight-gateway` + `twilight-http` + `twilight-model` 0.17 (plus `reqwest` 0.13 for attachment downloads) | Low-level and modular. It maps one-to-one onto today's hand-rolled gateway/REST code (`src/commands/discord.ts` talks to `gateway.discord.gg` v10 and `discord.com/api/v10` directly). It also avoids serenity's pending 0.12 to `next` rewrite. Fallback: `serenity` 0.12.5. |
| Cron | `croner` 4.0 with `chrono::DateTime<FixedOffset>` | Built-in 5-field POSIX patterns, `find_previous_occurrence` (for catch-up) and `find_next_occurrence`, generic over any chrono `TimeZone`. It's the most recently released cron crate. |
| Child processes | `tokio::process` + `process-wrap` 10 (`tokio1`, `ProcessGroup` or `ProcessSession`, `KillOnDrop`) + `tokio_util::codec::LinesCodec::new_with_max_length` + `serde_json` | Tokio streams the NDJSON. process-wrap (successor to the deprecated `command-group`) kills the whole `claude` process tree. |
| Web UI | `axum` 0.8 (`axum::response::sse`) + `tower-http` 0.7; assets via `include_str!` or `rust-embed` 8 | axum ships SSE (`Sse`, `Event`, `KeepAlive`). The UI today is three generated strings, so plain `include_str!` is enough. Use `rust-embed` if it becomes a directory. |
| YAML frontmatter | `serde-saphyr` 1.3 (+ about 10 lines of hand-rolled `---` splitting) | Pure Rust, `unsafe` forbidden, panic-free, resource budgets, actively released. `serde_yaml` is deprecated and `serde_yml` is flagged unsound/unmaintained (RUSTSEC-2025-0068). |
| JSON settings | `serde_json` 1.0 (+ `tempfile` for atomic writes) | The standard choice. `u64` fixes the snowflake-precision workaround the TS code needs. |
| Signals and shutdown | `tokio::signal::unix` + `tokio_util::sync::CancellationToken`/`TaskTracker`; `sd-notify` 0.5 on Linux only | No extra framework needed. sd-notify adds `Type=notify` readiness and the watchdog under systemd. launchd has no readiness protocol. |

## 1. Discord gateway and REST

What the daemon needs: one bot gateway connection, sending and editing messages, reactions, threads, slash commands and interactions, and attachment downloads.

| Crate | Latest release | Activity | Recent DL | Notes |
|---|---|---|---|---|
| `serenity` | 0.12.5, 2025-12-20 (0.12.4 was 2024-11-15) | Default branch `current` last commit 2026-05. Branch `next` is **339 commits ahead** and was active 2026-10-10, but has no release or version bump yet. 5.6k stars. | 1.46M | Batteries included: gateway, HTTP, cache, builders, `Attachment::download()`. Biggest community. The catch: 0.12 is effectively a maintenance line, and an unreleased breaking rewrite (`next`, MSRV 1.95) is coming, so expect a migration later. |
| `poise` | 0.7.1, 2026-10-12 (0.7.0 2026-09-06) | Active | 172k | Command framework on top of `serenity ^0.12.5` (slash plus prefix commands, autocomplete, modals). 0.7.0 had large API breaks. Only worth it if there will be many slash commands. ClaudeClaw has a handful. |
| `twilight-gateway` / `-http` / `-model` | 0.17.1, 2025-12-13 (0.17.0 2025-11-08, 0.16.0 2025-01-12) | `main` active (commits 2026-10-06 to 10-09). MSRV 1.89. 883 stars. | 154k / 626k / 722k | Modular, no framework, no implicit cache. Shards handle resume and reconnect. You do your own event loop and your own attachment downloads (via `reqwest`). More boilerplate, but every effect is explicit. |
| `twilight-interactions` | 0.17.1, 2026-05-27 | Third-party, active | 6k | Derive macros for slash-command definitions and parsing on twilight. Optional. ClaudeClaw's few commands can be built by hand. |

**Pick: twilight 0.17.** The current daemon has no Discord library. It speaks gateway v10 and REST v10 directly, with its own resume logic. Twilight is the closest structural match, so the spec can describe events and HTTP calls one-to-one. It also keeps the daemon in control of its single event loop and avoids serenity's framework and cache, which a single-guild bot doesn't need. Both projects ship about 1 to 2 releases a year, so release cadence doesn't separate them. The deciding risk is serenity's unreleased `next` rewrite.

Trade-offs to accept:
- Twilight's community is smaller, with fewer examples.
- Some features must be hand-built: download attachments with `reqwest`, and register slash commands with `twilight_http::Client::interaction(app_id).set_guild_commands(...)`.

If the team would rather write less glue, `serenity` 0.12.5 is a valid fallback, without poise.

Sources: https://crates.io/crates/serenity · https://github.com/serenity-rs/serenity/tree/next · https://crates.io/crates/poise · https://github.com/serenity-rs/poise/blob/current/CHANGELOG.md · https://crates.io/crates/twilight-gateway · https://github.com/twilight-rs/twilight/releases · https://crates.io/crates/twilight-interactions

## 2. Cron parsing with a fixed UTC offset and "previous match"

| Crate | Latest release | Fit |
|---|---|---|
| `croner` | 4.0.1, 2026-10-02 (4.0.0 2026-08-31) | **Best fit.** 5-field patterns. Seconds are `Optional` by default, so set `Seconds::Disallowed` to reject 6-field input. `find_next_occurrence` / `find_previous_occurrence` / `iter_before` / `iter_after` are generic over `CronDateTime`, which is implemented for chrono `DateTime<Tz: TimeZone>` (so `FixedOffset` works) and for `jiff::Zoned`. Supports `L`, `W`, `#`, and nicknames. 4.0 tightened step syntax and fixed backward search across DST gaps. 10M total downloads, 4.2M recent. |
| `cron` | 0.17.0, 2026-06-18 | Mature (26M downloads). **Requires a seconds field** (6 or 7 fields), so 5-field input has to be prefixed with `"0 "`. "Previous" is available via `DoubleEndedIterator::next_back` on `Schedule::after(..)`. Works with any chrono `TimeZone`. |
| `tokio-cron-scheduler` | 0.15.1, 2025-10-28 | A full scheduler built on `croner ^3`, with UUID jobs and optional Postgres or NATS persistence. Overkill: ClaudeClaw needs its own catch-up, in-flight guard and retry semantics, so a parser plus a `tokio::time::sleep_until` loop is simpler. |
| `saffron` | 0.1.0, 2021-02-01 | Unmaintained. Avoid. |

**Pick: `croner` 4 + `chrono` 0.4 (`FixedOffset::east_opt(7*3600)`).** `jiff` 0.2.38 (2026-10-06) is an equally supported backend if the codebase prefers jiff. With fixed offsets there is no DST, so either backend is fine.

Behaviour-compatibility notes for the spec (checked against `src/cron.ts`):
- The TS matcher **ANDs** day-of-month and day-of-week. croner's default is POSIX/vixie **OR**. Set `CronParser::builder().dom_and_dow(true)` to keep today's behaviour, or make the switch to POSIX deliberately.
- TS supports only `*`, lists, ranges and steps, with numeric values only. croner accepts a superset (names, `7` for Sunday, `L`/`W`/`#`, nicknames) and rejects non-standard `0/10` step forms unless `sloppy_ranges(true)` is set.
- `prevCronMatch` in TS scans back at most 360 minutes. Keep that bound in the scheduler, not in the parser.

Sources: https://crates.io/crates/croner · https://github.com/Hexagon/croner-rust (README, CHANGELOG, `src/lib.rs` `find_previous_occurrence`) · https://crates.io/crates/cron · https://docs.rs/cron · https://crates.io/crates/tokio-cron-scheduler · https://crates.io/crates/saffron

## 3. Async child processes with line-streamed stdout

| Crate | Latest release | Fit |
|---|---|---|
| `tokio::process` (tokio 1.53.2, 2026-10-03) | Core | `Command` has `kill_on_drop`, `process_group` (Unix) and `pre_exec`. Pipe stdout and wrap it in `FramedRead::new(stdout, LinesCodec::new_with_max_length(N))` (`tokio-util` 0.7.20, 2026-10-09). The length cap matters because `stream-json` lines carrying tool results or images can be very large. Use `BufReader::lines()` only if you accept unbounded lines. Apply timeouts with `tokio::time::timeout`, and drop or kill on cancellation. |
| `process-wrap` | 10.0.1, 2026-09-23 (8.x/9.x backports the same day) | Successor to `command-group`, with the same maintainers (watchexec). Composable wrappers: `ProcessGroup::leader()`, `ProcessSession`, `KillOnDrop`, `JobObject` (Windows). `kill()` signals the whole group, which covers MCP servers and shell tools that `claude` spawns. MSRV 1.87. Brings in `nix` 0.31. |
| `command-group` | 5.0.1, 2023-11-18 | README: "No further work will be done". Deprecated in favour of process-wrap. |
| `async-process` (smol) | 2.5.0, 2025-09-14 | Not tokio-native. Skip. |

**Pick: tokio + process-wrap (`features = ["tokio1"]`) + `LinesCodec` with a max length + `serde_json::from_str` per line.** Model the stream as a `#[serde(tag = "type")]` enum with a catch-all variant so unknown event types don't break parsing. Doing it by hand with `Command::process_group(0)` plus `nix::sys::signal::killpg` is a viable alternative that drops one dependency.

Supervisor interaction to keep in the spec: putting `claude` in its **own** process group takes it out of launchd's default cleanup ("When a job dies, launchd kills any remaining processes with the same process group ID as the job", unless `AbandonProcessGroup` is set). So the daemon must kill its child groups itself on shutdown: send SIGTERM, wait, then SIGKILL, mirroring the 5 s escalation in `src/runner.ts`. Under systemd, `KillMode=control-group` (the default) catches strays anyway.

Sources: https://docs.rs/tokio/latest/tokio/process/struct.Command.html · https://docs.rs/tokio-util/latest/tokio_util/codec/struct.LinesCodec.html · https://crates.io/crates/process-wrap · https://github.com/watchexec/process-wrap · https://github.com/watchexec/command-group · https://crates.io/crates/async-process

## 4. HTTP server with SSE and embedded static assets

| Crate | Latest release | Fit |
|---|---|---|
| `axum` | 0.8.9, 2026-04-14; `main` active (2026-10-11) | Built-in `axum::response::sse::{Sse, Event, KeepAlive}`. Feed it from a `tokio::sync::broadcast` receiver wrapped in `tokio_stream::wrappers::BroadcastStream` (`tokio-stream` 0.1.19, 2026-07-22). 131M recent downloads. |
| `tower-http` | 0.7.1, 2026-08-31 | Compression, tracing, CORS and timeouts as layers. |
| `actix-web` | 4.16.0, 2026-10-12 | Healthy, but has its own runtime model. No advantage here. |
| `warp` 0.4.3 (2026-05-04), `poem` 3.1.12 (2025-07-28) | — | Smaller or slowing ecosystems. Skip. |
| `rust-embed` | 8.13.0, 2026-10-07 | Embeds a directory in release builds and reads from disk in debug builds (good for UI iteration). Optional `compression`, `mime-guess`, and SHA-256 hash for ETags. Note: its repository moved off GitHub to https://pyrossh.dev/repos/rust-embed. |
| `memory-serve` | 2.4.0, 2026-09-17 | axum-specific. Brotli/gzip pre-compressed at build time, ETag and cache headers. Nice, but a smaller user base (26k recent). |
| `include_dir` | 0.7.4, 2024-06-17 | Stale. Skip. |

**Pick: axum 0.8 + tower-http.** For assets, the current UI is three generated strings (`src/ui/page/{template,styles,script}.ts`, about 120 KB), so `include_str!` / `include_bytes!` is enough with no extra crate. If the UI becomes a real directory, use `rust-embed` (with `mime-guess`). Choose `memory-serve` only if pre-compression matters.

Sources: https://docs.rs/axum/latest/axum/response/sse/index.html · https://crates.io/crates/axum · https://crates.io/crates/tower-http · https://crates.io/crates/rust-embed · https://crates.io/crates/memory-serve · https://crates.io/crates/actix-web

## 5. YAML frontmatter and JSON settings

| Crate | Latest release | Status |
|---|---|---|
| `serde_yaml` | 0.9.34+deprecated, 2024-03-25 | Deprecated by its author. Avoid for new code. |
| `serde_yml` | 0.0.13, 2026-05-27 | **RUSTSEC-2025-0068: unsound and unmaintained**, repo archived. Avoid. |
| `serde_yaml_ng` | 0.10.0, 2024-05-26 | Fork of serde_yaml. Still uses the unmaintained `unsafe-libyaml`. Last push 2025-09. |
| `serde_norway` | 0.9.42, 2024-12-21 | Fork of serde_yaml on `unsafe-libyaml-norway`. Last push 2025-08. |
| `serde-saphyr` | 1.3.0, 2026-09-16 (1.0 2026-07-31) | Pure Rust, `unsafe` forbidden, fuzzed and panic-free, configurable budgets against hostile input, duplicate-key policy. Very active. 7.4M recent downloads. |
| `yaml-rust2` / `saphyr` | 0.13.0 (2026-09-11) / 0.2.1 (2026-10-12) | DOM-level parsers with no serde. Usable, but more glue. |
| `gray_matter` | 0.3.2, 2025-07-10 | Frontmatter extractor, but pinned to `yaml-rust2 ^0.10` and returns its own `Pod` type. Splitting on `---` takes about 10 lines, so it isn't worth the dependency. |
| `serde_json` | 1.0.154, 2026-10-11 | The standard. |

**Pick: `serde-saphyr` for frontmatter (split on `---` by hand), `serde_json` for settings.**

Notes:
- **Compatibility flag:** today `src/jobs.ts` doesn't parse YAML. It does line-based `key: value` matching and strips quotes. A real YAML parser will **reject an unquoted schedule starting with `*`**: `schedule: */5 * * * *` reads as an alias. The bundled docs and examples always quote the schedule, but the spec should decide whether to keep a lenient fallback or document quoting as required.
- `src/config.ts` works around JSON.parse losing precision on Discord snowflakes above 2^53. In Rust, deserialize IDs as `u64`, or as strings via `serde_with`, and the issue goes away.
- To round-trip unknown settings keys, use `#[serde(flatten)] extra: serde_json::Map`, plus the `preserve_order` feature if key order matters. Write atomically with `tempfile::NamedTempFile::persist` (`tempfile` 3.27.0, 2026-03-11).
- If settings need comments later, `json5` 1.3.1 (2026-02-07) and `jsonc-parser` 0.34.0 (2026-09-27) are both maintained.

Sources: https://crates.io/crates/serde_yaml · https://rustsec.org/advisories/RUSTSEC-2025-0068.html · https://crates.io/crates/serde_yaml_ng · https://crates.io/crates/serde_norway · https://crates.io/crates/serde-saphyr · https://github.com/bourumir-wyngs/serde-saphyr · https://crates.io/crates/gray_matter · https://crates.io/crates/serde_json

## 6. Signal handling under launchd and systemd

| Crate | Latest release | Fit |
|---|---|---|
| `tokio::signal::unix` | tokio core | `signal(SignalKind::terminate())`, `interrupt()`, `hangup()` as async streams. Enough on its own. |
| `tokio-util` `CancellationToken` + `TaskTracker` | 0.7.20, 2026-10-09 | Broadcast shutdown to the gateway, scheduler, web server (`axum::serve(..).with_graceful_shutdown`) and runners, then `tracker.wait()` with a deadline. |
| `signal-hook` / `signal-hook-tokio` | 0.4.5 (2026-10-04) / 0.4.0 (2026-01-01) | Only needed for sync code or unusual signals. Not needed with tokio. |
| `ctrlc` | 3.5.2, 2026-02-10 | SIGINT/SIGTERM callback for sync programs. Redundant here. |
| `tokio-graceful-shutdown` | 0.20.0, 2026-07-30 | Subsystem-tree framework. Nice, but an opinionated layer the daemon doesn't need. |
| `sd-notify` | 0.5.0, 2026-03-09 | Pure Rust `READY=1`, `STOPPING=1`, `WATCHDOG=1`. No-op when `NOTIFY_SOCKET` is unset, so it's safe on macOS. |
| `libsystemd` | 0.7.2, 2025-04-30 | Heavier alternative to sd-notify. Skip. |

**Pick: tokio signals + CancellationToken/TaskTracker; `sd-notify` behind a `cfg(target_os = "linux")` path.**

Supervisor semantics the spec should encode:
- **launchd** sends SIGTERM, then SIGKILL after `ExitTimeOut` ("system-defined" default per `launchd.plist(5)`), so set it explicitly. With `KeepAlive = { SuccessfulExit = false; }`, a **zero exit is not restarted**. The TS daemon's SIGTERM handler always calls `process.exit(0)` (`src/commands/start.ts`), so any stray SIGTERM leaves it down under that setting. Either use `KeepAlive = true`, or reserve exit 0 for an intentional `stop` command and exit non-zero (for example `EX_TEMPFAIL` 75) for externally triggered shutdowns and fatal errors.
- **systemd** treats exit 0 and death by SIGHUP/SIGINT/SIGTERM/SIGPIPE as "clean". `Restart=on-failure` restarts on any non-zero exit or other signal. Default `KillMode=control-group` also reaps child process groups. Use `Type=notify` with `sd-notify` for readiness, and optionally `WatchdogSec=`.

Sources: https://docs.rs/tokio/latest/tokio/signal/unix/index.html · https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html · https://crates.io/crates/sd-notify · https://crates.io/crates/signal-hook · https://crates.io/crates/tokio-graceful-shutdown · https://keith.github.io/xcode-man-pages/launchd.plist.5.html · https://github.com/systemd/systemd/blob/main/man/systemd.service.xml

## Not verified / caveats

- Recent Discord API additions (Components V2, message forwarding, polls) were **not** checked against serenity 0.12.5 or twilight 0.17.1 model coverage. ClaudeClaw's current feature set doesn't use them.
- No crate was compiled or tested for this survey. The API claims come from READMEs, docs.rs method listings and crate source (croner 4.0.1, cron 0.17.0, rust-embed 8.13.0).
- Serenity's `next` branch release timeline is unknown: no milestone or announcement was found.
- I didn't find launchd's actual default `ExitTimeOut` value. The man page says only "system-defined".
- The YAML-alias pitfall (`*/5` unquoted) is based on YAML spec behaviour and wasn't tested against serde-saphyr specifically.

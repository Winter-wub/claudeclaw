# Survey: how Claude Code launchers behave

Research for issue #5 (planning map: #2). Date: 2026-10-13.

**Versions examined:** Claude Code 2.1.296. ccs 8.10.0 (`kaitranntt/ccs` @ `f45fa92`). claude-code-router 3.1.3
(`musistudio/claude-code-router` @ `057abaa`), plus the legacy `ccr code` path at tag `v2.0.0`. LiteLLM,
OpenRouter and Ollama from their docs only.

**How the claims were checked.** Claims marked **[verified]** were reproduced locally. Claude Code (and `ccs`)
were pointed at a loopback fake Anthropic Messages server, so no provider was called. Each run used a throwaway
`CLAUDE_CONFIG_DIR`, and `ccs` additionally ran under a throwaway `HOME` and `CCS_DIR`. Claims marked
**[source]** come from reading the code at the commits above. Claims marked **[docs]** come from vendor
documentation. Claims marked **[unverified]** are inferred and not yet checked.

## Summary

| Launcher | How it starts `claude` | Needs a background server? | stdout pollution in `-p` stream-json | Machine-readable profile/model list | Exit code on provider error | Session caveat |
|---|---|---|---|---|---|---|
| Direct, env only (`ANTHROPIC_BASE_URL` …) | It *is* `claude`. Config comes from env, from `--settings <file\|json>`, or from `CLAUDE_CONFIG_DIR` | No (unless the base URL is a local proxy) | None. Claude Code's own stderr noise stays on stderr | Models: the gateway's `GET /v1/models` (opt-in `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`). No "profiles" | `1`. The `result` event has `is_error:true` and `api_error_status:<http>` **[verified]** | Sessions live under `CLAUDE_CONFIG_DIR` |
| `ccs <profile>`, **settings/API** profile | `spawn(claude, ["--settings", <file>, ...args, --disallowedTools WebSearch, --append-system-prompt <ccs steering>])` with `stdio:inherit` | No (unless the profile is OpenAI-compatible, in which case ccs starts a local translating proxy) | **Yes** for `-p`/`--prompt`: a delegation box **[verified]**. Also yes on first run or after state changes: recovery hints and `[OK] Synchronized marketplace registry paths` **[verified]** | `ccs api list` prints a table only. `ccs env <p> --format raw` gives shell exports | Passes `claude`'s code through (`1` on 429) **[verified]** | Inherits `CLAUDE_CONFIG_DIR` (or a continuity-mapped account dir) |
| `ccs <profile>`, **account** profile | `spawn(claude, args)` with `CLAUDE_CONFIG_DIR=~/.ccs/instances/<name>` and `ANTHROPIC_*` stripped | No | Same first-run/state-change lines. No delegation (the gate only applies to settings profiles) | `ccs auth list --json` → `{version, profiles:[…]}` **[verified]** | Passed through | **Separate session store per account.** You cannot resume across accounts |
| `ccs <provider>`, **CLIProxy** (gemini, codex, agy, …) | Starts or joins a detached `cli-proxy-api` on `127.0.0.1:8317`, then `claude` with `ANTHROPIC_BASE_URL` set to it | **Yes**: a shared detached proxy, ref-counted by session | Same as above. No delegation box | Models: `ccs cliproxy catalog --json` **[verified]**. Provider names are built in and do **not** all appear in `config.yaml` | Passed through. If OAuth is missing, ccs may start an interactive auth flow **[source/unverified for non-TTY]** | Inherits the config dir |
| `ccr <profile> [cli] -- <args>` (CCR **v3**, current) | Runs a generated `/bin/sh` wrapper, which runs a Node middleware, which spawns `claude <args>`. Env sets `ANTHROPIC_BASE_URL=<gateway>` and `CLAUDE_CONFIG_DIR=<profile dir>` | **Yes**: the gateway (default `:3456`) must already be running for Claude Code profiles. It is not auto-started | In print mode stdout is piped through. **Inbound "remote sync" events are written to stdout as `[CCR remote] …`** (on by default) **[source]** | No CLI list (`config.sqlite`). Gateway serves `GET /v1/models` and `GET /health` **[source]** | Child's code passed through **[source]** | Generated profiles get their own `CLAUDE_CONFIG_DIR` |
| `ccr code` (CCR **≤ 2.x**, legacy) | Re-serialises argv through `minimist` and runs it with `shell:true`, adding `--settings <tmp json>` | Yes. Auto-starts the service and **kills it again on exit** when the ref count reaches 0 | `Service not running, starting service...` on **stdout**. Argv is mangled (see below) **[source]** | `ccr model` (interactive) | `code \|\| 0`, so a signal exit becomes 0 **[source]** | Inherits the config dir |
| LiteLLM proxy (and other gateways) | Env only: `ANTHROPIC_BASE_URL=http://host:4000`, `ANTHROPIC_AUTH_TOKEN=<key>` | Yes: a separate long-running proxy (`litellm --config …`) | None from the launch itself | `GET /v1/models` (OpenAI-style). Health: `GET /health` **[docs]** | Same as direct | Same as direct. See the cross-provider note |

**Key takeaway.** Wrapper launchers are not transparent. Beyond the known `-p` box:

1. **ccs drops the daemon's system prompt.** For settings and CLIProxy profiles, ccs appends its own
   `--append-system-prompt`, and Claude Code keeps only the **last** occurrence. The daemon's appended system
   prompt is silently dropped. This is **[verified]** end to end through `ccs fake --print … --append-system-prompt X`:
   `X` was absent from the request, and ccs's WebSearch steering text was present.
2. **Wrappers write non-JSON lines to stdout sometimes, not on every run.** ccs does it on first run or after a
   state change. CCR v3 does it on remote events.

A generic launcher therefore needs per-launcher argv rewrites, a strict NDJSON reader that tolerates stray
lines, and a way to declare which flags the wrapper itself injects.

---

## 1. ccs (Claude Code Switch) 8.10.0

### 1.1 `-p` delegation: confirmed against current source

- **The gate** is `src/ccs.ts` L84-95. If **any** argv element is `-p` or `--prompt`, or starts with
  `--prompt=`, **and** `resolvedTarget === 'claude' && profileInfo.type === 'settings'`, ccs routes to
  `DelegationHandler`. `--print` is not matched. The check uses `args.some(...)`, so a *value* equal to `-p`
  (for example, a prompt that is literally `-p`) also trips it. Rewriting only the first `-p` to `--print` is
  correct but not complete.
- **In delegation**, `src/delegation/headless-executor.ts` (~L295, L483) builds its own argv:
  `-p <prompt> --settings <file> --output-format stream-json --verbose [--permission-mode …] [--resume …] …`.
  It then spawns `claude` with `stdio: ['ignore','pipe','pipe']`. Because stdin is ignored, a prompt sent on
  stdin is lost. ccs consumes the stream-json itself and prints `ResultFormatter.format()` to stdout
  (`src/delegation/result-formatter.ts`): a box such as `[i] Delegated to <MODEL> (ccs:<profile>)` followed by
  a table. **[verified]**. The exit code is `result.exitCode || 0`. A 429 gave exit `1` **[verified]**.
  Delegation also uses its own session store (`<profile>:continue`).
- **Image analysis.** Settings profiles also run image-analysis preparation. When Gemini auth is missing, ccs
  prints an info line to **stderr**, which is harmless.

### 1.2 How the profile types exec `claude`

| Type | File | Exec |
|---|---|---|
| `settings` (API) | `src/dispatcher/flows/settings-flow.ts` (L380-433) | `execClaude(claude, ["--settings", <settings.json>, ...args + steering], env)`. It strips inherited `ANTHROPIC_BASE_URL/AUTH_TOKEN/API_KEY` unless the settings file sets them (`stripAnthropicRoutingEnv`, `src/utils/shell-executor.ts`). Some provider names run a **preflight key check** first (`glm`, `mm`, and Anthropic-direct keys). On failure it writes to stderr and calls `exit(1)`, so `claude` never runs. Skip with `CCS_SKIP_PREFLIGHT=1`. OpenAI-compatible profiles first start a local translating proxy (`startOpenAICompatProxy`). |
| `account` | `src/dispatcher/flows/account-flow.ts` | `execClaude(claude, args, {CLAUDE_CONFIG_DIR: ~/.ccs/instances/<name>, CCS_PROFILE_TYPE:'account', …})`. All `ANTHROPIC_*` are stripped (`stripAnthropicEnv`). It warns on **stderr** if `--resume <id>` points at a session in a different "lane" (`src/auth/resume-lane-warning.ts`). |
| `cliproxy` | `src/dispatcher/flows/cliproxy-flow.ts` and `src/cliproxy/executor/index.ts` | It ensures provider OAuth, then calls `checkOrJoinProxy`. If needed it runs `spawnProxy()` (detached, `stdio:'ignore'`, `src/cliproxy/executor/lifecycle-manager.ts`) on port `8317` (`src/cliproxy/config/port-manager.ts`), waits for TCP readiness, and only then spawns `claude` with `ANTHROPIC_BASE_URL/AUTH_TOKEN/MODEL` pointing at the proxy. A remote proxy (`cliproxy_server`) and an HTTPS tunnel are optional. |
| `default` | `default-flow.ts` | Plain `claude` with `ANTHROPIC_*` stripped. |

`execClaude` spawns with `stdio:'inherit'` and forwards SIGINT, SIGTERM and SIGHUP. On child exit it re-raises
the signal or calls `exit(code)` (`src/utils/signal-forwarder.ts`). In the non-delegation paths, **stdout is
`claude`'s own** apart from the pre-launch lines below.

### 1.3 Argv that ccs adds, and the system-prompt clobber

`appendThirdPartyWebSearchToolArgs` (`src/utils/websearch/claude-tool-args.ts`) runs for settings and CLIProxy
launches. It does two things:

- It **always** adds or merges `--disallowedTools WebSearch`.
- When `websearch.enabled` is true (`config.websearch?.enabled ?? true`, so on by default when a provider such
  as DuckDuckGo is available), it **appends** an `--append-system-prompt <steering>` pair. If the caller used
  `--append-system-prompt-file`, it appends a `--append-system-prompt-file <~/.ccs/prompts/…>` pair instead.
  `src/utils/prompt-injection-strategy.ts` deliberately uses the same mode as the caller.

Claude Code keeps only the **last** `--append-system-prompt` and the **last** `--append-system-prompt-file`.
One of each is concatenated. All of this is **[verified]** on 2.1.296. So the caller's appended prompt is
**replaced** by ccs's steering text. The current TypeScript daemon is affected today whenever it uses a
settings or CLIProxy profile with ccs WebSearch enabled.

Mitigations, in order of robustness:

1. Set `websearch: { enabled: false }` in the ccs config. This removes the steering pair. `--disallowedTools`
   is still added.
2. Pass the daemon prompt in the *other* form from the one ccs will choose. For example, pass it inline *and*
   pass any file form first. This is fragile.
3. Treat "injects `--append-system-prompt`" as a declared launcher capability and refuse the combination.

### 1.4 stdout pollution outside delegation [verified]

- **Auto-recovery hints** (`src/dispatcher/pre-dispatch.ts` → `RecoveryManager.showRecoveryHints`) print to
  stdout, for example `[i] Auto-recovery completed: …` and `[i] Next step: Login to Claude CLI`. This happens
  when ccs creates missing dirs or files.
- **Plugin-registry sync** (`src/management/shared-manager/plugin-metadata-normalizer.ts`) prints
  `[OK] Synchronized marketplace registry paths` whenever it rewrites `known_marketplaces.json`. ccs also writes
  that file into `~/.claude` and into `CLAUDE_CONFIG_DIR`. Both lines appeared on the first sandboxed `--print`
  run and not on the second.
- **TTY-gated output** does not affect a daemon: the update box and the first-run banner check
  `process.stdout.isTTY`.
- **`CCS_NO_PRE_DISPATCH=1`** is an internal guard used by `ccsx auth use`. It skips auto-migrate and recovery.
  It is undocumented, so do not depend on it.

### 1.5 Profile and model listing

- `ccs auth list --json` prints `{ "version": "8.10.0", "profiles": [...] }` (account profiles) **[verified]**.
- `ccs api list` prints a table only, with no `--json` (`src/commands/api-command/list-command.ts`). CLIProxy
  variants appear in the same table.
- `ccs cliproxy catalog --json` returns a per-provider model catalog (`{ "agy": [ {id, name, contextWindow, thinking…} ], … }`) **[verified]**.
- `ccs env <profile> --format raw|anthropic|openai [--shell bash|fish|powershell]` prints shell `export`
  lines with the profile's effective env **[verified]**. This can "flatten" a settings profile into an env-only
  launch. A CLIProxy profile still needs its proxy running.
- **Profile sources.** Profiles come from `config.yaml` (`accounts`, `profiles`, `cliproxy.variants`, …) plus
  **built-in** CLIProxy provider names (`gemini`, `codex`, `agy`, `qwen`, …) that need not appear in the file.
  Reading only `config.yaml` therefore misses those built-ins. The config directory honours `CCS_DIR` and
  `CCS_HOME` (`src/utils/config-manager.ts`).

---

## 2. claude-code-router (CCR)

### 2.1 v3 (current, 3.0.0 released 2026-07, 3.1.3 released 2026-10-09)

`ccr code` and `ccr activate` **no longer exist**. The CLI (`packages/cli/src/cli.ts`) offers:

- service commands: `start`, `ui`, `stop`, `serve`/`web`
- `ccr <profile name|id> [cli|app] [-- <agent args>]`

`docs/src/content/docs/en/guides/cli.md` documents these.

- **Launch plan.** `buildClaudeCodeLaunchPlan` (`packages/core/src/profiles/launch-core.ts` L311) sets the
  command to `~/.claude-code-router/bin/ccr-claude-code-wrapper-<slug>` and the env to `CLAUDE_CONFIG_DIR`
  (the profile's settings dir), `CCR_PROFILE_SURFACE`, the model env, and `TZ`.
- **The wrapper** (`profiles/service.ts` `claudeCodeWrapperShellScript`) is a `/bin/sh` script. It exports
  `ANTHROPIC_BASE_URL=<gateway>` and `CLAUDE_CONFIG_DIR`, unsets first-party provider vars, and execs a Node
  middleware.
- **The middleware** (`agents/codex/cli-middleware-runtime.ts` `runClaudeCodeCliWrapper`) spawns the real
  `claude` (override with `CCR_CLAUDE_CODE_BIN`) with the args **passed through**. It may prepend
  `--mcp-config <file>`. Under `-p`/`--print` it **pipes** stdout and re-writes it unchanged.
- **Pollution.** Remote sync is enabled by default (the wrapper sets `CCR_REMOTE_SYNC_ENDPOINT`). Inbound
  remote events are written to stdout as `\n[CCR remote] <text>\n`. Disable with `CCR_REMOTE_SYNC_ENABLED=0`
  or `CCR_REMOTE_SYNC_NOTIFY_INBOUND=0`. Because the wrapper only sets those vars when they are empty, the
  caller's env wins. **[source]**
- **Server.** For Claude Code profiles the gateway must already be running. `shouldAutoStartProfileGateway` is
  true only for grok, kimi and pi. Otherwise you get `ProfileGatewayUnavailableError`. Run `ccr start` or
  `ccr serve` (foreground, for a supervisor). The gateway listens on `127.0.0.1:3456` and the management UI on
  `:3458`. The gateway exposes `GET /health` and `GET /v1/models`
  (`packages/core/src/gateway/http/request-handler.ts`).
- **Listing.** No CLI or JSON list exists. Profiles live in `config.sqlite`. Models are available through the
  gateway's `/v1/models`, which needs a CCR client key **[unverified]**.
- **Exit code.** `waitForChild`, then `process.exitCode = code`.
- **Sessions.** Profiles with a generated scope get an isolated `CLAUDE_CONFIG_DIR`, so sessions do not cross
  profiles.

### 2.2 Legacy `ccr code` (≤ 2.0.0), from tag `v2.0.0` `packages/cli/src/utils/codeCommand.ts`

- **Env.** `createEnvVariables` sets `ANTHROPIC_AUTH_TOKEN=<APIKEY or "test">`,
  `ANTHROPIC_BASE_URL=http://127.0.0.1:<PORT|3456>`, `API_TIMEOUT_MS`, `DISABLE_TELEMETRY`, and so on. The env
  goes into a temp file passed as `--settings` (`ccr activate` / `ccr env` print the same vars).
- **Argv is not passed through.** It goes through `minimist(args)` and is rebuilt as `--key "JSON.stringify(value)"`,
  then run with `spawn(…, {shell:true})`. As a result:
  - positional args are dropped;
  - repeated flags become arrays;
  - multi-line values arrive with a literal `\n`;
  - `$`/backticks inside prompts are expanded by the shell.

  This is unsafe for daemon prompts. **[source]**, not executed.
- **Service.** If the service isn't running, `ccr code` prints `Service not running, starting service...` to
  **stdout**, spawns `ccr start` detached, and waits. On exit, `closeService()` SIGTERMs the service once the
  ref count is 0. So a daemon making sequential calls pays a cold start on every call.
- **Exit code.** `process.exit(code || 0)`.

---

## 3. Env-only, `--settings`, and `CLAUDE_CONFIG_DIR` (direct `claude`)

[docs: https://code.claude.com/docs/en/env-vars, /llm-gateway-connect, /headless]

- **Routing and auth.**
  - `ANTHROPIC_BASE_URL` sets the endpoint.
  - `ANTHROPIC_AUTH_TOKEN` is sent as `Authorization: Bearer` and takes precedence over a saved login.
  - `ANTHROPIC_API_KEY` is sent as `x-api-key`. Interactive mode asks for approval once; `-p` does not.
  - `apiKeyHelper` (a settings key) is sent in both headers. Its TTL is set by `CLAUDE_CODE_API_KEY_HELPER_TTL_MS`.
  - `ANTHROPIC_CUSTOM_HEADERS` adds extra headers.
  - Bedrock, Vertex, Foundry and AWS routes use their own `*_BASE_URL` and `CLAUDE_CODE_USE_*`/`*_SKIP_*_AUTH` vars.
- **Models.**
  - `ANTHROPIC_MODEL` is overridden by `--model`.
  - `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL` (plus `FABLE` per OpenRouter) set the tier models.
  - `ANTHROPIC_SMALL_FAST_MODEL` is deprecated.
  - A non-Claude model name logs `[claude-code:unrecognized_model]` to **stderr** but works **[verified]**.
- **Reliability knobs.** `API_TIMEOUT_MS` (default 600000). `CLAUDE_CODE_MAX_RETRIES` (default 10, cap 15).
  `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`.
- **`--settings <file-or-json>`.** Its `env` block applies to the run and **overrides the same variable
  exported in the shell**. You cannot unset a variable from it; set it to `""`. This makes `--settings` the
  most robust way to pin a profile per invocation.
- **`CLAUDE_CONFIG_DIR`** changes where settings, credentials and **sessions** live. Project and local
  settings cannot set it.
- **Model discovery.** With `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`, Claude Code queries the gateway's
  model list. This only populates the `/model` picker, so listing for the daemon has to hit the gateway itself.
- **Errors under `-p --output-format stream-json`** **[verified]** against a fake server returning 429 and 401
  with `CLAUDE_CODE_MAX_RETRIES=1`:
  - Each retry emits `{"type":"system","subtype":"api_retry","attempt","max_retries","retry_delay_ms","error_status":429,"error":"rate_limit"}`.
    The category for a 401 is `authentication_failed`.
  - The final event is `{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"API Error: Request rejected (429) · …"}`.
    Note that `subtype` is still `"success"`.
  - The process exits **1**. stderr holds nothing useful.
  - **Classify on `is_error` and `api_error_status`, not on `subtype`.**
  - SIGTERM exits 143 and records no result for the turn.

## 4. LiteLLM-style proxies and other wrappers

- **LiteLLM** [docs: https://docs.litellm.ai/docs/tutorials/claude_responses_api]
  - Run `litellm --config config.yaml`, which listens on `:4000` and must stay running.
  - Set `ANTHROPIC_BASE_URL=http://host:4000` (unified endpoint, `/v1/messages`) or `…/anthropic` (pass-through),
    and `ANTHROPIC_AUTH_TOKEN=<master or virtual key>`.
  - The `model_name` values in the config are what Claude Code sends, so map the `ANTHROPIC_DEFAULT_*_MODEL`
    vars to them.
  - Health is `GET /health`. Models are available through the OpenAI-style `GET /v1/models` **[unverified for
    the current version]**.
  - Gateways that shrink the context window or rewrite errors break auto-compaction. Set
    `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, and use `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS=1` when the upstream
    rejects beta fields (Claude Code gateway troubleshooting table).
- **OpenRouter** (direct, no local process) [docs]:
  - `ANTHROPIC_BASE_URL=https://openrouter.ai/api`
  - `ANTHROPIC_AUTH_TOKEN=<key>`
  - `ANTHROPIC_API_KEY=""`, which **must be explicitly empty**. This means the launcher must be able to *blank*
    or unset inherited env.
- **Anthropic-compatible vendor endpoints** (Z.AI/GLM, Kimi/Moonshot, DeepSeek, MiniMax): same env-only shape
  with a vendor base URL. ccs's settings profiles are exactly this.
- **Ollama** (≥ 0.14 has `/v1/messages`, ≥ 0.15 has `ollama launch claude`) [docs/press]: env-only against
  `http://localhost:11434`, with a placeholder token. How `ollama launch claude` passes args through and how
  clean its stdout is are **[unverified]**.
- **Settings-rewriting switchers** (for example cc-switch-style GUIs) edit `~/.claude/settings.json` globally
  instead of wrapping. They need no launcher, but they change the daemon's provider underneath it. Prefer
  `--settings` per call. **[unverified, not surveyed in depth]**

## 5. Session / resume compatibility

- **Sessions are stored per `CLAUDE_CONFIG_DIR`.** Any launcher that changes it does not see sessions created
  under another dir: ccs account profiles, CCR v3 generated profiles, or an explicit env. ccs prints a
  "resume lane" warning on stderr; others fail with "no conversation found" **[source; exact message unverified]**.
- **Delegation (`ccs … -p`) keeps its own session bookkeeping**, which is another reason never to hit it.
- **Resuming across providers** in the same config dir is mechanically possible. However, provider-specific
  content (for example thinking-block signatures from one backend) may be rejected by another **[unverified]**.
  The current daemon's rule of resetting the session on a launcher change is the safe default. Keep it.

---

## Implications for a generic launcher model

A launcher definition must be able to express:

1. **An argv template.** Use a prefix and a splice point for the daemon's argv, such as
   `["ccs", "{profile}", "{claude_args}"]`, `["ccr", "{profile}", "cli", "--", "{claude_args}"]` or
   `["claude", "{claude_args}"]`. An optional `--` separator is needed for CCR v3.
2. **Arg rewrites** that run after the daemon builds its argv. For example, ccs needs `-p` → `--print`.
   Rewrites should match **flags only**, not values. Today's first-occurrence rule is fragile because ccs
   checks every element. A rule that rejects or escapes a value equal to `-p`/`--prompt`/`--prompt=…` closes
   the gap.
3. **Injected-flag declarations.** A launcher may itself add flags (ccs adds `--append-system-prompt`,
   `--disallowedTools` and `--settings`; CCR adds `--mcp-config`). Since Claude Code takes the last value,
   the daemon must know which of its own flags would be clobbered. It should then either move its content to
   another transport or refuse to start. The cheapest fix today is to disable ccs WebSearch steering.
4. **Env set and env unset/blank.** OpenRouter needs `ANTHROPIC_API_KEY=""`. ccs and CCR read
   `CCS_DIR`/`CCR_*`. Also include an optional `--settings` file or JSON that the daemon passes itself; this
   is the most reliable per-call way to pin a profile, and it overrides shell env.
5. **A session namespace.** This is the effective `CLAUDE_CONFIG_DIR`, or a declared "isolates config dir"
   flag. Key stored session IDs by (launcher, profile, config dir) and reset on change.
6. **Pre-start and health checks.**
   - An optional `ensure` command, for example `ccr start` or `litellm --config …` run under a supervisor.
   - An optional `health` probe: a TCP port, `GET /health` (CCR, LiteLLM), or TCP `127.0.0.1:8317` (CLIProxy).
     Probes must not cost tokens.
   - A declared "server is lazily started by the wrapper" mode, which covers ccs CLIProxy and legacy `ccr code`.
7. **Profile and model discovery** as a command, a file, or an HTTP call, plus a parser:
   - `ccs auth list --json` → `.profiles[]`
   - ccs `config.yaml` keys plus the built-in provider list
   - `ccs cliproxy catalog --json`
   - `GET <base>/v1/models`
   - "none" for env-only

   Discovery should degrade to free text when it fails, as it does today.
8. **An output contract.** Read stdout as NDJSON but **skip and log non-JSON lines** instead of failing.
   Watch for ccs recovery and sync lines and `[CCR remote]`. Treat "no `result` event" as a launcher failure
   even if the exit code is 0, which covers the delegation box and legacy CCR's `code || 0`. Classify provider
   errors from `result.is_error` plus `result.api_error_status`, and from the `system/api_retry` events
   (`error` = `rate_limit`, `authentication_failed`, …). Do not rely on the exit code alone.
9. **Non-interactive guarantees.** stdin should be `/dev/null` or a pipe. Wrappers may try interactive auth
   (ccs CLIProxy OAuth) or preflight checks that exit early (ccs `glm`/`mm` key validation; skip with
   `CCS_SKIP_PREFLIGHT=1`). Surface the stderr tail when no `result` is produced.

## Unverified / open items

- CCR v3 and legacy `ccr code` were read in source only, not executed. That includes the `[CCR remote]`
  stdout write and the minimist argv mangling.
- How ccs CLIProxy behaves with missing OAuth when there is no TTY (does it hang or exit?).
- The exact Claude Code message when `--resume` hits an ID that is absent from the current config dir.
- Whether thinking-signature or provider-specific content breaks a cross-provider `--resume`.
- The authentication requirement and response shape of LiteLLM's and CCR's `/v1/models`.
- `ollama launch claude` argv passthrough and stdout cleanliness.
- Image-analysis argv injection in ccs (`appendThirdPartyImageAnalysisToolArgs`) when Gemini auth *is* present.
  It may add another system-prompt pair.

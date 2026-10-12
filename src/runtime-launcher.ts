import { CLAUDE_EXECUTABLE } from "./executable";
import { getSettings } from "./config";
import type { RuntimeLauncherConfig } from "./config";

const GLOBAL_DEFAULT: RuntimeLauncherConfig = { kind: "direct", directFallback: true };

/**
 * Resolve the launcher for a scope chain, most specific first.
 *
 * A scope key is whatever the runner passes as `threadId`: a Discord channel or
 * thread snowflake, a cron job name, or `agent:<agentName>`. Passing a chain is
 * how a Discord thread inherits its parent channel's choice before falling back
 * to the global default — e.g. `[threadId, parentChannelId]`.
 *
 * Stays synchronous: it reads the config module cache, and every spawn site
 * depends on being able to call it inline while building argv.
 */
export function getRuntimeLauncher(scopeChain?: string[]): RuntimeLauncherConfig {
  const runtime = getSettings().runtime;
  if (scopeChain && runtime?.launchers) {
    for (const key of scopeChain) {
      if (!key) continue;
      const scoped = runtime.launchers[key];
      if (scoped) return scoped;
    }
  }
  return runtime?.launcher ?? GLOBAL_DEFAULT;
}

export function buildClaudeCommand(baseArgs: string[], scopeChain?: string[]): string[] {
  const launcher = getRuntimeLauncher(scopeChain);
  if (launcher.kind === "ccs") {
    if (!launcher.profile) throw new Error("[runtime] CCS mode requires a profile. Set runtime.launcher.profile in settings.json.");
    // `ccs` intercepts -p/--prompt and, for API-type profiles (those with a settings
    // file: openrouter, studio), routes them through its delegation formatter, which
    // decorates stdout with a summary box. That is not valid stream-json, so the
    // daemon parses nothing and the reply comes back empty.
    //
    // Its gate (ccs.js) only matches "-p", "--prompt" and "--prompt=" -- not
    // "--print", claude's equivalent long form -- so swapping the flag reaches claude
    // directly for every profile type. Only the first occurrence is rewritten, since
    // a later "-p" would be prompt text rather than the flag.
    const args = [...baseArgs];
    const flagAt = args.indexOf("-p");
    if (flagAt !== -1) args[flagAt] = "--print";
    return ["ccs", launcher.profile, ...args];
  }
  return [CLAUDE_EXECUTABLE, ...baseArgs];
}

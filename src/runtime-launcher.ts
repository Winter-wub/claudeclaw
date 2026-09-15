import { CLAUDE_EXECUTABLE } from "./executable";
import { getSettings } from "./config";

export function getRuntimeLauncher() {
  return getSettings().runtime?.launcher ?? { kind: "direct" as const, directFallback: true };
}

export function buildClaudeCommand(baseArgs: string[]): string[] {
  const launcher = getRuntimeLauncher();
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

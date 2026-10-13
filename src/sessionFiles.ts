import { existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Must match Claude Code's JSONL directory sanitizer (slashes, backslashes, dots → dashes). */
export function sanitizeProjectSlug(cwd: string): string {
  return cwd.replace(/[/\\.]/g, "-");
}

// Bun's os.homedir() ignores a HOME changed at runtime (Node honours it), so read the
// env first; this is what made the session-file tests fail under bun.
function home(): string {
  return process.env.HOME || homedir();
}

export function getClaudeProjectDir(cwd: string = process.cwd()): string {
  return join(home(), ".claude", "projects", sanitizeProjectSlug(cwd));
}

/**
 * Resolve the Claude Code transcript JSONL for a session id.
 * Tries the cwd-derived project dir first, then scans ~/.claude/projects.
 */
export function findSessionJsonlPath(sessionId: string, cwd: string = process.cwd()): string | null {
  if (!UUID_RE.test(sessionId)) return null;

  const direct = join(getClaudeProjectDir(cwd), `${sessionId}.jsonl`);
  if (existsSync(direct)) return direct;

  const projectsRoot = join(home(), ".claude", "projects");
  if (!existsSync(projectsRoot)) return null;

  let newest: { path: string; mtimeMs: number } | null = null;
  for (const entry of readdirSync(projectsRoot, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const candidate = join(projectsRoot, entry.name, `${sessionId}.jsonl`);
    if (!existsSync(candidate)) continue;
    const mtimeMs = statSync(candidate).mtimeMs;
    if (!newest || mtimeMs > newest.mtimeMs) {
      newest = { path: candidate, mtimeMs };
    }
  }

  return newest?.path ?? null;
}

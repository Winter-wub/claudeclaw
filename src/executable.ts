import { execSync } from "child_process";
import { join, dirname } from "path";
import { existsSync } from "fs";

export function resolveClaudeExecutable(): string {
  if (process.platform !== "win32") return "claude";
  try {
    const out = execSync("where claude", { encoding: "utf8" });
    const cmdPath = out
      .split(/\r?\n/)
      .map((s) => s.trim())
      .find((s) => s.toLowerCase().endsWith(".cmd"));
    if (!cmdPath) return "claude";
    const exePath = join(
      dirname(cmdPath),
      "node_modules",
      "@anthropic-ai",
      "claude-code",
      "bin",
      "claude.exe"
    );
    return existsSync(exePath) ? exePath : "claude";
  } catch {
    return "claude";
  }
}

export const CLAUDE_EXECUTABLE = resolveClaudeExecutable();

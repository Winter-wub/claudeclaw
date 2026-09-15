/**
 * Test preload: move the process into a throwaway directory before any test module
 * is imported.
 *
 * Several modules capture their settings path at import time as
 * `join(process.cwd(), ".claude", "claudeclaw", "settings.json")` (see src/config.ts
 * and src/ui/constants.ts), and several test files write to that path. Running
 * `bun test` from a real ClaudeClaw working directory therefore overwrites — and in
 * some orderings deletes — the live settings.json, after which the running daemon
 * recreates it from defaults and the Discord token, jobsDir and model are gone.
 *
 * Because bunfig.toml preloads run before test modules are loaded, chdir-ing here
 * fixes every test file at once without each one needing its own guard.
 */
import { mkdtempSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

const sandbox = mkdtempSync(join(tmpdir(), "claudeclaw-test-cwd-"));
process.chdir(sandbox);

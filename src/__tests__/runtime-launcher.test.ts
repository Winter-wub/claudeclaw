import { describe, it, expect, beforeEach } from "bun:test";
import { mkdir, writeFile } from "fs/promises";
import { join } from "path";
import { buildClaudeCommand, getRuntimeLauncher } from "../runtime-launcher";
import { loadSettings, resetSettingsCache } from "../config";


const SETTINGS_DIR = join(process.cwd(), ".claude", "claudeclaw");
const SETTINGS_FILE = join(SETTINGS_DIR, "settings.json");

// These assertions describe a *direct* launcher, so the test must supply its own
// settings.json rather than reading whatever the daemon happens to be configured
// with -- otherwise it fails whenever the runtime is switched to ccs.

beforeEach(async () => {
  await mkdir(SETTINGS_DIR, { recursive: true });
  await writeFile(SETTINGS_FILE, JSON.stringify({
    runtime: { launcher: { kind: "direct", directFallback: true } },
  }, null, 2) + "\n");
  resetSettingsCache();
  await loadSettings();
});


describe("buildClaudeCommand", () => {
  it("direct mode returns [claude, ...args]", () => {
    const result = buildClaudeCommand(["-p", "hello"]);
    expect(result[0]).toBe("claude");
    expect(result.slice(1)).toEqual(["-p", "hello"]);
  });

  it("args are immutable (no mutation of input)", () => {
    const input = ["-p", "hello"];
    const original = [...input];
    buildClaudeCommand(input);
    expect(input).toEqual(original);
  });
});

describe("getRuntimeLauncher", () => {
  it("returns default launcher when settings not configured for runtime", () => {
    const launcher = getRuntimeLauncher();
    expect(launcher.kind).toBe("direct");
    expect(launcher.directFallback).toBe(true);
  });
});

describe("buildClaudeCommand with CCS mode", () => {
  it("throws when CCS mode has no profile", async () => {
    // Manually set runtime to CCS without profile by monkey-patching
    const { getSettings } = await import("../config");
    const settings = getSettings();
    const originalRuntime = settings.runtime;
    // @ts-expect-error - testing error path
    settings.runtime = { launcher: { kind: "ccs" } };
    expect(() => buildClaudeCommand(["-p", "test"])).toThrow("CCS mode requires a profile");
    // Restore
    // @ts-expect-error
    settings.runtime = originalRuntime;
  });
});

describe("buildClaudeCommand ccs flag rewrite", () => {
  it("rewrites -p to --print so ccs does not hijack API profiles into delegation mode", async () => {
    const { getSettings } = await import("../config");
    const settings = getSettings();
    const originalRuntime = settings.runtime;
    // @ts-expect-error - exercising the ccs path
    settings.runtime = { launcher: { kind: "ccs", profile: "openrouter" } };
    const out = buildClaudeCommand(["-p", "hi", "--output-format", "stream-json"]);
    expect(out.slice(0, 2)).toEqual(["ccs", "openrouter"]);
    expect(out).toContain("--print");
    expect(out).not.toContain("-p");
    // @ts-expect-error
    settings.runtime = originalRuntime;
  });

  it("only rewrites the flag, never prompt text that happens to be -p", async () => {
    const { getSettings } = await import("../config");
    const settings = getSettings();
    const originalRuntime = settings.runtime;
    // @ts-expect-error - exercising the ccs path
    settings.runtime = { launcher: { kind: "ccs", profile: "codex" } };
    const out = buildClaudeCommand(["-p", "-p"]);
    expect(out).toEqual(["ccs", "codex", "--print", "-p"]);
    // @ts-expect-error
    settings.runtime = originalRuntime;
  });

  it("leaves -p alone in direct mode", () => {
    const out = buildClaudeCommand(["-p", "hi"]);
    expect(out).toEqual(["claude", "-p", "hi"]);
  });
});

describe("scope chain resolution", () => {
  async function withSettings(runtime: unknown) {
    await writeFile(SETTINGS_FILE, JSON.stringify({ runtime }, null, 2) + "\n");
    resetSettingsCache();
    await loadSettings();
  }

  const GLOBAL_DIRECT = { launcher: { kind: "direct", directFallback: true } };

  it("a scoped entry wins over the global default", async () => {
    await withSettings({ ...GLOBAL_DIRECT, launchers: { "chan-1": { kind: "ccs", profile: "openrouter" } } });
    expect(buildClaudeCommand(["-p", "hi"], ["chan-1"])).toEqual(["ccs", "openrouter", "--print", "hi"]);
  });

  it("falls back to the global default when the scope has no entry", async () => {
    await withSettings({ ...GLOBAL_DIRECT, launchers: { "chan-1": { kind: "ccs", profile: "openrouter" } } });
    expect(buildClaudeCommand(["-p", "hi"], ["chan-other"])).toEqual(["claude", "-p", "hi"]);
  });

  it("walks the chain thread -> parent channel -> global", async () => {
    await withSettings({ ...GLOBAL_DIRECT, launchers: { parent: { kind: "ccs", profile: "codex" } } });
    // thread itself has no entry, so it inherits the parent channel's
    expect(getRuntimeLauncher(["thread", "parent"]).profile).toBe("codex");
    // the thread's own entry takes precedence over the parent's
    await withSettings({
      ...GLOBAL_DIRECT,
      launchers: { parent: { kind: "ccs", profile: "codex" }, thread: { kind: "ccs", profile: "agy" } },
    });
    expect(getRuntimeLauncher(["thread", "parent"]).profile).toBe("agy");
    // neither present -> global
    expect(getRuntimeLauncher(["nope", "also-nope"]).kind).toBe("direct");
  });

  it("ignores the map entirely when no scope is passed", async () => {
    await withSettings({ ...GLOBAL_DIRECT, launchers: { "chan-1": { kind: "ccs", profile: "openrouter" } } });
    expect(buildClaudeCommand(["-p", "hi"])).toEqual(["claude", "-p", "hi"]);
  });

  it("skips empty/undefined keys in the chain", async () => {
    await withSettings({ ...GLOBAL_DIRECT, launchers: { parent: { kind: "ccs", profile: "codex" } } });
    expect(getRuntimeLauncher(["", "parent"]).profile).toBe("codex");
  });

  it("a scoped ccs entry with no profile throws, like the global one", async () => {
    await withSettings({ ...GLOBAL_DIRECT, launchers: { "chan-1": { kind: "ccs" } } });
    expect(() => buildClaudeCommand(["-p", "hi"], ["chan-1"])).toThrow("CCS mode requires a profile");
  });
});

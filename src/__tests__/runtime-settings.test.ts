import { describe, it, expect, beforeEach } from "bun:test";
import { mkdir, writeFile, readFile } from "fs/promises";
import { join } from "path";
import {
  readRuntimeSettings,
  updateRuntimeSettings,
  readModelSetting,
  updateModelSetting,
  deleteRuntimeOverride,
  hasRuntimeOverride,
  listRuntimeOverrides,
} from "../ui/services/settings";
import { listCcsProfiles } from "../ui/services/ccs-profiles";


const SETTINGS_DIR = join(process.cwd(), ".claude", "claudeclaw");
const SETTINGS_FILE = join(SETTINGS_DIR, "settings.json");


beforeEach(async () => {
  await mkdir(SETTINGS_DIR, { recursive: true });
  await writeFile(SETTINGS_FILE, JSON.stringify({
    model: "opus",
    runtime: { launcher: { kind: "ccs", profile: "codex", directFallback: true } },
  }, null, 2) + "\n");
});


describe("readRuntimeSettings", () => {
  it("reads the launcher off disk", async () => {
    const l = await readRuntimeSettings();
    expect(l.kind).toBe("ccs");
    expect(l.profile).toBe("codex");
  });

  it("defaults to direct when runtime block is absent", async () => {
    await writeFile(SETTINGS_FILE, JSON.stringify({ model: "opus" }, null, 2) + "\n");
    const l = await readRuntimeSettings();
    expect(l.kind).toBe("direct");
    expect(l.profile).toBe("");
  });
});

describe("updateRuntimeSettings", () => {
  it("switches to direct and keeps the previous profile for toggling back", async () => {
    const l = await updateRuntimeSettings({ kind: "direct" });
    expect(l.kind).toBe("direct");
    const onDisk = JSON.parse(await readFile(SETTINGS_FILE, "utf-8"));
    expect(onDisk.runtime.launcher.kind).toBe("direct");
    expect(onDisk.runtime.launcher.profile).toBe("codex");
  });

  it("switches profile while staying on ccs", async () => {
    const l = await updateRuntimeSettings({ kind: "ccs", profile: "agy" });
    expect(l.kind).toBe("ccs");
    expect(l.profile).toBe("agy");
  });

  it("refuses ccs with no profile (would throw on every spawn)", async () => {
    await expect(updateRuntimeSettings({ kind: "ccs", profile: "" })).rejects.toThrow(/profile must be selected/i);
    // and leaves the file untouched
    const onDisk = JSON.parse(await readFile(SETTINGS_FILE, "utf-8"));
    expect(onDisk.runtime.launcher.profile).toBe("codex");
  });

  it("preserves unrelated settings keys", async () => {
    await updateRuntimeSettings({ kind: "ccs", profile: "openrouter" });
    const onDisk = JSON.parse(await readFile(SETTINGS_FILE, "utf-8"));
    expect(onDisk.model).toBe("opus");
  });
});

describe("listCcsProfiles", () => {
  it("never throws and reports availability", async () => {
    const out = await listCcsProfiles();
    expect(typeof out.available).toBe("boolean");
    expect(Array.isArray(out.groups)).toBe(true);
    for (const g of out.groups) {
      expect(typeof g.label).toBe("string");
      expect(g.names.length).toBeGreaterThan(0);
    }
  });
});

describe("model setting", () => {
  it("reads the model off disk", async () => {
    expect(await readModelSetting()).toBe("opus");
  });

  it("sets a tier and preserves the rest of the file", async () => {
    const m = await updateModelSetting("sonnet");
    expect(m).toBe("sonnet");
    const onDisk = JSON.parse(await readFile(SETTINGS_FILE, "utf-8"));
    expect(onDisk.model).toBe("sonnet");
    expect(onDisk.runtime.launcher.profile).toBe("codex");
  });

  it("treats empty string as 'let Claude Code decide' and trims input", async () => {
    expect(await updateModelSetting("   ")).toBe("");
    const onDisk = JSON.parse(await readFile(SETTINGS_FILE, "utf-8"));
    expect(onDisk.model).toBe("");
  });

  it("defaults to empty when the key is absent", async () => {
    await writeFile(SETTINGS_FILE, JSON.stringify({ runtime: {} }, null, 2) + "\n");
    expect(await readModelSetting()).toBe("");
  });
});

describe("per-scope launcher overrides", () => {
  it("a scoped write lands in launchers[key] and leaves the global default alone", async () => {
    const l = await updateRuntimeSettings({ kind: "ccs", profile: "agy" }, "chan-1");
    expect(l.profile).toBe("agy");
    const onDisk = JSON.parse(await readFile(SETTINGS_FILE, "utf-8"));
    expect(onDisk.runtime.launchers["chan-1"].profile).toBe("agy");
    expect(onDisk.runtime.launcher.profile).toBe("codex"); // global untouched
  });

  it("reads a scope's own entry, and inherits the global when it has none", async () => {
    await updateRuntimeSettings({ kind: "ccs", profile: "agy" }, "chan-1");
    expect((await readRuntimeSettings("chan-1")).profile).toBe("agy");
    expect((await readRuntimeSettings("chan-other")).profile).toBe("codex");
    expect((await readRuntimeSettings()).profile).toBe("codex");
  });

  it("applies the no-profile guard to scoped writes too", async () => {
    await expect(updateRuntimeSettings({ kind: "ccs", profile: "" }, "chan-1")).rejects.toThrow(/profile must be selected/i);
  });

  it("changing the global default leaves scoped overrides intact", async () => {
    await updateRuntimeSettings({ kind: "ccs", profile: "agy" }, "chan-1");
    await updateRuntimeSettings({ kind: "direct" });
    const onDisk = JSON.parse(await readFile(SETTINGS_FILE, "utf-8"));
    expect(onDisk.runtime.launcher.kind).toBe("direct");
    expect(onDisk.runtime.launchers["chan-1"].profile).toBe("agy");
  });

  it("deleteRuntimeOverride removes one key only, and no-ops when absent", async () => {
    await updateRuntimeSettings({ kind: "ccs", profile: "agy" }, "chan-1");
    await updateRuntimeSettings({ kind: "ccs", profile: "openrouter" }, "chan-2");
    await deleteRuntimeOverride("chan-1");
    const onDisk = JSON.parse(await readFile(SETTINGS_FILE, "utf-8"));
    expect(onDisk.runtime.launchers["chan-1"]).toBeUndefined();
    expect(onDisk.runtime.launchers["chan-2"].profile).toBe("openrouter");
    await deleteRuntimeOverride("never-set"); // must not throw
    expect((await listRuntimeOverrides()).sort()).toEqual(["chan-2"]);
  });

  it("hasRuntimeOverride distinguishes an override from inheritance", async () => {
    await updateRuntimeSettings({ kind: "ccs", profile: "agy" }, "chan-1");
    expect(await hasRuntimeOverride("chan-1")).toBe(true);
    expect(await hasRuntimeOverride("chan-other")).toBe(false);
  });
});

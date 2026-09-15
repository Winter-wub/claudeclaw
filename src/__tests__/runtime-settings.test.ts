import { describe, it, expect, beforeEach } from "bun:test";
import { mkdir, writeFile, readFile } from "fs/promises";
import { join } from "path";
import { readRuntimeSettings, updateRuntimeSettings } from "../ui/services/settings";
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

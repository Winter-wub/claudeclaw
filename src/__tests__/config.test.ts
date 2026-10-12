import { describe, it, expect, beforeEach } from "bun:test";
import { loadSettings, getSettings, resetSettingsCache } from "../config";
import { join } from "path";
import { writeFile, mkdir } from "fs/promises";

const SETTINGS_DIR = join(process.cwd(), ".claude", "claudeclaw");
const SETTINGS_FILE = join(SETTINGS_DIR, "settings.json");

beforeEach(async () => {
  resetSettingsCache();
  await mkdir(SETTINGS_DIR, { recursive: true });
  // Write default settings with runtime config
  const raw = {
    model: "", api: "", fallback: { model: "", api: "" },
    agentic: { enabled: false, defaultMode: "implementation", modes: [] },
    timezone: "UTC", timezoneOffsetMinutes: 0,
    heartbeat: { enabled: false, interval: 15, prompt: "", excludeWindows: [], forwardToTelegram: true },
    telegram: { token: "", allowedUserIds: [], listenChats: [], receiveEnabled: true, dmIsolation: "shared" },
    discord: { token: "", allowedUserIds: [], listenChannels: [], listenGuilds: [], allowedGuilds: [], imageOutputRoots: [], streaming: false },
    slack: { botToken: "", appToken: "", allowedUserIds: [], listenChannels: [], allowBots: [], allowBotIds: [] },
    security: { level: "moderate", allowedTools: [], disallowedTools: [] },
    web: { enabled: false, host: "127.0.0.1", port: 4632 },
    stt: { baseUrl: "", model: "" },
    sessionTimeoutMs: 1800000, timeouts: { telegram: 5, heartbeat: 15, job: 30, default: 5 },
    watchdog: { maxConsecutiveTimeouts: null, maxRuntimeSeconds: null },
    session: { autoRotate: false, maxMessages: 50, maxAgeHours: 24, summaryPath: "" },
    plugins: {},
    runtime: { launcher: { kind: "direct", directFallback: true } },
  };
  await writeFile(SETTINGS_FILE, JSON.stringify(raw, null, 2));
  await loadSettings();
});

describe("parseSettings - runtime config", () => {
  it("default has kind: direct", () => {
    const settings = getSettings();
    expect(settings.runtime?.launcher.kind).toBe("direct");
    expect(settings.runtime?.launcher.directFallback).toBe(true);
  });

  it("parses runtime: { kind: 'ccs', profile: 'codex', directFallback: true }", async () => {
    const raw = {
      model: "", api: "", fallback: { model: "", api: "" },
      agentic: { enabled: false, defaultMode: "implementation", modes: [] },
      timezone: "UTC", timezoneOffsetMinutes: 0,
      heartbeat: { enabled: false, interval: 15, prompt: "", excludeWindows: [], forwardToTelegram: true },
      telegram: { token: "", allowedUserIds: [], listenChats: [], receiveEnabled: true, dmIsolation: "shared" },
      discord: { token: "", allowedUserIds: [], listenChannels: [], listenGuilds: [], allowedGuilds: [], imageOutputRoots: [], streaming: false },
      slack: { botToken: "", appToken: "", allowedUserIds: [], listenChannels: [], allowBots: [], allowBotIds: [] },
      security: { level: "moderate", allowedTools: [], disallowedTools: [] },
      web: { enabled: false, host: "127.0.0.1", port: 4632 },
      stt: { baseUrl: "", model: "" },
      sessionTimeoutMs: 1800000, timeouts: { telegram: 5, heartbeat: 15, job: 30, default: 5 },
      watchdog: { maxConsecutiveTimeouts: null, maxRuntimeSeconds: null },
      session: { autoRotate: false, maxMessages: 50, maxAgeHours: 24, summaryPath: "" },
      plugins: {},
      runtime: { launcher: { kind: "ccs", profile: "codex", directFallback: true } },
    };
    await writeFile(SETTINGS_FILE, JSON.stringify(raw, null, 2));
    resetSettingsCache();
    await loadSettings();
    const settings = getSettings();
    expect(settings.runtime?.launcher.kind).toBe("ccs");
    expect(settings.runtime?.launcher.profile).toBe("codex");
    expect(settings.runtime?.launcher.directFallback).toBe(true);
    // Restore default
    const defaultRaw = {
      model: "", api: "", fallback: { model: "", api: "" },
      agentic: { enabled: false, defaultMode: "implementation", modes: [] },
      timezone: "UTC", timezoneOffsetMinutes: 0,
      heartbeat: { enabled: false, interval: 15, prompt: "", excludeWindows: [], forwardToTelegram: true },
      telegram: { token: "", allowedUserIds: [], listenChats: [], receiveEnabled: true, dmIsolation: "shared" },
      discord: { token: "", allowedUserIds: [], listenChannels: [], listenGuilds: [], allowedGuilds: [], imageOutputRoots: [], streaming: false },
      slack: { botToken: "", appToken: "", allowedUserIds: [], listenChannels: [], allowBots: [], allowBotIds: [] },
      security: { level: "moderate", allowedTools: [], disallowedTools: [] },
      web: { enabled: false, host: "127.0.0.1", port: 4632 },
      stt: { baseUrl: "", model: "" },
      sessionTimeoutMs: 1800000, timeouts: { telegram: 5, heartbeat: 15, job: 30, default: 5 },
      watchdog: { maxConsecutiveTimeouts: null, maxRuntimeSeconds: null },
      session: { autoRotate: false, maxMessages: 50, maxAgeHours: 24, summaryPath: "" },
      plugins: {},
      runtime: { launcher: { kind: "direct", directFallback: true } },
    };
    await writeFile(SETTINGS_FILE, JSON.stringify(defaultRaw, null, 2));
    await loadSettings();
  });
});

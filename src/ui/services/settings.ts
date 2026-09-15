import { readFile, writeFile } from "fs/promises";
import { SETTINGS_FILE } from "../constants";

export async function setHeartbeatEnabled(enabled: boolean): Promise<void> {
  await updateHeartbeatSettings({ enabled });
}

export interface HeartbeatSettingsPatch {
  enabled?: boolean;
  interval?: number;
  prompt?: string;
  excludeWindows?: Array<{ days?: number[]; start: string; end: string }>;
}

export interface HeartbeatSettingsData {
  enabled: boolean;
  interval: number;
  prompt: string;
  excludeWindows: Array<{ days?: number[]; start: string; end: string }>;
}

export async function readHeartbeatSettings(): Promise<HeartbeatSettingsData> {
  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  if (!data.heartbeat || typeof data.heartbeat !== "object") data.heartbeat = {};
  return {
    enabled: Boolean(data.heartbeat.enabled),
    interval: Number(data.heartbeat.interval) || 15,
    prompt: typeof data.heartbeat.prompt === "string" ? data.heartbeat.prompt : "",
    excludeWindows: Array.isArray(data.heartbeat.excludeWindows) ? data.heartbeat.excludeWindows : [],
  };
}

export async function updateHeartbeatSettings(patch: HeartbeatSettingsPatch): Promise<HeartbeatSettingsData> {
  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  if (!data.heartbeat || typeof data.heartbeat !== "object") data.heartbeat = {};

  if (typeof patch.enabled === "boolean") {
    data.heartbeat.enabled = patch.enabled;
  }
  if (typeof patch.interval === "number" && Number.isFinite(patch.interval)) {
    const clamped = Math.max(1, Math.min(1440, Math.round(patch.interval)));
    data.heartbeat.interval = clamped;
  }
  if (typeof patch.prompt === "string") {
    data.heartbeat.prompt = patch.prompt;
  }
  if (Array.isArray(patch.excludeWindows)) {
    data.heartbeat.excludeWindows = patch.excludeWindows;
  }

  await writeFile(SETTINGS_FILE, JSON.stringify(data, null, 2) + "\n");
  return {
    enabled: Boolean(data.heartbeat.enabled),
    interval: Number(data.heartbeat.interval) || 15,
    prompt: typeof data.heartbeat.prompt === "string" ? data.heartbeat.prompt : "",
    excludeWindows: Array.isArray(data.heartbeat.excludeWindows) ? data.heartbeat.excludeWindows : [],
  };
}

// --- Runtime launcher (direct `claude` vs `ccs <profile>`) ---

export type RuntimeLauncherKind = "direct" | "ccs";

export interface RuntimeLauncherData {
  kind: RuntimeLauncherKind;
  profile: string;
  directFallback: boolean;
}

export interface RuntimeLauncherPatch {
  kind: RuntimeLauncherKind;
  profile?: string;
}

function normalizeLauncher(raw: any): RuntimeLauncherData {
  const kind: RuntimeLauncherKind = raw?.kind === "ccs" ? "ccs" : "direct";
  return {
    kind,
    profile: typeof raw?.profile === "string" ? raw.profile.trim() : "",
    directFallback: raw?.directFallback === true,
  };
}

export async function readRuntimeSettings(): Promise<RuntimeLauncherData> {
  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  return normalizeLauncher(data.runtime?.launcher);
}

export async function updateRuntimeSettings(patch: RuntimeLauncherPatch): Promise<RuntimeLauncherData> {
  const profile = typeof patch.profile === "string" ? patch.profile.trim() : "";
  // A ccs launcher with no profile makes buildClaudeCommand throw on every spawn,
  // so refuse to persist that combination rather than bricking the daemon.
  if (patch.kind === "ccs" && !profile) {
    throw new Error("A CCS profile must be selected when using the ccs launcher");
  }

  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  if (!data.runtime || typeof data.runtime !== "object") data.runtime = {};
  if (!data.runtime.launcher || typeof data.runtime.launcher !== "object") data.runtime.launcher = {};

  const launcher = data.runtime.launcher as Record<string, any>;
  launcher.kind = patch.kind;
  // Keep the last profile around when switching to direct, so toggling back doesn't lose it.
  if (profile) launcher.profile = profile;
  // directFallback is preserved as-is: it is parsed by config.ts but not yet acted on
  // by buildClaudeCommand, so this UI deliberately does not write it.
  if (typeof launcher.directFallback !== "boolean") launcher.directFallback = true;

  await writeFile(SETTINGS_FILE, JSON.stringify(data, null, 2) + "\n");
  return normalizeLauncher(launcher);
}

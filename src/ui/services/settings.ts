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

/**
 * Read a scope's effective launcher. With a `scopeKey`, returns that scope's own
 * override if it has one, otherwise the global default. Without one, the global
 * default. Note this reads from disk, unlike getRuntimeLauncher() which reads the
 * config module cache -- the two must be kept in step.
 */
export async function readRuntimeSettings(scopeKey?: string): Promise<RuntimeLauncherData> {
  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  const scoped = scopeKey ? data.runtime?.launchers?.[scopeKey] : undefined;
  return normalizeLauncher(scoped ?? data.runtime?.launcher);
}

/** True when this scope has its own entry rather than inheriting. */
export async function hasRuntimeOverride(scopeKey: string): Promise<boolean> {
  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  return Boolean(data.runtime?.launchers?.[scopeKey]);
}

/** Scope keys that currently carry an override. */
export async function listRuntimeOverrides(): Promise<string[]> {
  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  const launchers = data.runtime?.launchers;
  return launchers && typeof launchers === "object" ? Object.keys(launchers) : [];
}

export async function updateRuntimeSettings(patch: RuntimeLauncherPatch, scopeKey?: string): Promise<RuntimeLauncherData> {
  const profile = typeof patch.profile === "string" ? patch.profile.trim() : "";
  // A ccs launcher with no profile makes buildClaudeCommand throw on every spawn,
  // so refuse to persist that combination rather than bricking the daemon. Applies
  // to scoped entries too -- a bad override breaks only that scope, but silently.
  if (patch.kind === "ccs" && !profile) {
    throw new Error("A CCS profile must be selected when using the ccs launcher");
  }

  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  if (!data.runtime || typeof data.runtime !== "object") data.runtime = {};

  let target: Record<string, any>;
  if (scopeKey) {
    if (!data.runtime.launchers || typeof data.runtime.launchers !== "object") data.runtime.launchers = {};
    if (!data.runtime.launchers[scopeKey] || typeof data.runtime.launchers[scopeKey] !== "object") {
      data.runtime.launchers[scopeKey] = {};
    }
    target = data.runtime.launchers[scopeKey] as Record<string, any>;
  } else {
    if (!data.runtime.launcher || typeof data.runtime.launcher !== "object") data.runtime.launcher = {};
    target = data.runtime.launcher as Record<string, any>;
  }

  target.kind = patch.kind;
  // Keep the last profile around when switching to direct, so toggling back doesn't lose it.
  if (profile) target.profile = profile;
  // directFallback is preserved as-is: it is parsed by config.ts but not yet acted on
  // by buildClaudeCommand, so this UI deliberately does not write it.
  if (typeof target.directFallback !== "boolean") target.directFallback = true;

  await writeFile(SETTINGS_FILE, JSON.stringify(data, null, 2) + "\n");
  return normalizeLauncher(target);
}

/**
 * Drop a scope's override so it inherits again. No-op when the scope has none.
 * Deliberately NOT called on session reset -- an override is a standing preference
 * for that scope, not state belonging to whatever session happens to be live.
 */
export async function deleteRuntimeOverride(scopeKey: string): Promise<void> {
  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  if (!data.runtime?.launchers || typeof data.runtime.launchers !== "object") return;
  if (!(scopeKey in data.runtime.launchers)) return;
  delete data.runtime.launchers[scopeKey];
  if (Object.keys(data.runtime.launchers).length === 0) delete data.runtime.launchers;
  await writeFile(SETTINGS_FILE, JSON.stringify(data, null, 2) + "\n");
}

// --- Model tier ---

/** Empty string means "let Claude Code pick", which is how settings.model is already treated. */
export async function readModelSetting(): Promise<string> {
  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  return typeof data.model === "string" ? data.model : "";
}

export async function updateModelSetting(model: string): Promise<string> {
  const next = typeof model === "string" ? model.trim() : "";
  const raw = await readFile(SETTINGS_FILE, "utf-8");
  const data = JSON.parse(raw) as Record<string, any>;
  data.model = next;
  await writeFile(SETTINGS_FILE, JSON.stringify(data, null, 2) + "\n");
  return next;
}

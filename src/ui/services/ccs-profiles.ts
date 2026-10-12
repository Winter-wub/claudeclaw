import { homedir } from "os";
import { join } from "path";

const CCS_CONFIG_FILE = join(homedir(), ".ccs", "config.yaml");

export interface CcsProfileGroup {
  label: string;
  names: string[];
}

export interface CcsProfileList {
  /** False when ~/.ccs/config.yaml is missing or unparseable — the UI then falls back to free text. */
  available: boolean;
  groups: CcsProfileGroup[];
}

/**
 * Enumerate the CCS profiles installed for the current user.
 *
 * No `ccs` subcommand emits a machine-readable profile list, so we read
 * ~/.ccs/config.yaml directly. The three groups below reproduce exactly what the
 * `ccs` CLI itself prints when handed an unknown profile name.
 *
 * Never throws: a missing or malformed config yields `available: false`, so the
 * settings route can still report the currently configured launcher.
 */
export async function listCcsProfiles(): Promise<CcsProfileList> {
  try {
    const text = await Bun.file(CCS_CONFIG_FILE).text();
    const cfg = Bun.YAML.parse(text) as Record<string, any> | null;
    if (!cfg || typeof cfg !== "object") return { available: false, groups: [] };

    const keysOf = (v: unknown): string[] =>
      v && typeof v === "object" && !Array.isArray(v) ? Object.keys(v as object) : [];
    const stringsOf = (v: unknown): string[] =>
      Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];

    const groups: CcsProfileGroup[] = [
      { label: "CLIProxy", names: stringsOf(cfg.cliproxy?.providers) },
      { label: "API", names: keysOf(cfg.profiles) },
      { label: "Account", names: keysOf(cfg.accounts) },
    ].filter((g) => g.names.length > 0);

    if (groups.length === 0) return { available: false, groups: [] };
    return { available: true, groups };
  } catch {
    return { available: false, groups: [] };
  }
}

/** Flat list of every known profile name, for server-side validation. */
export async function knownCcsProfileNames(): Promise<Set<string>> {
  const { groups } = await listCcsProfiles();
  return new Set(groups.flatMap((g) => g.names));
}

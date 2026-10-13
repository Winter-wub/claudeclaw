// All data here is fictional. No real names, IDs, tokens or paths.

export type DaemonStatus = "running" | "stopped" | "crash-loop" | "unreachable";

export interface Daemon {
  id: string;
  name: string;
  kind: "local" | "remote";
  location: string; // project dir for local, host:port for remote
  status: DaemonStatus;
  version: string;
  watchdogPid?: number;
  workerPid?: number;
  startedAt?: string;
  activeRuns: number;
  note?: string;
}

export const daemons: Daemon[] = [
  {
    id: "notes-assistant",
    name: "notes-assistant",
    kind: "local",
    location: "~/projects/notes-assistant",
    status: "running",
    version: "2.0.0",
    watchdogPid: 41210,
    workerPid: 41212,
    startedAt: "2026-10-12T22:04:00Z",
    activeRuns: 1,
  },
  {
    id: "home-lab",
    name: "home-lab",
    kind: "local",
    location: "~/projects/home-lab",
    status: "stopped",
    version: "2.0.0",
    activeRuns: 0,
    note: "Stopped from the CLI at 08:41 (claudeclaw stop). Exit 0, so the app did not restart it.",
  },
  {
    id: "studio-mini",
    name: "studio-mini",
    kind: "remote",
    location: "studio-mini.tailnet.example:7443",
    status: "crash-loop",
    version: "2.0.0",
    activeRuns: 0,
    note: "The Watchdog gave up after 6 crashes in 10 minutes and exited.",
  },
  {
    id: "pi-garden",
    name: "pi-garden",
    kind: "remote",
    location: "192.0.2.15:7443",
    status: "unreachable",
    version: "1.9.4",
    activeRuns: 0,
    note: "Connection timed out after 10 s. Last contact 2 h ago.",
  },
];

export interface Job {
  name: string;
  agent?: string;
  schedule: string;
  recurring: boolean;
  enabled: boolean;
  launcher?: string; // frontmatter override; undefined = inherit
  model?: string;
  timeout?: number;
  retry?: number;
  retryDelay?: number;
  notify: "always" | "errors" | "never";
  notifyOnFinish: boolean;
  lastResult?: "ok" | "error" | "skipped" | "refused";
  lastRanAt?: string;
  prompt: string;
}

export const jobs: Job[] = [
  {
    name: "morning-brief",
    schedule: "0 7 * * 1-5",
    recurring: true,
    enabled: true,
    notify: "always",
    notifyOnFinish: true,
    lastResult: "ok",
    lastRanAt: "2026-10-13T07:00:00Z",
    prompt:
      "Summarise yesterday's notes in the inbox folder into five bullet points.\nFlag anything that looks like a deadline this week.",
  },
  {
    name: "inbox-triage",
    schedule: "*/30 9-18 * * *",
    recurring: true,
    enabled: true,
    launcher: "~/launchers/ccs-work.sh",
    notify: "errors",
    notifyOnFinish: false,
    lastResult: "ok",
    lastRanAt: "2026-10-13T09:30:00Z",
    timeout: 600,
    prompt: "Sort new files in inbox/ into the right project folders. Do not delete anything.",
  },
  {
    name: "weekly-review",
    schedule: "0 17 * * 5",
    recurring: true,
    enabled: true,
    launcher: "~/launchers/ccr-local.sh",
    notify: "always",
    notifyOnFinish: false,
    lastResult: "refused",
    lastRanAt: "2026-10-10T17:00:00Z",
    retry: 2,
    retryDelay: 300,
    prompt: "Write the weekly review from this week's notes and open questions.",
  },
  {
    name: "researcher/paper-digest",
    agent: "researcher",
    schedule: "0 8 * * 1",
    recurring: true,
    enabled: true,
    notify: "always",
    notifyOnFinish: false,
    lastResult: "ok",
    lastRanAt: "2026-10-13T08:00:00Z",
    model: "opus",
    prompt: "Find three new papers on the reading-list topics and post a short digest.",
  },
  {
    name: "release-checklist",
    schedule: "30 14 20 10 *",
    recurring: false,
    enabled: true,
    notify: "always",
    notifyOnFinish: true,
    prompt: "Walk through RELEASE.md and report which items are still open.",
  },
  {
    name: "tidy-downloads",
    schedule: "0 3 * * 0",
    recurring: true,
    enabled: false,
    notify: "never",
    notifyOnFinish: false,
    lastResult: "skipped",
    lastRanAt: "2026-09-28T03:00:00Z",
    prompt: "List files in scratch/ older than 30 days.",
  },
];

export interface Launcher {
  path: string;
  builtin?: boolean;
  status: "ok" | "refused" | "untested";
  statusDetail: string;
  usedBy: string[];
}

export const launchers: Launcher[] = [
  {
    path: "direct",
    builtin: true,
    status: "ok",
    statusDetail: "claude on PATH, Claude Code 2.4.1",
    usedBy: ["Global default", "#general", "morning-brief", "researcher (agent)"],
  },
  {
    path: "~/launchers/ccs-work.sh",
    status: "ok",
    statusDetail: "Last tested today 09:12 — passed with 1 warning",
    usedBy: ["Thread #research › rust-port-notes", "inbox-triage (job frontmatter)"],
  },
  {
    path: "~/launchers/ccr-local.sh",
    status: "refused",
    statusDetail: "Not executable (missing +x). Runs using it are refused.",
    usedBy: ["#research", "weekly-review (job frontmatter)"],
  },
];

export type ScopeKind = "global" | "channel" | "thread" | "agent" | "job";

export interface Scope {
  id: string;
  kind: ScopeKind;
  label: string;
  parent?: string;
  launcher?: string; // explicit override at this scope
}

export const scopes: Scope[] = [
  { id: "global", kind: "global", label: "Global session", launcher: "direct" },
  { id: "ch-general", kind: "channel", label: "#general" },
  { id: "ch-research", kind: "channel", label: "#research", launcher: "~/launchers/ccr-local.sh" },
  { id: "ch-ops", kind: "channel", label: "#ops-alerts" },
  { id: "th-digest", kind: "thread", label: "paper-digest", parent: "ch-research" },
  { id: "th-rust", kind: "thread", label: "rust-port-notes", parent: "ch-research", launcher: "~/launchers/ccs-work.sh" },
  { id: "th-garden", kind: "thread", label: "garden-sensors", parent: "ch-ops" },
  { id: "ag-researcher", kind: "agent", label: "researcher" },
  { id: "ag-scribe", kind: "agent", label: "scribe" },
  { id: "job-inbox", kind: "job", label: "inbox-triage", launcher: "~/launchers/ccs-work.sh" },
];

/** Resolve thread → parent channel → global, returning the launcher and where it came from. */
export function resolveLauncher(scopeId: string): { launcher: string; from: string } {
  let s = scopes.find((x) => x.id === scopeId);
  while (s) {
    if (s.launcher) return { launcher: s.launcher, from: s.id === scopeId ? "set here" : `inherited from ${s.label}` };
    s = s.parent ? scopes.find((x) => x.id === s!.parent) : s.kind === "global" ? undefined : scopes[0];
  }
  return { launcher: "direct", from: "default" };
}

export function launcherStatus(path: string) {
  return launchers.find((l) => l.path === path)?.status ?? "untested";
}

export interface Session {
  id: string;
  scopeId: string;
  launcher: string;
  turns: number;
  createdAt: string;
  lastUsedAt: string;
  firstMessage: string;
  active: boolean;
}

export const sessions: Session[] = [
  { id: "3f2a9c1e", scopeId: "global", launcher: "direct", turns: 42, createdAt: "2026-10-11T06:10:00Z", lastUsedAt: "2026-10-13T09:58:00Z", firstMessage: "Good morning. What's on the list today?", active: true },
  { id: "8b71d0a4", scopeId: "ch-research", launcher: "direct", turns: 17, createdAt: "2026-10-02T10:00:00Z", lastUsedAt: "2026-10-09T15:20:00Z", firstMessage: "Can you compare these two caching approaches?", active: false },
  { id: "c04e55b2", scopeId: "th-rust", launcher: "~/launchers/ccs-work.sh", turns: 9, createdAt: "2026-10-12T13:00:00Z", lastUsedAt: "2026-10-13T08:45:00Z", firstMessage: "Start a notes file for the port.", active: true },
  { id: "e9d3a771", scopeId: "th-rust", launcher: "direct", turns: 23, createdAt: "2026-10-05T09:00:00Z", lastUsedAt: "2026-10-12T12:50:00Z", firstMessage: "Outline the module layout.", active: false },
  { id: "51aa0f3c", scopeId: "ag-researcher", launcher: "direct", turns: 6, createdAt: "2026-10-06T08:00:00Z", lastUsedAt: "2026-10-13T08:04:00Z", firstMessage: "Weekly paper digest", active: true },
  { id: "77c2be19", scopeId: "job-inbox", launcher: "~/launchers/ccs-work.sh", turns: 112, createdAt: "2026-09-30T09:00:00Z", lastUsedAt: "2026-10-13T09:30:00Z", firstMessage: "Sort new files in inbox/…", active: true },
  { id: "a0b9e6d2", scopeId: "th-garden", launcher: "direct", turns: 4, createdAt: "2026-10-08T18:00:00Z", lastUsedAt: "2026-10-08T18:30:00Z", firstMessage: "Why did the soil sensor go quiet?", active: true },
];

export interface ChatMessage {
  role: "user" | "assistant" | "system";
  text: string;
  tools?: string[];
  attachments?: string[];
  error?: boolean;
}

export const chatHistory: Record<string, ChatMessage[]> = {
  global: [
    { role: "user", text: "What's on the schedule for the rest of today?" },
    {
      role: "assistant",
      text: "Three things are still due today:\n• inbox-triage runs every 30 minutes until 18:30\n• the heartbeat checks in every 15 minutes\n• nothing else is scheduled until morning-brief tomorrow at 07:00",
      tools: ["Read", "Bash"],
    },
    { role: "user", text: "Draft a reply to the note in inbox/meeting.md", attachments: ["meeting.md"] },
    { role: "assistant", text: "Here's a short draft. I kept it under 100 words and left the date as a placeholder.", tools: ["Read", "Write"] },
  ],
  "th-rust": [
    { role: "user", text: "Add today's decisions to the notes file." },
    { role: "assistant", text: "Added four bullets under 2026-10-13 in notes/port.md.", tools: ["Edit"] },
  ],
};

export const agents = [
  { name: "researcher", jobs: 1, session: "51aa0f3c", launcher: "direct (inherited)", threads: 1, description: "Finds and summarises papers on the reading-list topics." },
  { name: "scribe", jobs: 0, session: "—", launcher: "direct (inherited)", threads: 0, description: "Keeps meeting notes tidy and writes follow-ups." },
  { name: "ops-watch", jobs: 2, session: "—", launcher: "~/launchers/ccs-work.sh", threads: 1, description: "Watches the home-lab alerts channel." },
];

export const discordChannels = [
  {
    id: "ch-general",
    name: "general",
    trigger: "mention" as const,
    allowedUsers: "global list (2)",
    launcher: "direct (inherited)",
    threads: [] as { id: string; name: string; agent?: string; lastActive: string }[],
  },
  {
    id: "ch-research",
    name: "research",
    trigger: "every message" as const,
    allowedUsers: "channel list (1)",
    launcher: "~/launchers/ccr-local.sh",
    threads: [
      { id: "th-digest", name: "paper-digest", agent: "paper-digest", lastActive: "2026-10-13T08:04:00Z" },
      { id: "th-rust", name: "rust-port-notes", agent: "rust-port-notes", lastActive: "2026-10-13T08:45:00Z" },
    ],
  },
  {
    id: "ch-ops",
    name: "ops-alerts",
    trigger: "every message" as const,
    allowedUsers: "global list (2)",
    launcher: "direct (inherited)",
    threads: [{ id: "th-garden", name: "garden-sensors", agent: "garden-sensors", lastActive: "2026-10-08T18:30:00Z" }],
  },
];

export const daemonLog = [
  { t: "09:58:12", level: "info", src: "worker", msg: "chat global: run finished in 8.4 s (session 3f2a9c1e, resumed)" },
  { t: "09:45:00", level: "info", src: "scheduler", msg: "heartbeat: fired" },
  { t: "09:30:41", level: "info", src: "worker", msg: "job inbox-triage: ok in 41 s" },
  { t: "09:30:03", level: "warn", src: "worker", msg: "job inbox-triage: skipped 2 non-JSON stdout lines from launcher (kept below)" },
  { t: "09:30:00", level: "info", src: "scheduler", msg: "job inbox-triage: fired" },
  { t: "09:12:30", level: "info", src: "api", msg: "Test launcher ~/launchers/ccs-work.sh: passed with 1 warning" },
  { t: "08:45:19", level: "info", src: "discord", msg: "thread rust-port-notes: reply sent (2 messages, 4 reactions)" },
  { t: "08:04:44", level: "info", src: "worker", msg: "job researcher/paper-digest: ok in 4 m 40 s" },
  { t: "07:00:52", level: "info", src: "worker", msg: "job morning-brief: ok in 52 s" },
  { t: "06:30:00", level: "error", src: "discord", msg: "#research: run refused — launcher for this channel is not executable" },
  { t: "06:12:08", level: "warn", src: "discord", msg: "gateway reconnect (resumed after 3 s)" },
  { t: "22:04:00", level: "info", src: "watchdog", msg: "worker started (pid 41212), lock held on project dir" },
];

export const runLogs = [
  { file: "inbox-triage-2026-10-13T09-30-00-000Z.log", name: "inbox-triage", result: "ok", duration: "41 s" },
  { file: "chat-2026-10-13T09-58-03-512Z.log", name: "chat", result: "ok", duration: "8 s" },
  { file: "researcher/paper-digest-2026-10-13T08-00-00-000Z.log", name: "researcher/paper-digest", result: "ok", duration: "4 m 40 s" },
  { file: "morning-brief-2026-10-13T07-00-00-000Z.log", name: "morning-brief", result: "ok", duration: "52 s" },
  { file: "weekly-review-2026-10-10T17-00-00-000Z.log", name: "weekly-review", result: "refused", duration: "0 s" },
];

export const auditLog = [
  { t: "2026-10-13 09:12", actor: "Device: this laptop", action: "launchers.test", target: "~/launchers/ccs-work.sh" },
  { t: "2026-10-12 21:40", actor: "Device: this laptop", action: "jobs.update", target: "inbox-triage (schedule)" },
  { t: "2026-10-12 18:02", actor: "API token: home-automation", action: "inject", target: "global session" },
  { t: "2026-10-11 10:15", actor: "Device: office desktop", action: "scopes.setLauncher", target: "#research" },
  { t: "2026-10-10 08:00", actor: "CLI (local socket)", action: "settings.discord.token", target: "replaced (value not logged)" },
];

export const usageDays = [3.1, 2.4, 4.8, 1.2, 0.9, 5.6, 3.3, 2.7, 4.1, 3.9, 1.8, 0.6, 6.2, 2.2].map((usd, i) => {
  const d = new Date(Date.UTC(2026, 8, 30 + i));
  return { day: d.toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" }), usd };
});

export const usageRows = [
  { label: "inbox-triage", scope: "job", launcher: "~/launchers/ccs-work.sh", input: 1_820_000, output: 212_000, cacheHit: 91, usd: 14.2, turns: 112 },
  { label: "Global session", scope: "global", launcher: "direct", input: 640_000, output: 98_000, cacheHit: 84, usd: 9.7, turns: 42 },
  { label: "researcher", scope: "agent", launcher: "direct", input: 410_000, output: 61_000, cacheHit: 77, usd: 8.1, turns: 6 },
  { label: "#research › rust-port-notes", scope: "thread", launcher: "~/launchers/ccs-work.sh", input: 220_000, output: 33_000, cacheHit: 88, usd: 3.4, turns: 9 },
  { label: "heartbeat", scope: "global", launcher: "direct", input: 150_000, output: 12_000, cacheHit: 95, usd: 1.1, turns: 96 },
];

export const crashes = [
  { t: "10:02:11", code: 101, msg: "panicked at 'failed to open settings.json: unexpected end of JSON'" },
  { t: "10:02:13", code: 101, msg: "panicked at 'failed to open settings.json: unexpected end of JSON'" },
  { t: "10:02:17", code: 101, msg: "panicked at 'failed to open settings.json: unexpected end of JSON'" },
  { t: "10:02:25", code: 101, msg: "panicked at 'failed to open settings.json: unexpected end of JSON'" },
  { t: "10:02:41", code: 101, msg: "panicked at 'failed to open settings.json: unexpected end of JSON'" },
  { t: "10:03:13", code: 101, msg: "panicked at 'failed to open settings.json: unexpected end of JSON'" },
];

export const devices = [
  { name: "this laptop", paired: "2026-10-01", lastSeen: "now", current: true },
  { name: "office desktop", paired: "2026-10-04", lastSeen: "yesterday", current: false },
];

export const apiTokens = [{ name: "home-automation", scopes: ["inject"], created: "2026-10-01 (imported from apiToken)", lastUsed: "2026-10-12" }];

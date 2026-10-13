import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  Bell,
  Bot,
  ChartColumn,
  ChevronDown,
  CircleAlert,
  FlaskConical,
  Hash,
  History,
  Laptop,
  MessageSquare,
  Plug,
  Plus,
  ScrollText,
  Server,
  Settings,
  TriangleAlert,
  X,
} from "lucide-react";
import { daemons as initialDaemons, type Daemon } from "./mock";
import { Ctx, Dot, cx, statusMeta, useApp, type AppCtx, type Notice, type Screen } from "./ui";
import { DaemonBanner } from "./screens/Banners";
import { StatusScreen } from "./screens/Status";
import { JobEditor } from "./screens/JobEditor";
import { ChatScreen } from "./screens/Chat";
import { SessionsScreen } from "./screens/Sessions";
import { LaunchersScreen } from "./screens/Launchers";
import { SettingsScreen } from "./screens/Settings";
import { LogsScreen } from "./screens/Logs";
import { UsageScreen } from "./screens/Usage";
import { DiscordScreen } from "./screens/Discord";
import { AgentsScreen } from "./screens/Agents";
import { Offline } from "./screens/Banners";

const NAV: { group: string; items: { id: Screen; label: string; icon: typeof Activity }[] }[] = [
  {
    group: "Run",
    items: [
      { id: "status", label: "Status & Jobs", icon: Activity },
      { id: "chat", label: "Chat", icon: MessageSquare },
      { id: "sessions", label: "Sessions", icon: History },
    ],
  },
  {
    group: "Connect",
    items: [
      { id: "discord", label: "Discord", icon: Hash },
      { id: "agents", label: "Agents", icon: Bot },
    ],
  },
  {
    group: "Observe",
    items: [
      { id: "logs", label: "Logs", icon: ScrollText },
      { id: "usage", label: "Usage & cost", icon: ChartColumn },
    ],
  },
  {
    group: "Configure",
    items: [
      { id: "launchers", label: "Launchers", icon: Plug },
      { id: "settings", label: "Settings", icon: Settings },
    ],
  },
];

function parseHash() {
  const [path, query = ""] = window.location.hash.replace(/^#\/?/, "").split("?");
  const [daemonId, screen, ...rest] = path.split("/");
  return {
    daemonId: daemonId || "notes-assistant",
    screen: (screen || "status") as Screen,
    param: rest.length ? decodeURIComponent(rest.join("/")) : undefined,
    flags: new URLSearchParams(query),
  };
}

const SAMPLE_NOTICES: Record<string, Omit<Notice, "id">> = {
  refused: {
    kind: "error",
    title: "notes-assistant · Job refused",
    body: "weekly-review did not run: its launcher is not executable. Nothing fell back to another launcher.",
    action: { label: "Open Launchers", screen: "launchers", daemonId: "notes-assistant" },
  },
  crash: {
    kind: "error",
    title: "studio-mini · Crash loop",
    body: "The Watchdog gave up after 6 crashes in 10 minutes. The daemon is not running.",
    action: { label: "View details", screen: "status", daemonId: "studio-mini" },
  },
  stopped: {
    kind: "warn",
    title: "home-lab · Daemon restarted",
    body: "Both processes vanished without a stop request. The app started the daemon again.",
    action: { label: "Open logs", screen: "logs", daemonId: "home-lab" },
  },
  finished: {
    kind: "ok",
    title: "notes-assistant · morning-brief finished",
    body: "Ran in 52 s. You get this because 'Notify when finished' is on for this job.",
    action: { label: "Open run log", screen: "logs", daemonId: "notes-assistant" },
  },
  update: {
    kind: "info",
    title: "ClaudeClaw 2.0.1 is available",
    body: "Signature verified. Daemons restart in place after the update; running jobs get 30 s to finish.",
    action: { label: "Review update", screen: "settings", daemonId: "notes-assistant", param: "updates" },
  },
};

export function App() {
  const [route, setRoute] = useState(parseHash);
  const [daemons, setDaemons] = useState<Daemon[]>(initialDaemons);
  const [notices, setNotices] = useState<Notice[]>([]);
  const [trayOpen, setTrayOpen] = useState(route.flags.has("tray"));
  const [switcherOpen, setSwitcherOpen] = useState(route.flags.has("switcher"));
  const [protoOpen, setProtoOpen] = useState(false);
  const [windowHidden, setWindowHidden] = useState(false);

  useEffect(() => {
    const onHash = () => setRoute(parseHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  const notify = (n: Omit<Notice, "id">) => setNotices((xs) => [{ ...n, id: Date.now() + Math.random() }, ...xs].slice(0, 4));

  useEffect(() => {
    const n = route.flags.get("notify");
    if (n) n.split(",").forEach((k) => SAMPLE_NOTICES[k] && notify(SAMPLE_NOTICES[k]));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const daemon = daemons.find((d) => d.id === route.daemonId) ?? daemons[0];
  const ctx: AppCtx = useMemo(
    () => ({
      daemons,
      daemon,
      screen: route.screen,
      param: route.param,
      go: (screen, param, daemonId) => {
        window.location.hash = `#/${daemonId ?? daemon.id}/${screen}${param ? "/" + encodeURIComponent(param) : ""}`;
        setWindowHidden(false);
      },
      setStatus: (id, s) =>
        setDaemons((ds) => ds.map((d) => (d.id === id ? { ...d, status: s, note: s === "running" ? undefined : d.note, attached: d.kind === "local" ? true : d.attached } : d))),
      update: (id, patch) => setDaemons((ds) => ds.map((d) => (d.id === id ? { ...d, ...patch } : d))),
      notify,
    }),
    [daemons, daemon, route],
  );

  const worst = daemons.some((d) => d.status === "crash-loop")
    ? "crash-loop"
    : daemons.some((d) => d.status === "unreachable" || d.status === "stopped")
      ? "unreachable"
      : "running";
  const problems = daemons.filter((d) => d.status !== "running").length;

  return (
    <Ctx.Provider value={ctx}>
      <div className="flex h-full flex-col bg-[radial-gradient(ellipse_at_top,_#27272a,_#09090b_60%)]">
        {/* Fake OS menu bar so the tray icon has somewhere to live */}
        <div className="relative z-50 flex h-7 shrink-0 items-center justify-end gap-4 bg-black/60 px-3 text-[12px] text-zinc-300 backdrop-blur">
          <span className="mr-auto text-zinc-500">OS menu bar (simulated)</span>
          <button
            onClick={() => setTrayOpen((v) => !v)}
            className={cx("relative flex items-center gap-1 rounded px-1.5 py-0.5", trayOpen && "bg-white/15")}
            title="ClaudeClaw tray icon"
          >
            <ClawGlyph className="h-3.5 w-3.5" />
            <span className={cx("absolute -right-0.5 -top-0.5 h-2 w-2 rounded-full ring-2 ring-black", statusMeta[worst].dot)} />
          </button>
          <span>Tue 13 Oct 10:05</span>
          {trayOpen && <TrayMenu onClose={() => setTrayOpen(false)} onShow={() => setWindowHidden(false)} />}
        </div>

        <div className="relative flex min-h-0 flex-1 items-stretch justify-center p-4">
          {windowHidden ? (
            <div className="m-auto max-w-sm text-center text-zinc-400">
              <p className="text-zinc-200">The window is closed, but the app is still running in the tray.</p>
              <p className="mt-2">It keeps watching your daemons and will notify you. Click the tray icon to reopen it, or choose Quit there (daemons keep running).</p>
            </div>
          ) : (
            <div className="flex w-full max-w-[1400px] flex-col overflow-hidden rounded-xl border border-zinc-800 bg-zinc-950 shadow-2xl">
              {/* Window title bar */}
              <div className="flex h-9 shrink-0 items-center gap-2 border-b border-zinc-800 bg-zinc-900 px-3">
                <button onClick={() => setWindowHidden(true)} title="Close (hides to tray)" className="h-3 w-3 rounded-full bg-red-500/80" />
                <span className="h-3 w-3 rounded-full bg-amber-500/80" />
                <span className="h-3 w-3 rounded-full bg-emerald-500/80" />
                <span className="ml-3 text-xs text-zinc-500">ClaudeClaw</span>
                {problems > 0 && (
                  <span className="ml-auto flex items-center gap-1 text-xs text-zinc-400">
                    <TriangleAlert className="h-3.5 w-3.5 text-amber-400" />
                    {problems} of {daemons.length} daemons need attention
                  </span>
                )}
              </div>

              <div className="flex min-h-0 flex-1">
                {/* Sidebar */}
                <aside className="flex w-56 shrink-0 flex-col border-r border-zinc-800 bg-zinc-900/40">
                  <DaemonSwitcher open={switcherOpen} setOpen={setSwitcherOpen} />
                  <nav className="flex-1 overflow-y-auto px-2 pb-3">
                    {NAV.map((g) => (
                      <div key={g.group} className="mt-3">
                        <div className="px-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">{g.group}</div>
                        {g.items.map((it) => {
                          const active = route.screen === it.id || (it.id === "status" && route.screen === "job");
                          const Icon = it.icon;
                          return (
                            <button
                              key={it.id}
                              onClick={() => ctx.go(it.id)}
                              className={cx(
                                "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left",
                                active ? "bg-zinc-800 text-zinc-50" : "text-zinc-400 hover:bg-zinc-800/60 hover:text-zinc-200",
                              )}
                            >
                              <Icon className="h-4 w-4" />
                              {it.label}
                              {it.id === "launchers" && daemon.id === "notes-assistant" && (
                                <CircleAlert className="ml-auto h-3.5 w-3.5 text-red-400" />
                              )}
                            </button>
                          );
                        })}
                      </div>
                    ))}
                  </nav>
                  <button
                    onClick={() => setProtoOpen((v) => !v)}
                    className="m-2 flex items-center gap-2 rounded-md border border-dashed border-violet-500/40 px-2 py-1.5 text-xs text-violet-300 hover:bg-violet-500/10"
                  >
                    <FlaskConical className="h-3.5 w-3.5" /> Prototype: simulate events
                  </button>
                </aside>

                {/* Main */}
                <main className="flex min-w-0 flex-1 flex-col">
                  <DaemonBanner />
                  <div className="min-h-0 flex-1 overflow-y-auto p-6">
                    <ScreenView />
                  </div>
                </main>
              </div>
            </div>
          )}

          {protoOpen && <ProtoPanel onClose={() => setProtoOpen(false)} notifyKey={(k) => notify(SAMPLE_NOTICES[k])} />}
          <Notifications notices={notices} dismiss={(id) => setNotices((xs) => xs.filter((n) => n.id !== id))} />
        </div>
      </div>
    </Ctx.Provider>
  );
}

function ScreenView() {
  const { daemon, screen } = useApp();
  const offline = daemon.status !== "running";
  if (screen === "status") return <StatusScreen />;
  if (offline && screen !== "settings") return <Offline />;
  switch (screen) {
    case "job":
      return <JobEditor />;
    case "chat":
      return <ChatScreen />;
    case "sessions":
      return <SessionsScreen />;
    case "launchers":
      return <LaunchersScreen />;
    case "settings":
      return <SettingsScreen />;
    case "logs":
      return <LogsScreen />;
    case "usage":
      return <UsageScreen />;
    case "discord":
      return <DiscordScreen />;
    case "agents":
      return <AgentsScreen />;
  }
  return null;
}

function DaemonSwitcher({ open, setOpen }: { open: boolean; setOpen: (v: boolean) => void }) {
  const { daemons, daemon, go, screen, update } = useApp();
  const groups: ["local" | "remote", string][] = [
    ["local", "On this computer"],
    ["remote", "Paired remote daemons"],
  ];
  return (
    <div className="relative border-b border-zinc-800 p-2">
      <button onClick={() => setOpen(!open)} className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-left hover:bg-zinc-800">
        <span className="flex h-7 w-7 items-center justify-center rounded-md bg-zinc-800">
          {daemon.kind === "local" ? <Laptop className="h-4 w-4" /> : <Server className="h-4 w-4" />}
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-1.5 font-medium text-zinc-100">
            <Dot status={daemon.status} /> <span className="truncate">{daemon.name}</span>
          </span>
          <span className="block truncate text-[11px] text-zinc-500">{daemon.location}</span>
        </span>
        <ChevronDown className="h-4 w-4 text-zinc-500" />
      </button>
      {open && (
        <div className="absolute left-2 right-[-180px] top-full z-30 mt-1 rounded-lg border border-zinc-700 bg-zinc-900 p-1 shadow-2xl">
          {groups.map(([kind, title]) => (
            <div key={kind} className="py-1">
              <div className="px-2 py-1 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">{title}</div>
              {daemons
                .filter((d) => d.kind === kind)
                .map((d) => (
                  <button
                    key={d.id}
                    onClick={() => {
                      go(screen === "job" ? "status" : screen, undefined, d.id);
                      setOpen(false);
                    }}
                    className={cx("flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-zinc-800", d.id === daemon.id && "bg-zinc-800")}
                  >
                    {d.kind === "local" ? <Laptop className="h-4 w-4 text-zinc-500" /> : <Server className="h-4 w-4 text-zinc-500" />}
                    <span className="min-w-0 flex-1">
                      <span className="block text-zinc-100">{d.name}</span>
                      <span className="block truncate text-[11px] text-zinc-500">{d.location}</span>
                    </span>
                    <span className="flex flex-col items-end gap-0.5 text-[11px] text-zinc-400">
                      <span className="flex items-center gap-1.5">
                        <Dot status={d.status} /> {statusMeta[d.status].label}
                        {d.kind === "local" && d.status === "running" && !d.attached && <span className="text-sky-400">· not watched</span>}
                      </span>
                      {d.kind === "local" && (
                        <span
                          onClick={(e) => {
                            e.stopPropagation();
                            update(d.id, { startAtLogin: !d.startAtLogin });
                          }}
                          className={cx("rounded px-1 text-[10px]", d.startAtLogin ? "bg-amber-500/15 text-amber-300" : "text-zinc-500 hover:text-zinc-300")}
                          title="Start this daemon when the app opens at login"
                        >
                          {d.startAtLogin ? "starts at login" : "manual start"}
                        </span>
                      )}
                    </span>
                  </button>
                ))}
            </div>
          ))}
          <div className="mt-1 border-t border-zinc-800 pt-1">
            <button className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-zinc-300 hover:bg-zinc-800">
              <Plus className="h-4 w-4" /> Add a local project folder… <span className="ml-auto text-[11px] text-zinc-500">attaches if already running</span>
            </button>
            <button className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-zinc-300 hover:bg-zinc-800">
              <Plus className="h-4 w-4" /> Pair a remote daemon… <span className="ml-auto text-[11px] text-zinc-500">host + one-time code</span>
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function TrayMenu({ onClose, onShow }: { onClose: () => void; onShow: () => void }) {
  const { daemons, go, setStatus, update } = useApp();
  return (
    <div className="absolute right-24 top-7 z-50 w-80 rounded-lg border border-zinc-700 bg-zinc-900/95 p-1 text-[13px] shadow-2xl backdrop-blur">
      <div className="px-2 py-1.5 text-[11px] text-zinc-500">ClaudeClaw — {daemons.filter((d) => d.status === "running").length} of {daemons.length} daemons running</div>
      {daemons.map((d) => (
        <div key={d.id} className="group flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-zinc-800">
          <Dot status={d.status} />
          <button
            className="flex-1 text-left text-zinc-100"
            onClick={() => {
              go("status", undefined, d.id);
              onShow();
              onClose();
            }}
          >
            {d.name}
            <span className="ml-2 text-[11px] text-zinc-500">{statusMeta[d.status].label}</span>
          </button>
          {d.kind === "local" && d.status === "running" && !d.attached && (
            <button className="text-[11px] text-sky-400 hover:text-sky-300" onClick={() => update(d.id, { attached: true })}>
              Attach
            </button>
          )}
          {d.kind === "local" && d.status === "running" && (
            <button className="text-[11px] text-zinc-400 hover:text-zinc-100" onClick={() => setStatus(d.id, "stopped")}>
              Stop
            </button>
          )}
          {d.kind === "local" && d.status !== "running" && (
            <button className="text-[11px] text-amber-400 hover:text-amber-300" onClick={() => setStatus(d.id, "running")}>
              Start
            </button>
          )}
        </div>
      ))}
      <div className="my-1 border-t border-zinc-800" />
      <TrayItem onClick={() => { onShow(); onClose(); }}>Open ClaudeClaw</TrayItem>
      <TrayItem onClick={() => { go("settings", "updates", "notes-assistant"); onShow(); onClose(); }}>
        Update available: 2.0.1…
      </TrayItem>
      <TrayItem>
        <span className="flex items-center gap-2">
          <Bell className="h-3.5 w-3.5" /> Pause notifications for 1 hour
        </span>
      </TrayItem>
      <div className="my-1 border-t border-zinc-800" />
      <TrayItem>
        Quit ClaudeClaw <span className="ml-auto text-[11px] text-zinc-500">daemons keep running</span>
      </TrayItem>
    </div>
  );
}

function TrayItem({ children, onClick }: { children: React.ReactNode; onClick?: () => void }) {
  return (
    <button onClick={onClick} className="flex w-full items-center rounded-md px-2 py-1.5 text-left text-zinc-200 hover:bg-zinc-800">
      {children}
    </button>
  );
}

function Notifications({ notices, dismiss }: { notices: Notice[]; dismiss: (id: number) => void }) {
  const { go } = useApp();
  return (
    <div className="pointer-events-none absolute right-4 top-2 z-40 flex w-80 flex-col gap-2">
      {notices.map((n) => (
        <div key={n.id} className="pointer-events-auto rounded-xl border border-zinc-700 bg-zinc-800/95 p-3 shadow-2xl backdrop-blur">
          <div className="flex items-start gap-2">
            <ClawGlyph className="mt-0.5 h-4 w-4 shrink-0" />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-1.5 font-semibold text-zinc-50">
                {n.kind === "error" && <CircleAlert className="h-3.5 w-3.5 text-red-400" />}
                {n.kind === "warn" && <TriangleAlert className="h-3.5 w-3.5 text-amber-400" />}
                {n.title}
              </div>
              <p className="mt-0.5 text-xs text-zinc-300">{n.body}</p>
              {n.action && (
                <button
                  onClick={() => {
                    go(n.action!.screen, n.action!.param, n.action!.daemonId);
                    dismiss(n.id);
                  }}
                  className="mt-2 text-xs font-medium text-amber-400 hover:text-amber-300"
                >
                  {n.action.label} →
                </button>
              )}
            </div>
            <button onClick={() => dismiss(n.id)} className="text-zinc-500 hover:text-zinc-200">
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
          <div className="mt-1 text-right text-[10px] text-zinc-500">OS notification (simulated)</div>
        </div>
      ))}
    </div>
  );
}

function ProtoPanel({ onClose, notifyKey }: { onClose: () => void; notifyKey: (k: string) => void }) {
  const items: [string, string][] = [
    ["refused", "Job failed: refused launcher"],
    ["crash", "Daemon crash-looping"],
    ["stopped", "Daemon vanished and was restarted"],
    ["finished", "Job finished (opt-in per job)"],
    ["update", "New version available"],
  ];
  return (
    <div className="absolute bottom-6 left-6 z-40 w-72 rounded-lg border border-violet-500/40 bg-zinc-900 p-3 shadow-2xl">
      <div className="mb-2 flex items-center justify-between text-xs font-semibold text-violet-300">
        Prototype controls (not part of the app)
        <button onClick={onClose}>
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
      <p className="mb-2 text-[11px] text-zinc-400">Fire the four OS notifications decided in #10. Switch daemons in the sidebar to see the error states.</p>
      <div className="flex flex-col gap-1">
        {items.map(([k, label]) => (
          <button key={k} onClick={() => notifyKey(k)} className="rounded-md px-2 py-1 text-left text-zinc-200 hover:bg-zinc-800">
            {label}
          </button>
        ))}
      </div>
    </div>
  );
}

export function ClawGlyph({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" className={className} fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round">
      <path d="M5 4c2 4 2 9 0 16" />
      <path d="M10 3c2 5 2 13 0 18" />
      <path d="M15 4c2 4 2 9 0 16" />
      <path d="M19 7c1 3 1 7 0 10" />
    </svg>
  );
}

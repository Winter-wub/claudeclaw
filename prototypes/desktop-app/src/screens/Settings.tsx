import { Copy, Download, KeyRound, Plus, ShieldCheck, Trash2 } from "lucide-react";
import { useState } from "react";
import { apiTokens, devices } from "../mock";
import { Badge, Button, Card, Field, PageHeader, Toggle, cx, inputCls, useApp } from "../ui";
import { Offline } from "./Banners";

const DAEMON_SECTIONS = [
  ["general", "General"],
  ["heartbeat", "Heartbeat"],
  ["discord", "Discord"],
  ["sessions", "Sessions & timeouts"],
  ["security", "Security"],
  ["remote", "Remote access & devices"],
  ["tokens", "API tokens"],
] as const;
const APP_SECTIONS = [
  ["notifications", "Notifications"],
  ["startup", "Startup & window"],
  ["language", "Language"],
  ["updates", "Updates"],
] as const;

export function SettingsScreen() {
  const { param, daemon, go } = useApp();
  const section = param ?? "general";
  const isDaemon = DAEMON_SECTIONS.some(([id]) => id === section);
  const Nav = ({ items, title }: { items: readonly (readonly [string, string])[]; title: string }) => (
    <div className="mb-4">
      <div className="px-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">{title}</div>
      {items.map(([id, label]) => (
        <button
          key={id}
          onClick={() => go("settings", id)}
          className={cx("block w-full rounded-md px-2 py-1.5 text-left", section === id ? "bg-zinc-800 text-zinc-50" : "text-zinc-400 hover:text-zinc-200")}
        >
          {label}
        </button>
      ))}
    </div>
  );
  return (
    <>
      <PageHeader title="Settings" subtitle="Daemon settings are saved through the Management API and apply within 30 s. App settings live on this computer." />
      <div className="grid grid-cols-[200px_1fr] gap-6">
        <div>
          <Nav items={DAEMON_SECTIONS} title={`Daemon: ${daemon.name}`} />
          <Nav items={APP_SECTIONS} title="This app" />
        </div>
        <div className="max-w-2xl space-y-4">
          {isDaemon && daemon.status !== "running" ? <Offline /> : <Section id={section} />}
        </div>
      </div>
    </>
  );
}

function Section({ id }: { id: string }) {
  const [on, setOn] = useState<Record<string, boolean>>({ hb: true, rotate: false, remote: true, login: true, tray: true, nDown: true, nFail: true, nUpdate: true, streaming: false });
  const t = (k: string) => ({ on: on[k], onChange: (v: boolean) => setOn((o) => ({ ...o, [k]: v })) });
  switch (id) {
    case "general":
      return (
        <Card title="General">
          <div className="grid grid-cols-2 gap-4">
            <Field label="Model" hint="Empty lets Claude Code pick.">
              <input className={inputCls} placeholder="default" />
            </Field>
            <Field label="Timezone" hint="Used for cron and quiet hours.">
              <select className={inputCls}>
                <option>UTC</option>
                <option>Europe/Berlin</option>
                <option>America/New_York</option>
              </select>
            </Field>
            <Field label="Fallback model" hint="Used when the primary is rate-limited.">
              <input className={inputCls} placeholder="none" />
            </Field>
            <Field label="Agentic routing">
              <Toggle on={false} label="Route planning vs implementation to different models" />
            </Field>
          </div>
        </Card>
      );
    case "heartbeat":
      return (
        <Card title="Heartbeat" actions={<Toggle {...t("hb")} />}>
          <div className="grid grid-cols-2 gap-4">
            <Field label="Interval (minutes)" hint="1–1440">
              <input className={inputCls} defaultValue={15} />
            </Field>
            <Field label="Prompt" hint="Text, or a path ending .md/.txt/.prompt">
              <input className={inputCls} defaultValue="prompts/HEARTBEAT.md" />
            </Field>
          </div>
          <div className="mt-4">
            <div className="mb-1 text-xs font-medium text-zinc-400">Quiet hours</div>
            {[
              ["23:00", "07:00", "Every day"],
              ["00:00", "23:59", "Sat, Sun"],
            ].map(([a, b, d]) => (
              <div key={a + d} className="mb-1 flex items-center gap-2">
                <input className={cx(inputCls, "w-24")} defaultValue={a} />–<input className={cx(inputCls, "w-24")} defaultValue={b} />
                <span className="text-zinc-400">{d}</span>
                <Button size="sm" variant="ghost">
                  <Trash2 className="h-3.5 w-3.5" />
                </Button>
              </div>
            ))}
            <Button size="sm">
              <Plus className="h-3.5 w-3.5" /> Add window
            </Button>
          </div>
        </Card>
      );
    case "discord":
      return (
        <Card title="Discord">
          <Field label="Bot token" hint="Write-only: the daemon reports only whether a token is set.">
            <div className="flex items-center gap-2">
              <Badge tone="green">Set</Badge>
              <Button size="sm">Replace…</Button>
              <Button size="sm" variant="ghost">
                Remove
              </Button>
            </div>
          </Field>
          <div className="mt-4 grid grid-cols-2 gap-4">
            <Field label="Allowed users" hint="Global allow-list. Channel lists in the Discord screen.">
              <div className="text-zinc-300">2 users</div>
            </Field>
            <Field label="Live preview while Claude writes">
              <Toggle {...t("streaming")} />
            </Field>
          </div>
        </Card>
      );
    case "sessions":
      return (
        <Card title="Sessions & timeouts">
          <Toggle {...t("rotate")} label="Rotate the global session automatically" />
          <div className="mt-3 grid grid-cols-2 gap-4">
            <Field label="Rotate after (messages)">
              <input className={inputCls} defaultValue={50} />
            </Field>
            <Field label="Rotate after (hours)">
              <input className={inputCls} defaultValue={24} />
            </Field>
          </div>
          <div className="mt-4 grid grid-cols-4 gap-4">
            {[
              ["Chat", 5],
              ["Heartbeat", 15],
              ["Job", 30],
              ["Default", 5],
            ].map(([l, v]) => (
              <Field key={l} label={`${l} timeout (min)`}>
                <input className={inputCls} defaultValue={v} />
              </Field>
            ))}
          </div>
        </Card>
      );
    case "security":
      return (
        <Card title="Security">
          <Field label="Level" hint="Controls the tools Claude Code may use.">
            <select className={inputCls} defaultValue="moderate">
              <option value="locked">Locked</option>
              <option value="strict">Strict</option>
              <option value="moderate">Moderate</option>
              <option value="unrestricted">Unrestricted</option>
            </select>
          </Field>
          <div className="mt-4 grid grid-cols-2 gap-4">
            <Field label="Extra allowed tools">
              <input className={inputCls} placeholder="e.g. WebFetch" />
            </Field>
            <Field label="Disallowed tools">
              <input className={inputCls} placeholder="e.g. Bash(rm:*)" />
            </Field>
          </div>
        </Card>
      );
    case "remote":
      return (
        <>
          <Card title="Remote access" actions={<Toggle {...t("remote")} />}>
            <p className="text-zinc-400">
              Off by default. When on, the daemon listens with TLS on a self-signed certificate and both sides pin each other's key. Works over a LAN or a
              VPN such as Tailscale.
            </p>
            <div className="mt-3 grid grid-cols-2 gap-4">
              <Field label="Listen on">
                <input className={inputCls} defaultValue="0.0.0.0:7443" />
              </Field>
              <Field label="Daemon fingerprint">
                <code className="block py-1.5 text-zinc-300">SHA256:Zm9v…YmFy</code>
              </Field>
            </div>
            <Button className="mt-3" variant="primary">
              <KeyRound className="h-3.5 w-3.5" /> Create pairing code
            </Button>
            <div className="mt-3 rounded-md border border-dashed border-zinc-700 p-3 text-center">
              <div className="font-mono text-xl tracking-[0.3em] text-zinc-50">KQ7-M2D-41X</div>
              <div className="text-[11px] text-zinc-500">One-time, valid 9:41 more. Enter it with the host in the other app's “Pair a remote daemon”.</div>
            </div>
          </Card>
          <Card title="Paired devices">
            {devices.map((d) => (
              <div key={d.name} className="flex items-center gap-3 border-b border-zinc-800 py-2 last:border-0">
                <ShieldCheck className="h-4 w-4 text-emerald-400" />
                <span className="flex-1 text-zinc-200">
                  {d.name} {d.current && <Badge>this app</Badge>}
                </span>
                <span className="text-xs text-zinc-500">
                  paired {d.paired} · seen {d.lastSeen}
                </span>
                <Button size="sm" variant="ghost" disabled={d.current}>
                  Revoke
                </Button>
              </div>
            ))}
            <p className="mt-2 text-[11px] text-zinc-500">Every paired device is a full admin in v1.</p>
          </Card>
        </>
      );
    case "tokens":
      return (
        <Card title="API tokens" actions={<Button size="sm"><Plus className="h-3.5 w-3.5" /> New token</Button>}>
          <p className="mb-3 text-zinc-400">For scripts on other machines. Each token has limited scopes; only a hash is stored, so the value is shown once.</p>
          {apiTokens.map((tk) => (
            <div key={tk.name} className="flex items-center gap-3 py-2">
              <KeyRound className="h-4 w-4 text-zinc-400" />
              <span className="flex-1 text-zinc-200">{tk.name}</span>
              {tk.scopes.map((s) => (
                <Badge key={s} tone="blue">
                  {s}
                </Badge>
              ))}
              <span className="text-xs text-zinc-500">{tk.created}</span>
              <Button size="sm" variant="ghost">
                Revoke
              </Button>
            </div>
          ))}
        </Card>
      );
    case "notifications":
      return (
        <Card title="Desktop notifications">
          <div className="space-y-3">
            <Toggle {...t("nDown")} label="A daemon stopped unexpectedly or is crash-looping" />
            <Toggle {...t("nFail")} label="A job failed (including a refused launcher)" />
            <Toggle {...t("nUpdate")} label="A new version is available" />
            <p className="text-[11px] text-zinc-500">“Job finished” is opt-in per job in the job editor.</p>
          </div>
        </Card>
      );
    case "startup":
      return (
        <Card title="Startup & window">
          <div className="space-y-3">
            <Toggle {...t("login")} label="Open at login and start my local daemons" />
            <Toggle {...t("tray")} label="Closing the window keeps the app in the tray" />
            <p className="text-[11px] text-zinc-500">The app is the only thing that autostarts. Quitting it leaves daemons running.</p>
          </div>
        </Card>
      );
    case "language":
      return (
        <Card title="Language">
          <select className={inputCls} defaultValue="en">
            <option value="en">English</option>
            <option disabled>ไทย (Thai) — coming later</option>
          </select>
        </Card>
      );
    case "updates":
      return (
        <Card title="Updates">
          <div className="flex items-center gap-3">
            <div className="flex-1">
              <div className="text-zinc-100">ClaudeClaw 2.0.1 is available</div>
              <div className="text-xs text-zinc-500">You have 2.0.0. Signature verified with minisign.</div>
            </div>
            <Button variant="primary">
              <Download className="h-3.5 w-3.5" /> Update app and local daemons
            </Button>
          </div>
          <p className="mt-3 text-[11px] text-zinc-500">
            Each local daemon finishes running jobs (up to 30 s), then its Watchdog re-execs the new binary. Remote daemons update on their own machines.
          </p>
          <Button size="sm" variant="ghost" className="mt-2">
            <Copy className="h-3.5 w-3.5" /> Copy release notes link
          </Button>
        </Card>
      );
  }
  return null;
}

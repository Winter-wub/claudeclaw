import { CircleAlert, Copy, Pencil, Play, Plus, RotateCw, Square, TriangleAlert, WifiOff } from "lucide-react";
import { useState } from "react";
import { describeCron, fmtRun, nextRuns } from "../cron";
import { crashes, jobs, launcherStatus } from "../mock";
import { Badge, Button, Card, PageHeader, Toggle, cx, fmtAgo, statusMeta, useApp } from "../ui";

export function StatusScreen() {
  const { daemon } = useApp();
  if (daemon.status === "stopped") return <Stopped />;
  if (daemon.status === "crash-loop") return <CrashLoop />;
  if (daemon.status === "unreachable") return <Unreachable />;
  return <Running />;
}

function Controls() {
  const { daemon, setStatus } = useApp();
  return (
    <>
      <Button onClick={() => setStatus(daemon.id, "running")} disabled={daemon.status === "running"}>
        <Play className="h-3.5 w-3.5" /> Start
      </Button>
      <Button onClick={() => setStatus(daemon.id, "stopped")} disabled={daemon.status !== "running"}>
        <Square className="h-3.5 w-3.5" /> Stop
      </Button>
      <Button disabled={daemon.status !== "running"}>
        <RotateCw className="h-3.5 w-3.5" /> Restart
      </Button>
    </>
  );
}

function Stat({ label, value, sub, tone }: { label: string; value: React.ReactNode; sub?: React.ReactNode; tone?: string }) {
  return (
    <div className="rounded-lg border border-zinc-800 bg-zinc-900/60 px-4 py-3">
      <div className="text-[11px] uppercase tracking-wider text-zinc-500">{label}</div>
      <div className={cx("mt-1 text-[15px] font-semibold text-zinc-50", tone)}>{value}</div>
      {sub && <div className="mt-0.5 text-[11px] text-zinc-500">{sub}</div>}
    </div>
  );
}

function Running() {
  const { daemon, go } = useApp();
  const [heartbeatOn, setHeartbeatOn] = useState(true);
  return (
    <>
      <PageHeader
        title="Status & Jobs"
        subtitle={
          <>
            {daemon.kind === "local" ? "Local daemon" : "Remote daemon"} · {daemon.location} · v{daemon.version}
          </>
        }
        actions={<Controls />}
      />
      <div className="grid grid-cols-5 gap-3">
        <Stat label="Daemon" value={<span className="text-emerald-400">{statusMeta[daemon.status].label}</span>} sub="up 12 h 1 min" />
        <Stat label="Processes" value="Watchdog + Worker" sub={`pids ${daemon.watchdogPid} / ${daemon.workerPid}`} />
        <Stat label="Active runs" value={daemon.activeRuns} sub="inbox-triage queue: 0 waiting" />
        <Stat label="Discord" value={<span className="text-emerald-400">Connected</span>} sub="3 channels · 3 threads" />
        <Stat label="Next heartbeat" value="10:15" sub="every 15 min · quiet 23:00–07:00" />
      </div>

      <div className="mt-4 rounded-lg border border-red-500/30 bg-red-500/5 px-4 py-3">
        <div className="flex items-center gap-2 font-medium text-red-300">
          <CircleAlert className="h-4 w-4" /> 1 job was refused on its last run
        </div>
        <p className="mt-1 text-zinc-400">
          <b className="text-zinc-200">weekly-review</b> uses a launcher that is not executable. The run was refused and did not fall back to{" "}
          <code>direct</code>.
        </p>
        <div className="mt-2 flex gap-2">
          <Button size="sm" onClick={() => go("launchers")}>
            Open Launchers
          </Button>
          <Button size="sm" variant="ghost" onClick={() => go("job", "weekly-review")}>
            Edit job
          </Button>
        </div>
      </div>

      <Card
        className="mt-4"
        title="Jobs"
        actions={
          <Button size="sm" variant="primary" onClick={() => go("job", "new")}>
            <Plus className="h-3.5 w-3.5" /> New job
          </Button>
        }
      >
        <table className="w-full text-left">
          <thead className="text-[11px] uppercase tracking-wider text-zinc-500">
            <tr>
              <th className="pb-2 font-medium">Job</th>
              <th className="pb-2 font-medium">Schedule</th>
              <th className="pb-2 font-medium">Launcher</th>
              <th className="pb-2 font-medium">Next run (UTC)</th>
              <th className="pb-2 font-medium">Last run</th>
              <th className="pb-2 font-medium">On</th>
              <th />
            </tr>
          </thead>
          <tbody className="divide-y divide-zinc-800/80">
            {jobs.map((j) => {
              const next = j.enabled ? nextRuns(j.schedule, 1, new Date("2026-10-13T10:05:00Z"))[0] : undefined;
              const ls = j.launcher ? launcherStatus(j.launcher) : "ok";
              return (
                <tr key={j.name} className={cx("align-top", !j.enabled && "opacity-50")}>
                  <td className="py-2">
                    <button onClick={() => go("job", j.name)} className="font-medium text-zinc-100 hover:underline">
                      {j.name}
                    </button>
                    <div className="mt-0.5 flex gap-1">
                      {j.agent && <Badge tone="violet">agent: {j.agent}</Badge>}
                      {!j.recurring && <Badge tone="blue">one-shot</Badge>}
                    </div>
                  </td>
                  <td className="py-2">
                    <div className="text-zinc-200">{describeCron(j.schedule)}</div>
                    <code className="text-[11px] text-zinc-500">{j.schedule}</code>
                  </td>
                  <td className="py-2">
                    <span className={cx("font-mono text-[12px]", ls === "refused" ? "text-red-400" : "text-zinc-300")}>{j.launcher ?? "inherit (direct)"}</span>
                  </td>
                  <td className="py-2 text-zinc-300">{next ? fmtRun(next) : "—"}</td>
                  <td className="py-2">
                    {j.lastResult === "ok" && <Badge tone="green">ok</Badge>}
                    {j.lastResult === "error" && <Badge tone="red">error</Badge>}
                    {j.lastResult === "refused" && (
                      <Badge tone="red">
                        <CircleAlert className="h-3 w-3" /> refused: launcher
                      </Badge>
                    )}
                    {j.lastResult === "skipped" && <Badge>skipped</Badge>}
                    {!j.lastResult && <span className="text-zinc-500">never</span>}
                    <div className="mt-0.5 text-[11px] text-zinc-500">{fmtAgo(j.lastRanAt)}</div>
                  </td>
                  <td className="py-2">
                    <Toggle on={j.enabled} />
                  </td>
                  <td className="py-2 text-right whitespace-nowrap">
                    <Button size="sm" variant="ghost" title="Run now">
                      <Play className="h-3.5 w-3.5" />
                    </Button>
                    <Button size="sm" variant="ghost" onClick={() => go("job", j.name)} title="Edit">
                      <Pencil className="h-3.5 w-3.5" />
                    </Button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </Card>

      <Card className="mt-4" title="Heartbeat" actions={<Toggle on={heartbeatOn} onChange={setHeartbeatOn} label={heartbeatOn ? "On" : "Off"} />}>
        <div className="grid grid-cols-3 gap-4 text-zinc-300">
          <div>
            <div className="text-[11px] text-zinc-500">Interval</div>every 15 minutes
          </div>
          <div>
            <div className="text-[11px] text-zinc-500">Quiet hours</div>23:00–07:00 every day; Sat–Sun all day
          </div>
          <div>
            <div className="text-[11px] text-zinc-500">Prompt</div>
            <code>prompts/HEARTBEAT.md</code>
          </div>
        </div>
        <Button size="sm" className="mt-3" onClick={() => go("settings", "heartbeat")}>
          Edit heartbeat
        </Button>
      </Card>
    </>
  );
}

function Stopped() {
  const { daemon, setStatus } = useApp();
  return (
    <>
      <PageHeader title="Status & Jobs" subtitle={`${daemon.location} · v${daemon.version}`} actions={<Controls />} />
      <div className="rounded-xl border border-zinc-700 bg-zinc-900/60 p-8 text-center">
        <Square className="mx-auto h-8 w-8 text-zinc-500" />
        <h2 className="mt-3 text-base font-semibold text-zinc-100">{daemon.name} is not running</h2>
        <p className="mx-auto mt-1 max-w-lg text-zinc-400">{daemon.note}</p>
        <p className="mx-auto mt-1 max-w-lg text-zinc-500">Jobs and the heartbeat are paused. Missed runs from the last 6 h are caught up when it starts.</p>
        <div className="mt-4 flex justify-center gap-2">
          <Button variant="primary" onClick={() => setStatus(daemon.id, "running")}>
            <Play className="h-3.5 w-3.5" /> Start daemon
          </Button>
          <Button>Start at login: on</Button>
        </div>
      </div>
    </>
  );
}

function CrashLoop() {
  const { daemon } = useApp();
  return (
    <>
      <PageHeader title="Status & Jobs" subtitle={`Remote daemon · ${daemon.location} · v${daemon.version}`} />
      <div className="rounded-xl border border-red-500/40 bg-red-500/5 p-5">
        <div className="flex items-center gap-2 text-[15px] font-semibold text-red-300">
          <CircleAlert className="h-5 w-5" /> Crash loop — the Watchdog gave up
        </div>
        <p className="mt-1 text-zinc-300">
          The Worker crashed 6 times in 10 minutes. Restarts backed off from 1 s to 32 s, then the Watchdog stopped trying, sent a Discord DM to the
          owner, and exited. The app does not override that decision.
        </p>
        <div className="mt-4 grid grid-cols-2 gap-4">
          <div>
            <div className="mb-1 text-[11px] uppercase tracking-wider text-zinc-500">Crashes</div>
            <ol className="space-y-1 font-mono text-[12px]">
              {crashes.map((c, i) => (
                <li key={i} className="flex gap-3">
                  <span className="text-zinc-500">{c.t}</span>
                  <span className="text-red-400">exit {c.code}</span>
                </li>
              ))}
            </ol>
          </div>
          <div>
            <div className="mb-1 text-[11px] uppercase tracking-wider text-zinc-500">Last error</div>
            <pre className="whitespace-pre-wrap rounded-md bg-zinc-950 p-3 font-mono text-[12px] text-red-300">
              {crashes[crashes.length - 1].msg}
              {"\n"}  at settings::load (settings.rs:88){"\n"}hint: the file was cut off mid-write
            </pre>
          </div>
        </div>
        <div className="mt-4 flex flex-wrap gap-2">
          <Button variant="danger">
            <RotateCw className="h-3.5 w-3.5" /> Restart anyway
          </Button>
          <Button>
            <Copy className="h-3.5 w-3.5" /> Copy diagnostics
          </Button>
          <span className="self-center text-xs text-zinc-500">
            Restart works only if the remote Watchdog accepts a start request; otherwise run <code>claudeclaw start</code> on that machine.
          </span>
        </div>
      </div>
    </>
  );
}

function Unreachable() {
  const { daemon } = useApp();
  return (
    <>
      <PageHeader title="Status & Jobs" subtitle={`Remote daemon · ${daemon.location}`} />
      <div className="rounded-xl border border-amber-500/40 bg-amber-500/5 p-6">
        <div className="flex items-center gap-2 text-[15px] font-semibold text-amber-200">
          <WifiOff className="h-5 w-5" /> Can't reach {daemon.name}
        </div>
        <p className="mt-1 text-zinc-300">{daemon.note}</p>
        <ul className="mt-3 list-disc space-y-1 pl-5 text-zinc-400">
          <li>The app can't start a remote daemon. If it is stopped, start it on that machine.</li>
          <li>The pinned key still matches; this is a network problem, not a pairing problem.</li>
          <li>
            The daemon there reports v{daemon.version}; this app is v2.0.0. <TriangleAlert className="inline h-3.5 w-3.5 text-amber-400" /> Older daemons
            may lack some screens.
          </li>
        </ul>
        <div className="mt-4 flex gap-2">
          <Button variant="primary">
            <RotateCw className="h-3.5 w-3.5" /> Retry now
          </Button>
          <Button>Edit host…</Button>
          <Button variant="ghost">Forget this daemon</Button>
        </div>
      </div>
    </>
  );
}

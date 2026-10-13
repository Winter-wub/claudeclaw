import { Check, CircleAlert, FlaskConical, FolderOpen, Loader2, Plus, TriangleAlert, X } from "lucide-react";
import { useState } from "react";
import { launchers, resolveLauncher, scopes } from "../mock";
import { Badge, Button, Card, PageHeader, cx, useApp } from "../ui";
import { LauncherPicker } from "./JobEditor";

type CheckRow = { label: string; state: "ok" | "warn" | "fail" | "skip"; detail?: string };

const RESULTS: Record<string, CheckRow[]> = {
  direct: [
    { label: "Executable found and runnable", state: "ok", detail: "claude on PATH" },
    { label: "Run started with --print and stream-json", state: "ok" },
    { label: "Stdout is clean stream-json", state: "ok" },
    { label: "Daemon's system prompt arrived", state: "ok", detail: "probe phrase echoed back" },
    { label: "A result event was emitted", state: "ok", detail: "is_error: false · 3.1 s" },
  ],
  "~/launchers/ccs-work.sh": [
    { label: "Executable found and runnable", state: "ok" },
    { label: "Run started with --print and stream-json", state: "ok" },
    { label: "Stdout is clean stream-json", state: "warn", detail: "2 non-JSON lines skipped (kept in the log)" },
    { label: "Daemon's system prompt arrived", state: "ok", detail: "probe phrase echoed back — no ccs clobber" },
    { label: "A result event was emitted", state: "ok", detail: "is_error: false · 5.8 s" },
  ],
  "~/launchers/ccr-local.sh": [
    { label: "Executable found and runnable", state: "fail", detail: "File exists but is not executable. Run: chmod +x ~/launchers/ccr-local.sh" },
    { label: "Run started with --print and stream-json", state: "skip" },
    { label: "Stdout is clean stream-json", state: "skip" },
    { label: "Daemon's system prompt arrived", state: "skip" },
    { label: "A result event was emitted", state: "skip" },
  ],
};

function Icon({ state }: { state: CheckRow["state"] }) {
  if (state === "ok") return <Check className="h-4 w-4 text-emerald-400" />;
  if (state === "warn") return <TriangleAlert className="h-4 w-4 text-amber-400" />;
  if (state === "fail") return <X className="h-4 w-4 text-red-400" />;
  return <span className="inline-block h-4 w-4 text-center text-zinc-600">–</span>;
}

export function LaunchersScreen() {
  const [testing, setTesting] = useState<string | null>(null);
  const [tested, setTested] = useState<string | null>(null);
  const run = (p: string) => {
    setTesting(p);
    setTested(null);
    setTimeout(() => {
      setTesting(null);
      setTested(p);
    }, 900);
  };

  return (
    <>
      <PageHeader
        title="Launchers"
        subtitle="A launcher is the path to one runnable executable that the daemon calls in place of claude. A run whose launcher can't start is refused; it never falls back."
        actions={
          <Button>
            <FolderOpen className="h-3.5 w-3.5" /> Add launcher…
          </Button>
        }
      />
      <div className="space-y-3">
        {launchers.map((l) => (
          <Card key={l.path}>
            <div className="flex items-start gap-4">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <code className="text-[14px] text-zinc-50">{l.path}</code>
                  {l.builtin && <Badge>built in</Badge>}
                  {l.status === "ok" && <Badge tone="green">runnable</Badge>}
                  {l.status === "refused" && (
                    <Badge tone="red">
                      <CircleAlert className="h-3 w-3" /> refused
                    </Badge>
                  )}
                </div>
                <p className={cx("mt-1 text-xs", l.status === "refused" ? "text-red-300" : "text-zinc-400")}>{l.statusDetail}</p>
                <div className="mt-2 flex flex-wrap gap-1">
                  <span className="text-[11px] text-zinc-500">Used by:</span>
                  {l.usedBy.map((u) => (
                    <Badge key={u}>{u}</Badge>
                  ))}
                </div>
              </div>
              <Button onClick={() => run(l.path)} disabled={testing !== null}>
                {testing === l.path ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <FlaskConical className="h-3.5 w-3.5" />}
                Test launcher
              </Button>
            </div>
            {tested === l.path && (
              <div className="mt-3 rounded-md border border-zinc-800 bg-zinc-950 p-3">
                <div className="mb-2 text-xs text-zinc-400">One small run in a throwaway session. Costs a few hundred tokens.</div>
                <ul className="space-y-1.5">
                  {RESULTS[l.path].map((c) => (
                    <li key={c.label} className="flex items-start gap-2">
                      <Icon state={c.state} />
                      <span className={cx(c.state === "skip" ? "text-zinc-600" : "text-zinc-200")}>{c.label}</span>
                      {c.detail && <span className="text-xs text-zinc-500">— {c.detail}</span>}
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </Card>
        ))}
      </div>

      <Card className="mt-6" title="Who uses which launcher" actions={<Button size="sm"><Plus className="h-3.5 w-3.5" /> Add override</Button>}>
        <p className="mb-3 text-xs text-zinc-400">
          Resolution: thread → parent channel → global. A job can also name one in its frontmatter. Changing a scope's launcher starts a fresh session
          there.
        </p>
        <table className="w-full text-left">
          <thead className="text-[11px] uppercase tracking-wider text-zinc-500">
            <tr>
              <th className="pb-2 font-medium">Scope</th>
              <th className="pb-2 font-medium">Set here</th>
              <th className="pb-2 font-medium">Effective</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-zinc-800/80">
            {scopes
              .filter((s) => s.kind !== "agent")
              .map((s) => {
                const eff = resolveLauncher(s.id);
                const bad = launchers.find((l) => l.path === eff.launcher)?.status === "refused";
                return (
                  <tr key={s.id}>
                    <td className="py-2 text-zinc-200">
                      <span className={cx(s.kind === "thread" && "pl-5 text-zinc-300")}>{s.kind === "thread" ? `└ ${s.label}` : s.label}</span>
                      <span className="ml-2 text-[11px] capitalize text-zinc-500">{s.kind}</span>
                    </td>
                    <td className="w-80 py-2">
                      {s.kind === "global" ? (
                        <LauncherPicker value={s.launcher === "direct" ? undefined : s.launcher} onChange={() => {}} inheritLabel="direct" />
                      ) : (
                        <span className="font-mono text-[12px] text-zinc-300">{s.launcher ?? <span className="text-zinc-600">inherit</span>}</span>
                      )}
                    </td>
                    <td className="py-2">
                      <span className={cx("font-mono text-[12px]", bad ? "text-red-400" : "text-zinc-200")}>{eff.launcher}</span>
                      <span className="ml-2 text-[11px] text-zinc-500">{eff.from}</span>
                    </td>
                  </tr>
                );
              })}
          </tbody>
        </table>
      </Card>
    </>
  );
}

import { Pause, Search } from "lucide-react";
import { useState } from "react";
import { auditLog, daemonLog, runLogs } from "../mock";
import { Badge, Button, Card, PageHeader, Tabs, cx, inputCls } from "../ui";

export function LogsScreen() {
  const [tab, setTab] = useState<"daemon" | "runs" | "audit">("daemon");
  const [q, setQ] = useState("");
  const [levels, setLevels] = useState({ info: true, warn: true, error: true });
  const [run, setRun] = useState(runLogs[0].file);
  const lines = daemonLog.filter((l) => levels[l.level as keyof typeof levels] && (!q || l.msg.toLowerCase().includes(q.toLowerCase())));
  const sel = runLogs.find((r) => r.file === run)!;
  return (
    <>
      <PageHeader title="Logs" subtitle="Streamed live over the Management API." />
      <Tabs
        value={tab}
        onChange={setTab}
        tabs={[
          { id: "daemon", label: "Daemon" },
          { id: "runs", label: "Run logs" },
          { id: "audit", label: "Audit log" },
        ]}
      />
      {tab === "daemon" && (
        <Card
          title={
            <span className="flex items-center gap-2">
              <span className="h-2 w-2 animate-pulse rounded-full bg-emerald-400" /> Live
            </span>
          }
          actions={
            <>
              <div className="relative">
                <Search className="absolute left-2 top-2 h-3.5 w-3.5 text-zinc-500" />
                <input className={cx(inputCls, "w-56 pl-7")} placeholder="Filter…" value={q} onChange={(e) => setQ(e.target.value)} />
              </div>
              {(["info", "warn", "error"] as const).map((lv) => (
                <button
                  key={lv}
                  onClick={() => setLevels((l) => ({ ...l, [lv]: !l[lv] }))}
                  className={cx("rounded border px-2 py-1 text-xs", levels[lv] ? "border-zinc-600 text-zinc-200" : "border-zinc-800 text-zinc-600")}
                >
                  {lv}
                </button>
              ))}
              <Button size="sm" variant="ghost">
                <Pause className="h-3.5 w-3.5" />
              </Button>
            </>
          }
        >
          <div className="font-mono text-[12px] leading-6">
            {lines.map((l, i) => (
              <div key={i} className="flex gap-3">
                <span className="text-zinc-500">{l.t}</span>
                <span className={cx("w-10", l.level === "error" ? "text-red-400" : l.level === "warn" ? "text-amber-400" : "text-zinc-500")}>{l.level}</span>
                <span className="w-20 text-sky-400/80">{l.src}</span>
                <span className="text-zinc-200">{l.msg}</span>
              </div>
            ))}
          </div>
        </Card>
      )}
      {tab === "runs" && (
        <div className="grid grid-cols-[320px_1fr] gap-4">
          <Card>
            {runLogs.map((r) => (
              <button
                key={r.file}
                onClick={() => setRun(r.file)}
                className={cx("flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left", run === r.file ? "bg-zinc-800" : "hover:bg-zinc-800/50")}
              >
                <span className="flex-1 truncate text-zinc-200">{r.name}</span>
                <Badge tone={r.result === "ok" ? "green" : "red"}>{r.result}</Badge>
                <span className="text-[11px] text-zinc-500">{r.duration}</span>
              </button>
            ))}
          </Card>
          <Card title={<span className="font-mono text-xs">{sel.file}</span>}>
            <pre className="whitespace-pre-wrap font-mono text-[12px] leading-relaxed text-zinc-300">
              {sel.result === "refused"
                ? `# ${sel.name}\nDate: 2026-10-10T17:00:00.000Z\nLauncher: ~/launchers/ccr-local.sh (from job frontmatter)\nResult: REFUSED — launcher is not executable\n\nNo process was started. No fallback was attempted.`
                : `# ${sel.name}\nDate: 2026-10-13T09:30:00.000Z\nSession: 77c2be19 (resumed)\nLauncher: ~/launchers/ccs-work.sh\nExit code: 0\n\n## Output\nMoved 4 files:\n  inbox/receipt-a.pdf → finance/2026/\n  inbox/sketch.png → projects/garden/\n  …\n\n## Skipped stdout lines (non-JSON)\n[ccs] using profile: work\n[ccs] config dir ready`}
            </pre>
          </Card>
        </div>
      )}
      {tab === "audit" && (
        <Card>
          <p className="mb-3 text-xs text-zinc-400">Every change made through the API. Secret values and prompt contents are never logged.</p>
          <table className="w-full text-left">
            <thead className="text-[11px] uppercase tracking-wider text-zinc-500">
              <tr>
                <th className="pb-2 font-medium">When</th>
                <th className="pb-2 font-medium">Who</th>
                <th className="pb-2 font-medium">Action</th>
                <th className="pb-2 font-medium">Target</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-zinc-800/80">
              {auditLog.map((a, i) => (
                <tr key={i}>
                  <td className="py-2 text-zinc-400">{a.t}</td>
                  <td className="py-2 text-zinc-200">{a.actor}</td>
                  <td className="py-2 font-mono text-[12px] text-sky-300">{a.action}</td>
                  <td className="py-2 text-zinc-300">{a.target}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
    </>
  );
}

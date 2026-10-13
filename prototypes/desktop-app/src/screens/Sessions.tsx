import { Archive, MessageSquare, Minimize2 } from "lucide-react";
import { useState } from "react";
import { chatHistory, scopes, sessions } from "../mock";
import { Badge, Button, Card, PageHeader, cx, fmtAgo, useApp } from "../ui";

export function SessionsScreen() {
  const { go } = useApp();
  const [sel, setSel] = useState(sessions[0].id);
  const [filter, setFilter] = useState<"all" | "active">("all");
  const s = sessions.find((x) => x.id === sel)!;
  const scope = scopes.find((x) => x.id === s.scopeId)!;
  const label = (id: string) => {
    const sc = scopes.find((x) => x.id === id)!;
    return sc.kind === "thread" ? `${scopes.find((p) => p.id === sc.parent)?.label} › ${sc.label}` : sc.label;
  };
  const rows = sessions.filter((x) => filter === "all" || x.active);
  return (
    <>
      <PageHeader
        title="Sessions"
        subtitle="One Claude Code session per (scope, launcher). Switching a scope's launcher parks its session; switching back resumes it."
        actions={
          <div className="flex rounded-md border border-zinc-700 p-0.5">
            {(["all", "active"] as const).map((f) => (
              <button key={f} onClick={() => setFilter(f)} className={cx("rounded px-2 py-1 text-xs", filter === f ? "bg-zinc-700 text-zinc-50" : "text-zinc-400")}>
                {f === "all" ? "All" : "In use"}
              </button>
            ))}
          </div>
        }
      />
      <div className="grid grid-cols-[1fr_380px] gap-4">
        <Card>
          <table className="w-full text-left">
            <thead className="text-[11px] uppercase tracking-wider text-zinc-500">
              <tr>
                <th className="pb-2 font-medium">Scope</th>
                <th className="pb-2 font-medium">Launcher</th>
                <th className="pb-2 font-medium">Session</th>
                <th className="pb-2 font-medium">Turns</th>
                <th className="pb-2 font-medium">Last used</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-zinc-800/80">
              {rows.map((x) => (
                <tr key={x.id} onClick={() => setSel(x.id)} className={cx("cursor-pointer hover:bg-zinc-800/40", x.id === sel && "bg-zinc-800/60")}>
                  <td className="py-2 pl-1">
                    <span className="text-zinc-100">{label(x.scopeId)}</span>
                    <div className="text-[11px] capitalize text-zinc-500">{scopes.find((sc) => sc.id === x.scopeId)?.kind}</div>
                  </td>
                  <td className="py-2 font-mono text-[12px] text-zinc-300">{x.launcher}</td>
                  <td className="py-2">
                    <code className="text-zinc-300">{x.id}</code> {x.active ? <Badge tone="green">in use</Badge> : <Badge>parked</Badge>}
                  </td>
                  <td className="py-2 text-zinc-300">{x.turns}</td>
                  <td className="py-2 text-zinc-400">{fmtAgo(x.lastUsedAt)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
        <Card
          title={
            <span>
              {label(s.scopeId)} · <code>{s.id}</code>
            </span>
          }
        >
          <dl className="grid grid-cols-2 gap-y-1 text-xs">
            <dt className="text-zinc-500">Launcher</dt>
            <dd className="font-mono text-zinc-300">{s.launcher}</dd>
            <dt className="text-zinc-500">Created</dt>
            <dd className="text-zinc-300">{fmtAgo(s.createdAt)}</dd>
            <dt className="text-zinc-500">Turns</dt>
            <dd className="text-zinc-300">{s.turns}</dd>
          </dl>
          <div className="mt-3 max-h-64 space-y-2 overflow-y-auto rounded-md border border-zinc-800 bg-zinc-950 p-3">
            {(chatHistory[s.scopeId] ?? [{ role: "user", text: s.firstMessage }, { role: "assistant", text: "…" }]).map((m, i) => (
              <div key={i} className="text-xs">
                <span className={cx("font-semibold", m.role === "user" ? "text-amber-300" : "text-sky-300")}>{m.role}</span>
                <p className="whitespace-pre-wrap text-zinc-300">{m.text}</p>
              </div>
            ))}
          </div>
          <div className="mt-3 flex flex-wrap gap-2">
            <Button size="sm" variant="primary" disabled={!s.active} onClick={() => go("chat", scope.id)}>
              <MessageSquare className="h-3.5 w-3.5" /> Continue in Chat
            </Button>
            <Button size="sm" disabled={!s.active}>
              <Minimize2 className="h-3.5 w-3.5" /> Compact
            </Button>
            <Button size="sm" variant="ghost">
              <Archive className="h-3.5 w-3.5" /> Clear (keeps a backup)
            </Button>
          </div>
          {!s.active && <p className="mt-2 text-[11px] text-zinc-500">Parked: this scope now uses a different launcher. Switch back to resume it.</p>}
        </Card>
      </div>
    </>
  );
}

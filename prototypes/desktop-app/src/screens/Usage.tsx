import { useState } from "react";
import { usageDays, usageRows } from "../mock";
import { Card, PageHeader, cx } from "../ui";

const fmtTok = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(2)} M` : `${Math.round(n / 1e3)} k`);

export function UsageScreen() {
  const [range, setRange] = useState<"7" | "14" | "30">("14");
  const days = usageDays.slice(range === "7" ? -7 : 0);
  const max = Math.max(...days.map((d) => d.usd));
  const total = days.reduce((a, d) => a + d.usd, 0);
  const [hover, setHover] = useState<number | null>(null);
  return (
    <>
      <PageHeader
        title="Usage & cost"
        subtitle="Estimated from token counts in each session's transcript. Launchers that route to other providers may bill differently."
        actions={
          <div className="flex rounded-md border border-zinc-700 p-0.5">
            {(["7", "14", "30"] as const).map((r) => (
              <button key={r} onClick={() => setRange(r)} className={cx("rounded px-2 py-1 text-xs", range === r ? "bg-zinc-700 text-zinc-50" : "text-zinc-400")}>
                {r} days
              </button>
            ))}
          </div>
        }
      />
      <div className="grid grid-cols-4 gap-3">
        {[
          ["Estimated cost", `$${total.toFixed(2)}`, `last ${days.length} days`],
          ["Today", `$${days[days.length - 1].usd.toFixed(2)}`, "so far"],
          ["Tokens in / out", "3.24 M / 416 k", "all scopes"],
          ["Cache hit", "87 %", "reads vs fresh input"],
        ].map(([l, v, s]) => (
          <div key={l} className="rounded-lg border border-zinc-800 bg-zinc-900/60 px-4 py-3">
            <div className="text-[11px] uppercase tracking-wider text-zinc-500">{l}</div>
            <div className="mt-1 text-xl font-semibold tabular-nums text-zinc-50">{v}</div>
            <div className="text-[11px] text-zinc-500">{s}</div>
          </div>
        ))}
      </div>
      <Card className="mt-4" title="Cost per day (USD)">
        <div className="relative flex h-40 items-end gap-1.5">
          {days.map((d, i) => (
            <div key={d.day} className="flex flex-1 flex-col items-center gap-1" onMouseEnter={() => setHover(i)} onMouseLeave={() => setHover(null)}>
              {hover === i && <span className="text-[11px] tabular-nums text-zinc-200">${d.usd.toFixed(2)}</span>}
              <div className={cx("w-full rounded-t", hover === i ? "bg-amber-400" : "bg-amber-500/70")} style={{ height: `${(d.usd / max) * 120}px` }} />
            </div>
          ))}
        </div>
        <div className="mt-1 flex gap-1.5">
          {days.map((d, i) => (
            <span key={d.day} className="flex-1 text-center text-[10px] text-zinc-500">
              {i % 2 === 0 ? d.day : ""}
            </span>
          ))}
        </div>
      </Card>
      <Card className="mt-4" title="By scope">
        <table className="w-full text-left">
          <thead className="text-[11px] uppercase tracking-wider text-zinc-500">
            <tr>
              <th className="pb-2 font-medium">Scope</th>
              <th className="pb-2 font-medium">Launcher</th>
              <th className="pb-2 text-right font-medium">Turns</th>
              <th className="pb-2 text-right font-medium">Input</th>
              <th className="pb-2 text-right font-medium">Output</th>
              <th className="pb-2 text-right font-medium">Cache hit</th>
              <th className="pb-2 text-right font-medium">Cost</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-zinc-800/80 tabular-nums">
            {usageRows.map((r) => (
              <tr key={r.label}>
                <td className="py-2 text-zinc-100">
                  {r.label} <span className="text-[11px] text-zinc-500">{r.scope}</span>
                </td>
                <td className="py-2 font-mono text-[12px] text-zinc-400">{r.launcher}</td>
                <td className="py-2 text-right text-zinc-300">{r.turns}</td>
                <td className="py-2 text-right text-zinc-300">{fmtTok(r.input)}</td>
                <td className="py-2 text-right text-zinc-300">{fmtTok(r.output)}</td>
                <td className="py-2 text-right text-zinc-300">{r.cacheHit} %</td>
                <td className="py-2 text-right text-zinc-100">${r.usd.toFixed(2)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Card>
    </>
  );
}

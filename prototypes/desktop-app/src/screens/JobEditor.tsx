import { ArrowLeft, CircleAlert, Code, FolderOpen, Info, ListChecks, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { bothDayFieldsRestricted, describeCron, fmtRun, nextRuns, validateCron } from "../cron";
import { jobs, launchers, type Job } from "../mock";
import { Badge, Button, Card, Field, PageHeader, Toggle, cx, inputCls, useApp } from "../ui";

type Mode = "minutes" | "daily" | "weekly" | "monthly" | "once" | "custom";
const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

const NEW_JOB: Job = {
  name: "",
  schedule: "0 9 * * 1-5",
  recurring: true,
  enabled: true,
  notify: "always",
  notifyOnFinish: false,
  prompt: "",
};

function toFile(j: Job) {
  const lines = [`schedule: "${j.schedule}"`, `recurring: ${j.recurring}`];
  if (!j.enabled) lines.push("enabled: false");
  if (j.launcher) lines.push(`launcher: ${j.launcher}`);
  if (j.model) lines.push(`model: ${j.model}`);
  if (j.timeout) lines.push(`timeout: ${j.timeout}`);
  if (j.retry) lines.push(`retry: ${j.retry}`);
  if (j.retryDelay) lines.push(`retry_delay: ${j.retryDelay}`);
  if (j.notify !== "always") lines.push(`notify: ${j.notify === "errors" ? "error" : "false"}`);
  return `---\n${lines.join("\n")}\n---\n${j.prompt}\n`;
}

function fromFile(text: string, base: Job): Job {
  const m = text.match(/^---\s*\n([\s\S]*?)\n---\s*\n([\s\S]*)$/);
  if (!m) return base;
  const get = (k: string) => m[1].split("\n").map((l) => l.trim()).find((l) => l.startsWith(k + ":"))?.slice(k.length + 1).trim().replace(/^["']|["']$/g, "");
  const notify = get("notify");
  return {
    ...base,
    schedule: get("schedule") ?? base.schedule,
    recurring: /^(true|yes|1)$/i.test(get("recurring") ?? ""),
    enabled: !/^(false|no|0)$/i.test(get("enabled") ?? ""),
    launcher: get("launcher") || undefined,
    model: get("model") || undefined,
    timeout: Number(get("timeout")) || undefined,
    retry: Number(get("retry")) || undefined,
    retryDelay: Number(get("retry_delay")) || undefined,
    notify: notify === "error" ? "errors" : notify === "false" || notify === "no" ? "never" : "always",
    prompt: m[2].trim(),
  };
}

export function JobEditor() {
  const { param, go } = useApp();
  const existing = jobs.find((j) => j.name === param);
  const [job, setJob] = useState<Job>(existing ?? NEW_JOB);
  const [raw, setRaw] = useState(false);
  const [rawText, setRawText] = useState("");
  const set = <K extends keyof Job>(k: K, v: Job[K]) => setJob((j) => ({ ...j, [k]: v }));

  const toggleRaw = () => {
    if (!raw) setRawText(toFile(job));
    else setJob(fromFile(rawText, job));
    setRaw(!raw);
  };

  return (
    <>
      <PageHeader
        title={existing ? `Edit job · ${existing.name}` : "New job"}
        subtitle={
          <button onClick={() => go("status")} className="inline-flex items-center gap-1 text-zinc-400 hover:text-zinc-200">
            <ArrowLeft className="h-3.5 w-3.5" /> Back to Status & Jobs
          </button>
        }
        actions={
          <>
            <Button onClick={toggleRaw}>
              {raw ? <ListChecks className="h-3.5 w-3.5" /> : <Code className="h-3.5 w-3.5" />}
              {raw ? "Back to form" : "Edit raw markdown"}
            </Button>
            {existing && (
              <Button variant="ghost">
                <Trash2 className="h-3.5 w-3.5" /> Delete
              </Button>
            )}
            <Button variant="primary" onClick={() => go("status")}>
              Save
            </Button>
          </>
        }
      />

      {raw ? (
        <Card title={<span className="font-mono">{existing?.agent ? `agents/${existing.agent}/jobs/` : "jobs/"}{(existing?.name.split("/").pop() ?? (job.name || "untitled"))}.md</span>}>
          <textarea value={rawText} onChange={(e) => setRawText(e.target.value)} spellCheck={false} className={cx(inputCls, "h-80 font-mono text-[12px] leading-relaxed")} />
          <p className="mt-2 text-[11px] text-zinc-500">
            Saved byte-for-byte through the API. Unknown frontmatter keys are kept. The form reads the same keys back when you switch.
          </p>
        </Card>
      ) : (
        <div className="grid grid-cols-[1fr_340px] gap-4">
          <div className="space-y-4">
            <Card title="Basics">
              <div className="grid grid-cols-2 gap-4">
                <Field label="Name" hint="File name in the jobs folder. Letters, digits, . _ -">
                  <input className={inputCls} value={job.name} placeholder="e.g. morning-brief" onChange={(e) => set("name", e.target.value)} disabled={!!existing} />
                </Field>
                <Field label="Runs as">
                  <select className={inputCls} defaultValue={job.agent ?? ""}>
                    <option value="">Main daemon (job's own session)</option>
                    <option value="researcher">Agent: researcher</option>
                    <option value="scribe">Agent: scribe</option>
                  </select>
                </Field>
              </div>
              <div className="mt-4">
                <Field label="Prompt" hint="If the whole prompt is a path ending .md/.txt/.prompt, that file is read at run time.">
                  <textarea className={cx(inputCls, "h-28")} value={job.prompt} onChange={(e) => set("prompt", e.target.value)} />
                </Field>
              </div>
            </Card>

            <CronBuilder value={job.schedule} onChange={(v) => set("schedule", v)} recurring={job.recurring} setRecurring={(v) => set("recurring", v)} />

            <Card title="Launcher">
              <LauncherPicker value={job.launcher} onChange={(v) => set("launcher", v)} />
            </Card>

            <Card title="Run options">
              <div className="grid grid-cols-4 gap-4">
                <Field label="Model" hint="Empty = daemon default">
                  <input className={inputCls} value={job.model ?? ""} placeholder="default" onChange={(e) => set("model", e.target.value || undefined)} />
                </Field>
                <Field label="Timeout (s)" hint="Empty = 30 min">
                  <input className={inputCls} value={job.timeout ?? ""} placeholder="1800" onChange={(e) => set("timeout", Number(e.target.value) || undefined)} />
                </Field>
                <Field label="Retries" hint="After a failure">
                  <input className={inputCls} value={job.retry ?? ""} placeholder="0" onChange={(e) => set("retry", Number(e.target.value) || undefined)} />
                </Field>
                <Field label="Retry delay (s)">
                  <input className={inputCls} value={job.retryDelay ?? ""} placeholder="300" onChange={(e) => set("retryDelay", Number(e.target.value) || undefined)} />
                </Field>
              </div>
            </Card>
          </div>

          <div className="space-y-4">
            <Card title="Enabled">
              <Toggle on={job.enabled} onChange={(v) => set("enabled", v)} label={job.enabled ? "Job runs on schedule" : "Paused"} />
            </Card>
            <Card title="Notifications">
              <Field label="Discord DM to the owner">
                <select className={inputCls} value={job.notify} onChange={(e) => set("notify", e.target.value as Job["notify"])}>
                  <option value="always">Always post the result</option>
                  <option value="errors">Only on errors</option>
                  <option value="never">Never</option>
                </select>
              </Field>
              <div className="mt-3">
                <Toggle on={job.notifyOnFinish} onChange={(v) => set("notifyOnFinish", v)} label="Desktop notification when finished" />
                <p className="mt-1 text-[11px] text-zinc-500">Failures always notify. This adds a notice on success too. Stored in the app, not in the job file.</p>
              </div>
            </Card>
            {existing?.lastResult === "refused" && (
              <Card title={<span className="flex items-center gap-1.5 text-red-300"><CircleAlert className="h-4 w-4" /> Last run refused</span>}>
                <p className="text-zinc-400">Fri 10 Oct 17:00 — the launcher in this job's frontmatter is not executable. Fix the file permissions or pick another launcher.</p>
              </Card>
            )}
          </div>
        </div>
      )}
    </>
  );
}

function CronBuilder({ value, onChange, recurring, setRecurring }: { value: string; onChange: (v: string) => void; recurring: boolean; setRecurring: (v: boolean) => void }) {
  const [mode, setMode] = useState<Mode>(!recurring ? "once" : guessMode(value));
  const [time, setTime] = useState(() => guessTime(value));
  const [every, setEvery] = useState(15);
  const [days, setDays] = useState<number[]>(() => guessDays(value));
  const [dom, setDom] = useState(1);
  const [date, setDate] = useState("2026-10-20");

  const build = (m: Mode, t = time, e = every, d = days, dm = dom, dt = date) => {
    const [hh, mm] = t.split(":").map(Number);
    if (m === "minutes") return `*/${e} * * * *`;
    if (m === "daily") return `${mm} ${hh} * * *`;
    if (m === "weekly") return `${mm} ${hh} * * ${d.length ? d.join(",") : "*"}`.replace("* * 1,2,3,4,5", "* * 1-5");
    if (m === "monthly") return `${mm} ${hh} ${dm} * *`;
    if (m === "once") {
      const [, mo, da] = dt.split("-").map(Number);
      return `${mm} ${hh} ${da} ${mo} *`;
    }
    return value;
  };
  const apply = (m: Mode, patch: Partial<{ t: string; e: number; d: number[]; dm: number; dt: string }> = {}) => {
    setMode(m);
    if (m !== "custom") onChange(build(m, patch.t ?? time, patch.e ?? every, patch.d ?? days, patch.dm ?? dom, patch.dt ?? date));
    setRecurring(m !== "once");
  };

  const err = validateCron(value);
  const runs = useMemo(() => nextRuns(value, 5, new Date("2026-10-13T10:05:00Z")), [value]);
  const modes: [Mode, string][] = [
    ["minutes", "Every N minutes"],
    ["daily", "Daily"],
    ["weekly", "Weekly"],
    ["monthly", "Monthly"],
    ["once", "Once"],
    ["custom", "Custom cron"],
  ];

  return (
    <Card title="Schedule" actions={<span className="text-[11px] text-zinc-500">Daemon timezone: UTC</span>}>
      <div className="mb-4 flex flex-wrap gap-1">
        {modes.map(([m, label]) => (
          <button
            key={m}
            onClick={() => apply(m)}
            className={cx("rounded-md border px-2.5 py-1 text-xs", mode === m ? "border-amber-500 bg-amber-500/10 text-amber-200" : "border-zinc-700 text-zinc-400 hover:text-zinc-200")}
          >
            {label}
          </button>
        ))}
      </div>

      <div className="grid grid-cols-[1fr_260px] gap-6">
        <div className="space-y-3">
          {mode === "minutes" && (
            <Field label="Every">
              <select className={inputCls} value={every} onChange={(e) => { setEvery(+e.target.value); apply("minutes", { e: +e.target.value }); }}>
                {[5, 10, 15, 20, 30].map((n) => (
                  <option key={n} value={n}>{n} minutes</option>
                ))}
              </select>
            </Field>
          )}
          {(mode === "daily" || mode === "weekly" || mode === "monthly" || mode === "once") && (
            <Field label="At">
              <input type="time" className={inputCls} value={time} onChange={(e) => { setTime(e.target.value); apply(mode, { t: e.target.value }); }} />
            </Field>
          )}
          {mode === "weekly" && (
            <Field label="On">
              <div className="flex gap-1">
                {DAYS.map((d, i) => {
                  const on = days.includes(i);
                  return (
                    <button
                      key={d}
                      onClick={() => {
                        const nd = on ? days.filter((x) => x !== i) : [...days, i].sort();
                        setDays(nd);
                        apply("weekly", { d: nd });
                      }}
                      className={cx("w-10 rounded-md border py-1 text-xs", on ? "border-amber-500 bg-amber-500/10 text-amber-200" : "border-zinc-700 text-zinc-400")}
                    >
                      {d}
                    </button>
                  );
                })}
              </div>
            </Field>
          )}
          {mode === "monthly" && (
            <Field label="Day of month" hint="Days 29–31 skip shorter months.">
              <input type="number" min={1} max={31} className={inputCls} value={dom} onChange={(e) => { setDom(+e.target.value); apply("monthly", { dm: +e.target.value }); }} />
            </Field>
          )}
          {mode === "once" && (
            <Field label="Date" hint="One-shot: after it runs, the daemon removes the schedule line and the job stays as a file.">
              <input type="date" className={inputCls} value={date} onChange={(e) => { setDate(e.target.value); apply("once", { dt: e.target.value }); }} />
            </Field>
          )}
          <Field label="Cron expression" hint={mode === "custom" ? "minute hour day-of-month month day-of-week. 0 = Sunday." : "Generated from the form. Edit it to switch to Custom."}>
            <input
              className={cx(inputCls, "font-mono", err && "border-red-500")}
              value={value}
              onChange={(e) => {
                setMode("custom");
                onChange(e.target.value);
              }}
            />
          </Field>
          {err && (
            <p className="flex items-start gap-1.5 text-xs text-red-400">
              <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" /> {err}
            </p>
          )}
          {!err && bothDayFieldsRestricted(value) && (
            <p className="flex items-start gap-1.5 text-xs text-amber-300">
              <Info className="mt-0.5 h-3.5 w-3.5 shrink-0" /> Both day fields are set, so the job runs only when the date matches <b>both</b> (ClaudeClaw
              semantics; standard cron would match either).
            </p>
          )}
        </div>
        <div>
          <div className="mb-1 flex items-center justify-between text-xs font-medium text-zinc-400">
            Next runs <Badge>{err ? "invalid" : describeCron(value)}</Badge>
          </div>
          <ol className="space-y-1 rounded-md border border-zinc-800 bg-zinc-950 p-3 font-mono text-[12px]">
            {runs.length === 0 && <li className="text-zinc-500">{err ? "Fix the expression to see runs." : "No run in the next 400 days."}</li>}
            {runs.map((r, i) => (
              <li key={i} className={cx(i === 0 ? "text-zinc-100" : "text-zinc-400")}>
                {fmtRun(r)}
                {!recurring && i === 0 && <span className="ml-2 text-sky-400">then done</span>}
              </li>
            )).slice(0, recurring ? 5 : 1)}
          </ol>
        </div>
      </div>
    </Card>
  );
}

function guessMode(expr: string): Mode {
  const d = describeCron(expr);
  if (d.startsWith("Every") && !d.includes(",")) return "minutes";
  if (d.startsWith("Daily")) return "daily";
  if (d.startsWith("Weekdays") || /^(Sun|Mon|Tue|Wed|Thu|Fri|Sat)/.test(d)) return "weekly";
  if (d.startsWith("Monthly")) return "monthly";
  return "custom";
}

function guessDays(expr: string): number[] {
  const dow = expr.trim().split(/\s+/)[4] ?? "*";
  if (dow === "1-5") return [1, 2, 3, 4, 5];
  if (/^[\d,]+$/.test(dow)) return dow.split(",").map(Number);
  return [1, 2, 3, 4, 5];
}

function guessTime(expr: string) {
  const [m, h] = expr.split(/\s+/);
  return /^\d+$/.test(m) && /^\d+$/.test(h) ? `${h.padStart(2, "0")}:${m.padStart(2, "0")}` : "09:00";
}

export function LauncherPicker({ value, onChange, inheritLabel = "Inherit from global (direct)" }: { value?: string; onChange: (v?: string) => void; inheritLabel?: string }) {
  const selected = launchers.find((l) => l.path === value);
  return (
    <div>
      <div className="flex gap-2">
        <select className={cx(inputCls, "font-mono")} value={value ?? ""} onChange={(e) => onChange(e.target.value || undefined)}>
          <option value="">{inheritLabel}</option>
          {launchers.map((l) => (
            <option key={l.path} value={l.path}>
              {l.path}
              {l.status === "refused" ? "  — not runnable" : ""}
            </option>
          ))}
        </select>
        <Button title="Add a new launcher path with a file picker">
          <FolderOpen className="h-3.5 w-3.5" /> Browse…
        </Button>
      </div>
      <p className="mt-1 text-[11px] text-zinc-500">
        The list shows <code>direct</code> plus every path already used by another scope. Switching launcher starts a fresh session for this scope.
      </p>
      {selected?.status === "refused" && (
        <p className="mt-2 flex items-start gap-1.5 text-xs text-red-400">
          <CircleAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" /> {selected.statusDetail}
        </p>
      )}
    </div>
  );
}

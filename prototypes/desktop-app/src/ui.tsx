import { createContext, useContext, type ReactNode } from "react";
import type { Daemon, DaemonStatus } from "./mock";

export function cx(...c: (string | false | undefined | null)[]) {
  return c.filter(Boolean).join(" ");
}

export function Button({
  children,
  variant = "default",
  size = "md",
  className,
  ...rest
}: React.ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "default" | "primary" | "danger" | "ghost"; size?: "sm" | "md" }) {
  return (
    <button
      {...rest}
      className={cx(
        "inline-flex items-center gap-1.5 rounded-md font-medium transition-colors disabled:opacity-40 disabled:pointer-events-none",
        size === "sm" ? "px-2 py-1 text-xs" : "px-3 py-1.5 text-[13px]",
        variant === "default" && "bg-zinc-800 hover:bg-zinc-700 text-zinc-100 border border-zinc-700",
        variant === "primary" && "bg-amber-500 hover:bg-amber-400 text-zinc-950",
        variant === "danger" && "bg-red-600/90 hover:bg-red-500 text-white",
        variant === "ghost" && "hover:bg-zinc-800 text-zinc-300",
        className,
      )}
    >
      {children}
    </button>
  );
}

export function Badge({ children, tone = "zinc" }: { children: ReactNode; tone?: "zinc" | "green" | "red" | "amber" | "blue" | "violet" }) {
  const tones = {
    zinc: "bg-zinc-800 text-zinc-300 border-zinc-700",
    green: "bg-emerald-500/10 text-emerald-400 border-emerald-500/30",
    red: "bg-red-500/10 text-red-400 border-red-500/30",
    amber: "bg-amber-500/10 text-amber-400 border-amber-500/30",
    blue: "bg-sky-500/10 text-sky-400 border-sky-500/30",
    violet: "bg-violet-500/10 text-violet-400 border-violet-500/30",
  };
  return <span className={cx("inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-[11px] font-medium", tones[tone])}>{children}</span>;
}

export function Card({ title, actions, children, className }: { title?: ReactNode; actions?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={cx("rounded-lg border border-zinc-800 bg-zinc-900/60", className)}>
      {(title || actions) && (
        <header className="flex items-center justify-between border-b border-zinc-800 px-4 py-2.5">
          <h3 className="text-[13px] font-semibold text-zinc-100">{title}</h3>
          <div className="flex items-center gap-2">{actions}</div>
        </header>
      )}
      <div className="p-4">{children}</div>
    </section>
  );
}

export function PageHeader({ title, subtitle, actions }: { title: string; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="mb-5 flex items-start justify-between gap-4">
      <div>
        <h1 className="text-lg font-semibold text-zinc-50">{title}</h1>
        {subtitle && <p className="mt-0.5 text-zinc-400">{subtitle}</p>}
      </div>
      <div className="flex items-center gap-2">{actions}</div>
    </div>
  );
}

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="block">
      <span className="mb-1 block text-xs font-medium text-zinc-400">{label}</span>
      {children}
      {hint && <span className="mt-1 block text-[11px] text-zinc-500">{hint}</span>}
    </label>
  );
}

export const inputCls =
  "w-full rounded-md border border-zinc-700 bg-zinc-950 px-2.5 py-1.5 text-[13px] text-zinc-100 placeholder:text-zinc-600 focus:border-amber-500 focus:outline-none";

export function Toggle({ on, onChange, label }: { on: boolean; onChange?: (v: boolean) => void; label?: string }) {
  return (
    <button type="button" onClick={() => onChange?.(!on)} className="inline-flex items-center gap-2 text-[13px]">
      <span className={cx("relative h-4 w-7 rounded-full transition-colors", on ? "bg-amber-500" : "bg-zinc-700")}>
        <span className={cx("absolute top-0.5 h-3 w-3 rounded-full bg-white transition-all", on ? "left-3.5" : "left-0.5")} />
      </span>
      {label}
    </button>
  );
}

export function Tabs<T extends string>({ tabs, value, onChange }: { tabs: { id: T; label: ReactNode }[]; value: T; onChange: (v: T) => void }) {
  return (
    <div className="mb-4 flex gap-1 border-b border-zinc-800">
      {tabs.map((t) => (
        <button
          key={t.id}
          onClick={() => onChange(t.id)}
          className={cx(
            "-mb-px border-b-2 px-3 py-2 text-[13px]",
            value === t.id ? "border-amber-500 text-zinc-50" : "border-transparent text-zinc-400 hover:text-zinc-200",
          )}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

export const statusMeta: Record<DaemonStatus, { label: string; dot: string; tone: "green" | "zinc" | "red" | "amber" }> = {
  running: { label: "Running", dot: "bg-emerald-400", tone: "green" },
  stopped: { label: "Stopped", dot: "bg-zinc-500", tone: "zinc" },
  "crash-loop": { label: "Crash loop", dot: "bg-red-500", tone: "red" },
  unreachable: { label: "Unreachable", dot: "bg-amber-400", tone: "amber" },
};

export function Dot({ status, pulse }: { status: DaemonStatus; pulse?: boolean }) {
  return <span className={cx("inline-block h-2 w-2 shrink-0 rounded-full", statusMeta[status].dot, pulse && "animate-pulse")} />;
}

export type Screen = "status" | "job" | "chat" | "sessions" | "discord" | "agents" | "logs" | "usage" | "launchers" | "settings";

export interface Notice {
  id: number;
  kind: "error" | "warn" | "info" | "ok";
  title: string;
  body: string;
  action?: { label: string; screen: Screen; daemonId: string; param?: string };
}

export interface AppCtx {
  daemons: Daemon[];
  daemon: Daemon;
  screen: Screen;
  param?: string;
  go: (screen: Screen, param?: string, daemonId?: string) => void;
  setStatus: (id: string, s: Daemon["status"]) => void;
  update: (id: string, patch: Partial<Daemon>) => void;
  notify: (n: Omit<Notice, "id">) => void;
}

export const Ctx = createContext<AppCtx>(null!);
export const useApp = () => useContext(Ctx);

export function fmtAgo(iso?: string) {
  if (!iso) return "—";
  const now = Date.parse("2026-10-13T10:05:00Z");
  const mins = Math.round((now - Date.parse(iso)) / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins} min ago`;
  if (mins < 60 * 24) return `${Math.round(mins / 60)} h ago`;
  return `${Math.round(mins / 1440)} d ago`;
}

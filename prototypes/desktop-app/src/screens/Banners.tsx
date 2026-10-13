import { CircleAlert, Play, PlugZap, RotateCw, TriangleAlert, WifiOff } from "lucide-react";
import { Button, useApp } from "../ui";

export function DaemonBanner() {
  const { daemon, setStatus, go, screen } = useApp();
  if (daemon.status === "running" || screen === "status") return null;
  const tone =
    daemon.status === "crash-loop"
      ? "border-red-500/40 bg-red-500/10 text-red-200"
      : daemon.status === "unreachable"
        ? "border-amber-500/40 bg-amber-500/10 text-amber-100"
        : "border-zinc-700 bg-zinc-800/60 text-zinc-200";
  const Icon = daemon.status === "crash-loop" ? CircleAlert : daemon.status === "unreachable" ? WifiOff : TriangleAlert;
  const text =
    daemon.status === "crash-loop"
      ? "Crash loop: the Watchdog gave up and the daemon is not running."
      : daemon.status === "unreachable"
        ? "Can't reach this remote daemon."
        : "The daemon is not running.";
  return (
    <div className={`flex items-center gap-3 border-b px-6 py-2.5 ${tone}`}>
      <Icon className="h-4 w-4 shrink-0" />
      <span className="flex-1">{text}</span>
      <Button size="sm" variant="ghost" onClick={() => go("status")}>
        Details
      </Button>
      {daemon.status === "stopped" && (
        <Button size="sm" variant="primary" onClick={() => setStatus(daemon.id, "running")}>
          <Play className="h-3 w-3" /> Start
        </Button>
      )}
    </div>
  );
}

export function Offline() {
  const { daemon, go } = useApp();
  return (
    <div className="mx-auto mt-16 max-w-md text-center">
      <PlugZap className="mx-auto h-10 w-10 text-zinc-600" />
      <h2 className="mt-3 text-base font-semibold text-zinc-100">No live data from {daemon.name}</h2>
      <p className="mt-1 text-zinc-400">
        The app reads everything through the Management API and never opens the daemon's files, so this screen is empty while the daemon is not
        answering.
      </p>
      <Button className="mt-4" onClick={() => go("status")}>
        <RotateCw className="h-3.5 w-3.5" /> Go to Status
      </Button>
    </div>
  );
}

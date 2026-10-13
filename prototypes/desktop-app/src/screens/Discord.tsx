import { CircleAlert, Hash, MessageSquareText, Plus, UserMinus, UserPlus } from "lucide-react";
import { useState } from "react";
import { discordChannels, launcherStatus } from "../mock";
import { Badge, Button, Card, PageHeader, cx, fmtAgo, useApp } from "../ui";

export function DiscordScreen() {
  const { go } = useApp();
  const [sel, setSel] = useState("ch-research");
  const [fired, setFired] = useState<string[]>([]);
  const ch = discordChannels.find((c) => c.id === sel)!;
  const bad = launcherStatus(ch.launcher) === "refused";
  return (
    <>
      <PageHeader
        title="Discord"
        subtitle={
          <span className="flex items-center gap-2">
            <span className="h-2 w-2 rounded-full bg-emerald-400" /> Gateway connected · server “Fictional Workshop” · DMs from 2 allowed users
          </span>
        }
        actions={
          <Button>
            <Plus className="h-3.5 w-3.5" /> Listen in a channel…
          </Button>
        }
      />
      <div className="grid grid-cols-[260px_1fr] gap-4">
        <Card>
          {discordChannels.map((c) => (
            <div key={c.id}>
              <button
                onClick={() => setSel(c.id)}
                className={cx("flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left", sel === c.id ? "bg-zinc-800 text-zinc-50" : "text-zinc-300 hover:bg-zinc-800/50")}
              >
                <Hash className="h-4 w-4 text-zinc-500" /> {c.name}
                {launcherStatus(c.launcher) === "refused" && <CircleAlert className="ml-auto h-3.5 w-3.5 text-red-400" />}
              </button>
              {c.threads
                .filter((t) => !fired.includes(t.id))
                .map((t) => (
                  <div key={t.id} className="flex items-center gap-2 py-1 pl-8 text-zinc-400">
                    <MessageSquareText className="h-3.5 w-3.5" /> {t.name}
                  </div>
                ))}
            </div>
          ))}
        </Card>
        <div className="space-y-4">
          <Card title={`#${ch.name}`}>
            <dl className="grid grid-cols-3 gap-4">
              <div>
                <dt className="text-[11px] text-zinc-500">Replies to</dt>
                <dd className="text-zinc-200">{ch.trigger === "mention" ? "Mentions only" : "Every message"}</dd>
              </div>
              <div>
                <dt className="text-[11px] text-zinc-500">Who may talk to Claude</dt>
                <dd className="text-zinc-200">{ch.allowedUsers}</dd>
              </div>
              <div>
                <dt className="text-[11px] text-zinc-500">Launcher</dt>
                <dd className={cx("font-mono text-[12px]", bad ? "text-red-400" : "text-zinc-200")}>{ch.launcher}</dd>
              </div>
            </dl>
            {bad && (
              <div className="mt-3 flex items-center gap-2 rounded-md border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-200">
                <CircleAlert className="h-4 w-4" /> Messages here are refused: this channel's launcher is not executable. Users see a short error in Discord.
                <Button size="sm" className="ml-auto" onClick={() => go("launchers")}>
                  Fix
                </Button>
              </div>
            )}
            <div className="mt-3 flex gap-2">
              <Button size="sm">Edit channel…</Button>
              <Button size="sm" variant="ghost" onClick={() => go("chat", ch.id)}>
                Chat in this scope
              </Button>
              <Button size="sm" variant="ghost">
                Stop listening
              </Button>
            </div>
          </Card>
          <Card
            title="Threads with their own agent"
            actions={
              <Button size="sm" variant="primary">
                <UserPlus className="h-3.5 w-3.5" /> Hire into a new thread…
              </Button>
            }
          >
            <p className="mb-3 text-xs text-zinc-400">
              A hired thread gets its own working folder and session, inherits this channel's launcher unless overridden, and keeps going until fired.
            </p>
            {ch.threads.length === 0 && <p className="text-zinc-500">No threads in this channel.</p>}
            {ch.threads.map((t) => {
              const isFired = fired.includes(t.id);
              return (
                <div key={t.id} className={cx("flex items-center gap-3 border-b border-zinc-800 py-2 last:border-0", isFired && "opacity-50")}>
                  <MessageSquareText className="h-4 w-4 text-zinc-400" />
                  <span className="flex-1">
                    <span className="text-zinc-100">{t.name}</span>
                    <span className="ml-2 font-mono text-[11px] text-zinc-500">agents/{t.agent}-…/</span>
                  </span>
                  <span className="text-xs text-zinc-500">active {fmtAgo(t.lastActive)}</span>
                  {isFired ? (
                    <Badge>fired · folder archived</Badge>
                  ) : (
                    <>
                      <Button size="sm" variant="ghost" onClick={() => go("chat", t.id)}>
                        Chat
                      </Button>
                      <Button size="sm" variant="ghost" onClick={() => setFired((f) => [...f, t.id])}>
                        <UserMinus className="h-3.5 w-3.5" /> Fire
                      </Button>
                    </>
                  )}
                </div>
              );
            })}
          </Card>
        </div>
      </div>
    </>
  );
}

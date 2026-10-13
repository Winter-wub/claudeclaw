import { Bot, ChevronDown, CircleAlert, Hash, Globe, MessageSquareText, Paperclip, Send, Timer, Wrench } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { chatHistory, launcherStatus, resolveLauncher, scopes, sessions, type ChatMessage, type Scope } from "../mock";
import { Badge, Button, cx, inputCls, useApp } from "../ui";

const KIND_LABEL: Record<Scope["kind"], string> = { global: "Global", channel: "Discord channels", thread: "Discord threads", agent: "Agents", job: "Jobs" };
const KIND_ICON = { global: Globe, channel: Hash, thread: MessageSquareText, agent: Bot, job: Timer };

function scopeLabel(s: Scope) {
  if (s.kind === "thread") return `${scopes.find((p) => p.id === s.parent)?.label} › ${s.label}`;
  return s.label;
}

export function ChatScreen() {
  const { go, param } = useApp();
  const [scopeId, setScopeId] = useState(param ?? "global");
  const [open, setOpen] = useState(false);
  const [msgs, setMsgs] = useState<Record<string, ChatMessage[]>>(chatHistory);
  const [draft, setDraft] = useState("");
  const scope = scopes.find((s) => s.id === scopeId)!;
  const { launcher, from } = resolveLauncher(scopeId);
  const refused = launcherStatus(launcher) === "refused";
  const session = sessions.find((s) => s.scopeId === scopeId && s.launcher === launcher && s.active);
  const list = msgs[scopeId] ?? [];
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [list.length]);

  const send = () => {
    if (!draft.trim()) return;
    const user: ChatMessage = { role: "user", text: draft };
    const reply: ChatMessage = refused
      ? { role: "system", error: true, text: `Run refused: the launcher for ${scopeLabel(scope)} is not executable. Nothing ran and no other launcher was tried.` }
      : { role: "assistant", text: "(mock reply) Done — this answer stays in the app and is not posted to Discord.", tools: ["Read"] };
    setMsgs((m) => ({ ...m, [scopeId]: [...(m[scopeId] ?? []), user] }));
    setDraft("");
    setTimeout(() => setMsgs((m) => ({ ...m, [scopeId]: [...(m[scopeId] ?? []), reply] })), 500);
  };

  const groups = (["global", "channel", "thread", "agent", "job"] as const).map((k) => [k, scopes.filter((s) => s.kind === k)] as const);

  return (
    <div className="-m-6 flex h-[calc(100%+3rem)] flex-col">
      <div className="flex items-center gap-3 border-b border-zinc-800 px-6 py-3">
        <div className="relative">
          <button onClick={() => setOpen(!open)} className="flex items-center gap-2 rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 hover:bg-zinc-800">
            {(() => {
              const I = KIND_ICON[scope.kind];
              return <I className="h-4 w-4 text-zinc-400" />;
            })()}
            <span className="font-medium text-zinc-100">{scopeLabel(scope)}</span>
            <ChevronDown className="h-4 w-4 text-zinc-500" />
          </button>
          {open && (
            <div className="absolute left-0 top-full z-20 mt-1 w-72 rounded-lg border border-zinc-700 bg-zinc-900 p-1 shadow-2xl">
              {groups.map(([k, items]) => (
                <div key={k} className="py-1">
                  <div className="px-2 py-1 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">{KIND_LABEL[k]}</div>
                  {items.map((s) => {
                    const I = KIND_ICON[s.kind];
                    const bad = launcherStatus(resolveLauncher(s.id).launcher) === "refused";
                    return (
                      <button
                        key={s.id}
                        onClick={() => {
                          setScopeId(s.id);
                          setOpen(false);
                        }}
                        className={cx("flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-zinc-800", s.id === scopeId && "bg-zinc-800")}
                      >
                        <I className="h-3.5 w-3.5 text-zinc-500" />
                        <span className="flex-1 text-zinc-200">{scopeLabel(s)}</span>
                        {bad && <CircleAlert className="h-3.5 w-3.5 text-red-400" />}
                      </button>
                    );
                  })}
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="flex items-center gap-2 text-xs text-zinc-400">
          <span>
            Launcher <code className={cx(refused ? "text-red-400" : "text-zinc-200")}>{launcher}</code> <span className="text-zinc-500">({from})</span>
          </span>
          <span className="text-zinc-600">·</span>
          {session ? (
            <button className="hover:text-zinc-200" onClick={() => go("sessions")}>
              resumes session <code>{session.id}</code> ({session.turns} turns)
            </button>
          ) : (
            <span>new session on first message</span>
          )}
        </div>
        <span className="ml-auto text-[11px] text-zinc-500">Replies stay in the app; nothing is mirrored to Discord.</span>
      </div>

      <div className="flex-1 space-y-4 overflow-y-auto px-6 py-5">
        {list.length === 0 && <p className="mt-10 text-center text-zinc-500">No messages from the app in this scope yet.</p>}
        {list.map((m, i) => (
          <div key={i} className={cx("flex", m.role === "user" ? "justify-end" : "justify-start")}>
            <div
              className={cx(
                "max-w-[70%] rounded-lg px-3.5 py-2.5",
                m.role === "user" && "bg-amber-500/15 text-zinc-100",
                m.role === "assistant" && "border border-zinc-800 bg-zinc-900 text-zinc-200",
                m.role === "system" && "border border-red-500/40 bg-red-500/10 text-red-200",
              )}
            >
              {m.error && <CircleAlert className="mb-1 h-4 w-4" />}
              <p className="whitespace-pre-wrap">{m.text}</p>
              {m.attachments && (
                <div className="mt-2 flex gap-1">
                  {m.attachments.map((a) => (
                    <Badge key={a}>
                      <Paperclip className="h-3 w-3" /> {a}
                    </Badge>
                  ))}
                </div>
              )}
              {m.tools && (
                <div className="mt-2 flex gap-1">
                  {m.tools.map((t) => (
                    <Badge key={t} tone="blue">
                      <Wrench className="h-3 w-3" /> {t}
                    </Badge>
                  ))}
                </div>
              )}
            </div>
          </div>
        ))}
        <div ref={end} />
      </div>

      {refused && (
        <div className="mx-6 mb-2 flex items-center gap-2 rounded-md border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-200">
          <CircleAlert className="h-4 w-4" /> Messages in this scope will be refused until its launcher is fixed.
          <Button size="sm" className="ml-auto" onClick={() => go("launchers")}>
            Fix in Launchers
          </Button>
        </div>
      )}
      <div className="flex items-end gap-2 border-t border-zinc-800 px-6 py-3">
        <Button variant="ghost" title="Attach files (up to 5, 10 MB each)">
          <Paperclip className="h-4 w-4" />
        </Button>
        <textarea
          className={cx(inputCls, "h-10 resize-none")}
          placeholder={`Message ${scopeLabel(scope)}…`}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
        />
        <Button variant="primary" onClick={send}>
          <Send className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
}

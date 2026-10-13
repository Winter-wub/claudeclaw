import { Bot, Code, MessageSquare, Plus } from "lucide-react";
import { useState } from "react";
import { agents, jobs } from "../mock";
import { Badge, Button, Card, PageHeader, cx, useApp } from "../ui";

export function AgentsScreen() {
  const { go } = useApp();
  const [sel, setSel] = useState(agents[0].name);
  const a = agents.find((x) => x.name === sel)!;
  const agentJobs = jobs.filter((j) => j.agent === a.name);
  return (
    <>
      <PageHeader
        title="Agents"
        subtitle="Each agent has its own folder, persona and session, and can own jobs."
        actions={
          <Button variant="primary">
            <Plus className="h-3.5 w-3.5" /> New agent
          </Button>
        }
      />
      <div className="grid grid-cols-[280px_1fr] gap-4">
        <Card>
          {agents.map((x) => (
            <button
              key={x.name}
              onClick={() => setSel(x.name)}
              className={cx("flex w-full items-start gap-2 rounded-md px-2 py-2 text-left", sel === x.name ? "bg-zinc-800" : "hover:bg-zinc-800/50")}
            >
              <Bot className="mt-0.5 h-4 w-4 text-violet-400" />
              <span>
                <span className="block text-zinc-100">{x.name}</span>
                <span className="block text-[11px] text-zinc-500">
                  {x.jobs} jobs · {x.threads} threads
                </span>
              </span>
            </button>
          ))}
        </Card>
        <div className="space-y-4">
          <Card
            title={a.name}
            actions={
              <>
                <Button size="sm" onClick={() => go("chat", `ag-${a.name}`)}>
                  <MessageSquare className="h-3.5 w-3.5" /> Chat
                </Button>
                <Button size="sm">
                  <Code className="h-3.5 w-3.5" /> Edit persona (raw)
                </Button>
              </>
            }
          >
            <p className="text-zinc-300">{a.description}</p>
            <dl className="mt-3 grid grid-cols-3 gap-4 text-xs">
              <div>
                <dt className="text-zinc-500">Folder</dt>
                <dd className="font-mono text-zinc-300">agents/{a.name}/</dd>
              </div>
              <div>
                <dt className="text-zinc-500">Launcher</dt>
                <dd className="font-mono text-zinc-300">{a.launcher}</dd>
              </div>
              <div>
                <dt className="text-zinc-500">Session</dt>
                <dd className="font-mono text-zinc-300">{a.session}</dd>
              </div>
            </dl>
            <pre className="mt-3 max-h-40 overflow-y-auto rounded-md border border-zinc-800 bg-zinc-950 p-3 font-mono text-[12px] text-zinc-400">
              {`# ${a.name}\n\nYou are ${a.name}. ${a.description}\nKeep answers short. Cite sources as links.\n`}
            </pre>
          </Card>
          <Card
            title="Jobs"
            actions={
              <Button size="sm" onClick={() => go("job", "new")}>
                <Plus className="h-3.5 w-3.5" /> Add job
              </Button>
            }
          >
            {agentJobs.length === 0 && <p className="text-zinc-500">No jobs for this agent.</p>}
            {agentJobs.map((j) => (
              <button key={j.name} onClick={() => go("job", j.name)} className="flex w-full items-center gap-3 rounded-md px-2 py-1.5 text-left hover:bg-zinc-800/50">
                <span className="flex-1 text-zinc-100">{j.name}</span>
                <code className="text-xs text-zinc-500">{j.schedule}</code>
                <Badge tone="green">{j.lastResult}</Badge>
              </button>
            ))}
          </Card>
        </div>
      </div>
    </>
  );
}

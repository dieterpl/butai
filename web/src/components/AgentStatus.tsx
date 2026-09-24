import { useEffect, useState } from "react";
import type { AgentDto } from "@/protocol/generated/protocol";
import type { QualifiedAgent } from "@/logic/events";
import { agentMark } from "@/pages/parts";

/** Repaint only the moving token, without redrawing the fleet or its pane. */
export function AgentStatus({ agent, sprite = false }: { agent: AgentDto | QualifiedAgent; sprite?: boolean }) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    if (agent.state !== "working" || agent.exited != null) return;
    const clock = setInterval(() => setNow(Date.now()), 200);
    return () => clearInterval(clock);
  }, [agent.state, agent.exited]);
  const mark = agentMark(agent, agent.state === "working" ? now : Date.now());
  return <span className="whitespace-pre">{sprite ? mark.glyph : mark.short}</span>;
}

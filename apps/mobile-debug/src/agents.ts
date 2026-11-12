// The agents the drawer keeps apart: each family (Codex, DSH) lists its own conversations,
// projects and phone sessions, and a new chat goes to the family chosen. Sessions with any
// other agent are "其他". No React Native imports, so vitest runs it.
import type { AgentInfo, SessionInfo } from "@tether/client";
import { pickCodex } from "./history";

export type FamilyKey = "codex" | "dsh" | "other";

/** A family as the drawer shows it; `agent` is the one new chats go to (none for "其他"). */
export interface Family {
  key: FamilyKey;
  label: string;
  agent?: string;
}

/** The family of an agent id; without the computer's list, guessed from the id. */
export function familyOf(agent: string, agents: AgentInfo[]): FamilyKey {
  const kind = agents.find(a => a.id === agent)?.kind;
  if (kind === "codex" || (!kind && agent.startsWith("codex"))) return "codex";
  if (kind === "dsh" || (!kind && agent === "dsh")) return "dsh";
  return "other";
}

/** The families this computer offers: Codex and DSH when there, "其他" when sessions need it. */
export function families(agents: AgentInfo[], sessions: SessionInfo[]): Family[] {
  const out: Family[] = [];
  const codex = pickCodex(agents);
  if (codex) out.push({ key: "codex", label: "Codex", agent: codex.id });
  const dsh = agents.find(a => a.kind === "dsh" && a.chat && a.available);
  if (dsh) out.push({ key: "dsh", label: "DSH", agent: dsh.id });
  if (sessions.some(s => s.state !== "exited" && familyOf(s.agent, agents) === "other")) out.push({ key: "other", label: "其他" });
  return out;
}

/** How texts name the agent: "Codex", "DSH", else its own name. */
export function agentLabel(agent: string | undefined, agents: AgentInfo[]): string {
  if (!agent) return "agent";
  const family = familyOf(agent, agents);
  if (family === "codex") return "Codex";
  if (family === "dsh") return "DSH";
  return agents.find(a => a.id === agent)?.name ?? agent;
}

/** The sessions that belong under `family`. */
export function sessionsOf(family: FamilyKey | undefined, sessions: SessionInfo[], agents: AgentInfo[]): SessionInfo[] {
  if (!family) return sessions;
  return sessions.filter(s => familyOf(s.agent, agents) === family);
}

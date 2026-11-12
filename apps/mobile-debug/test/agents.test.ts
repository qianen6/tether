import { describe, expect, it } from "vitest";
import type { AgentInfo, SessionInfo } from "@tether/client";
import { agentLabel, families, familyOf, sessionsOf } from "../src/agents";

const agent = (id: string, kind: AgentInfo["kind"], available = true, chat = true): AgentInfo => ({
  id, name: id === "dsh" ? "DeepSeek Harness" : id, kind, available, detail: null, chat, terminal_command: [id],
});
const session = (id: string, agentId: string, state: SessionInfo["state"] = "idle"): SessionInfo => ({
  id, agent: agentId, title: id, cwd: "C:\\w", state, status_line: null, created_at: 1, updated_at: 2,
  last_seq: 0, chat: false, model: null, effort: null, speed: null, thread: null,
});

const agents = [agent("codex", "codex"), agent("codex-desktop", "codex"), agent("dsh", "dsh"), agent("claude", "claude"), agent("shell", "terminal", true, false)];

describe("families", () => {
  it("keeps Codex and DSH apart, and others in their own place", () => {
    expect(families(agents, [])).toEqual([
      { key: "codex", label: "Codex", agent: "codex-desktop" },
      { key: "dsh", label: "DSH", agent: "dsh" },
    ]);
    const withClaude = families(agents, [session("s1", "claude")]);
    expect(withClaude.map(f => f.key)).toEqual(["codex", "dsh", "other"]);
    // A closed one does not make "其他" appear.
    expect(families(agents, [session("s1", "claude", "exited")]).map(f => f.key)).toEqual(["codex", "dsh"]);
  });

  it("leaves out a DSH that is not installed", () => {
    expect(families([agent("codex", "codex"), agent("dsh", "dsh", false)], []).map(f => f.key)).toEqual(["codex"]);
  });

  it("files sessions by family, both Codexes together", () => {
    const sessions = [session("a", "codex"), session("b", "codex-desktop"), session("c", "dsh"), session("d", "claude")];
    expect(sessionsOf("codex", sessions, agents).map(s => s.id)).toEqual(["a", "b"]);
    expect(sessionsOf("dsh", sessions, agents).map(s => s.id)).toEqual(["c"]);
    expect(sessionsOf("other", sessions, agents).map(s => s.id)).toEqual(["d"]);
    expect(sessionsOf(undefined, sessions, agents)).toHaveLength(4);
  });

  it("guesses from the id before the computer's list arrives", () => {
    expect(familyOf("codex-desktop", [])).toBe("codex");
    expect(familyOf("dsh", [])).toBe("dsh");
    expect(familyOf("my-acp", [])).toBe("other");
  });

  it("names the agent in texts", () => {
    expect(agentLabel("codex-desktop", agents)).toBe("Codex");
    expect(agentLabel("dsh", agents)).toBe("DSH");
    expect(agentLabel("claude", agents)).toBe("claude");
    expect(agentLabel(undefined, agents)).toBe("agent");
  });
});

import { describe, expect, it } from "vitest";
import type { HistoryThread, SessionInfo } from "@tether/client";
import { entries, folderKey, folderName, groups, hiddenSessions, matches, pairSessions, projectOf } from "../src/entries";

const thread = (id: string, cwd: string, created: number, chat = false, project: string | null = null): HistoryThread => ({
  id, title: `t ${id}`, preview: `p ${id}`, cwd, origin: "vscode", created_at: created, updated_at: created + 1, chat, project,
});
const session = (id: string, cwd: string, created: number, agent = "codex", chat = false): SessionInfo => ({
  id, agent, title: `s ${id}`, cwd, state: "idle", status_line: null, created_at: created, updated_at: created + 5,
  last_seq: 0, chat, model: null, effort: null, speed: null, thread: null,
});

describe("folders", () => {
  it("compares folders however they are written", () => {
    expect(folderKey("C:\\Work\\App\\")).toBe(folderKey("c:/work/app"));
    expect(folderKey("\\\\?\\C:\\Work")).toBe("c:/work");
    expect(folderName("C:\\Users\\me\\Documents\\Codex\\2026-11-09\\hello")).toBe("hello");
    expect(folderName("/home/me/app/")).toBe("app");
  });
});

describe("pairing", () => {
  it("prefers a saved link, then the conversation a new session began", () => {
    const threads = [thread("a", "C:\\chat\\one", 1_000, true), thread("b", "C:\\proj", 50_000), thread("c", "C:\\proj", 90_000)];
    const sessions = [session("s1", "c:/chat/one", 2_000, "codex", true), session("s2", "C:\\proj", 80_000), session("s3", "C:\\proj", 10)];
    const paired = pairSessions(threads, sessions, { s3: "b" }, "codex");
    expect(paired.get("s3")).toBe("b");
    expect(paired.get("s1")).toBe("a"); // begun within the slack before the session
    expect(paired.get("s2")).toBe("c"); // b is taken, and older than s2
  });

  it("trusts the conversation the computer names over links and guesses", () => {
    const threads = [thread("a", "C:\\proj", 1_000), thread("b", "C:\\proj", 2_000)];
    const s1 = { ...session("s1", "C:\\proj", 1_500), thread: "b" };
    const s2 = session("s2", "C:\\proj", 900);
    const paired = pairSessions(threads, [s1, s2], { s2: "b" }, "codex");
    expect(paired.get("s1")).toBe("b");
    expect(paired.get("s2")).toBe("a"); // its link is taken; the folder guess finds a
  });

  it("leaves other agents and older conversations alone", () => {
    const threads = [thread("a", "C:\\proj", 1_000)];
    expect(pairSessions(threads, [session("s", "C:\\proj", 100_000)], {}, "codex").size).toBe(0);
    expect(pairSessions(threads, [session("s", "C:\\proj", 900, "claude")], {}, "codex").size).toBe(0);
  });
});

describe("entries", () => {
  it("shows a paired session as its conversation, and closed sessions not at all", () => {
    const threads = [thread("a", "C:\\proj", 1_000)];
    const live = session("s1", "C:\\proj", 900);
    const gone = { ...session("s2", "C:\\other", 5), state: "exited" as const };
    const other = session("s3", "C:\\other", 9_000, "claude");
    const list = entries(threads, [live, gone, other], new Map([["s1", "a"]]));
    expect(list.map(e => e.key)).toEqual(["s:s3", "t:a"]);
    expect(list[1]!.session?.id).toBe("s1");
  });

  it("groups project work by folder, newest folder first, chats apart", () => {
    const list = entries(
      [thread("a", "C:\\one", 1_000), thread("b", "c:/one/", 5_000), thread("c", "C:\\two", 3_000), thread("d", "C:\\chat", 9_000, true)],
      [],
      new Map(),
    );
    const g = groups(list);
    expect(g.map(x => x.label)).toEqual(["one", "two"]);
    expect(g[0]!.entries.map(e => e.thread?.id)).toEqual(["b", "a"]);
  });

  it("groups by the computer's projects, which may hold several folders", () => {
    const projects = [
      { id: "lib", name: "图书馆", roots: ["C:\\图书馆", "C:\\周三会议"] },
      { id: "club", name: "社团", roots: ["C:\\爱极客AIGREEK"] },
      { id: "club2", name: "社团", roots: ["C:\\社团"] },
      { id: "deep", name: "论文", roots: ["C:\\图书馆\\论文"] },
    ];
    const list = entries(
      [
        thread("a", "C:\\爱极客AIGREEK", 9_000),
        thread("b", "C:\\周三会议", 8_000),
        thread("c", "c:/图书馆/ppt", 7_000),
        thread("d", "C:\\图书馆\\论文\\x", 6_000),
        thread("e", "C:\\elsewhere", 5_000, false, "club2"),
        thread("f", "C:\\loose", 4_000),
      ],
      [session("s", "C:\\图书馆", 10_000)],
      new Map(),
    );
    const g = groups(list, projects);
    expect(g.map(x => [x.label, x.entries.map(e => e.key)])).toEqual([
      ["图书馆", ["s:s", "t:b", "t:c"]],
      ["社团", ["t:a"]],
      ["论文", ["t:d"]],
      ["社团", ["t:e"]],
      ["loose", ["t:f"]],
    ]);
    expect(g[0]!.cwd).toBe("C:\\图书馆");
    expect(g[3]!.project?.id).toBe("club2");
    expect(projectOf(list.find(e => e.key === "t:f")!, projects)).toBeUndefined();
  });

  it("finds by title, preview or folder", () => {
    const [e] = entries([thread("a", "C:\\Projects\\Shop", 1)], [], new Map());
    expect(matches(e!, "shop")).toBe(true);
    expect(matches(e!, "p a")).toBe(true);
    expect(matches(e!, "nothing")).toBe(false);
  });
});

describe("sessions the computer no longer lists", () => {
  const on = (id: string, t: string, updated: number, state: SessionInfo["state"] = "idle"): SessionInfo => ({
    ...session(id, "C:\proj", updated - 5), thread: t, updated_at: updated, state,
  });
  const listed = [thread("a", "C:\proj", 100_000), thread("b", "C:\proj", 200_000)];
  const now = 1_000_000;

  it("hides a session on an archived conversation, or one under another provider", () => {
    expect([...hiddenSessions(listed, [on("s1", "gone", 300_000), on("s2", "a", 300_000)], "codex", now, false)]).toEqual(["s1"]);
  });
  it("keeps sessions at work, waiting for you, or just begun, and other agents'", () => {
    const sessions = [
      on("w", "gone", 300_000, "working"),
      on("n", "gone", 300_000, "needs_you"),
      on("new", "gone", now - 1_000),
      { ...on("o", "gone", 300_000), agent: "claude" },
    ];
    expect(hiddenSessions(listed, sessions, "codex", now, false).size).toBe(0);
  });
  it("keeps a session older than the page unless the page is the whole list", () => {
    const old = [on("s", "gone", 50_000)];
    expect(hiddenSessions(listed, old, "codex", now, false).size).toBe(0);
    expect([...hiddenSessions(listed, old, "codex", now, true)]).toEqual(["s"]);
  });
  it("hides nothing before the list arrived, or for a search", () => {
    expect(hiddenSessions(listed, [on("s", "gone", 300_000)], "codex", 0, true).size).toBe(0);
    expect(hiddenSessions([], [on("s", "gone", 300_000)], "codex", now, true).size).toBe(0);
  });
});

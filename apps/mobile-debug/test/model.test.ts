import { describe, expect, it } from "vitest";
import type { Event, SessionInfo } from "@tether/client";
import { applyEvent, emptyChat, finished, foldSteps, mergeSession, pendingApprovals, readableError, sortSessions, stepsLabel, type Chat, type ChatItem } from "../src/model";
import { fromBase64, toBase64 } from "../src/base64";
import { withMods } from "../src/keys";

const run = (events: Event[], chat: Chat = emptyChat) => events.reduce(applyEvent, chat);
const approval = { id: "p1", kind: "command" as const, title: "运行：ls", detail: "ls -la", cwd: null, choices: ["allow_once" as const, "deny" as const] };

describe("errors", () => {
  it("keeps an explanation before the provider's message", () => {
    const hint = "电脑上的 Codex 现在登录的是 ChatGPT 账号，不支持这个模型。请在手机上换一个模型。";
    expect(readableError(`${hint}\n{"error":{"message":"not supported"}}`)).toBe(`${hint}\nnot supported`);
    expect(readableError(`status 400: {"detail":"bad"}`)).toBe("bad（status 400）");
  });
});

describe("chat", () => {
  it("streams deltas then replaces them with the full text", () => {
    const c = run([
      { seq: 1, at: 0, e: "user_text", text: "hi" },
      { seq: 1, at: 0, e: "assistant_delta", item: "a", text: "ec" },
      { seq: 1, at: 0, e: "assistant_delta", item: "a", text: "ho" },
    ]);
    expect(c.items[1]).toMatchObject({ kind: "assistant", text: "echo", done: false });
    const done = run([{ seq: 2, at: 0, e: "assistant_text", item: "a", text: "echo: hi" }], c);
    expect(done.items).toHaveLength(2);
    expect(done.items[1]).toMatchObject({ text: "echo: hi", done: true });
    // A late delta does not touch finished text.
    const late = run([{ seq: 2, at: 0, e: "assistant_delta", item: "a", text: "x" }], done);
    expect(late.items[1]).toMatchObject({ text: "echo: hi" });
  });

  it("keeps the pictures a message was sent with", () => {
    const c = run([
      { seq: 1, at: 0, e: "user_text", text: "看图", images: ["C:/T/a.png"] },
      { seq: 2, at: 0, e: "user_text", text: "没图", images: null },
    ]);
    expect(c.items[0]).toMatchObject({ kind: "user", images: ["C:/T/a.png"] });
    expect(c.items[1]).not.toHaveProperty("images");
  });

  it("ignores replayed durable events", () => {
    const events: Event[] = [
      { seq: 1, at: 0, e: "user_text", text: "a" },
      { seq: 2, at: 0, e: "turn_done", summary: null },
    ];
    const once = run(events);
    expect(run(events, once)).toBe(once);
    expect(once.seq).toBe(2);
  });

  it("pairs tools and tracks approvals and state", () => {
    const c = run([
      { seq: 1, at: 0, e: "tool_start", item: "t", tool: "shell", summary: "ls" },
      { seq: 2, at: 0, e: "approval", approval },
      { seq: 3, at: 0, e: "state", state: "needs_you", status_line: "等审批" },
    ]);
    expect(pendingApprovals(c).map(a => a.id)).toEqual(["p1"]);
    expect(c.state).toBe("needs_you");
    const d = run([
      { seq: 4, at: 0, e: "approval_done", approval: "p1", decision: "allow_once" },
      { seq: 5, at: 0, e: "tool_end", item: "t", ok: true, output: "a.txt" },
      { seq: 6, at: 0, e: "state", state: "idle", status_line: null },
    ], c);
    expect(pendingApprovals(d)).toEqual([]);
    expect(d.items[0]).toMatchObject({ kind: "tool", tool: "shell", ok: true, output: "a.txt" });
    expect(d.state).toBe("idle");
  });
});

describe("sessions", () => {
  const s = (id: string, state: SessionInfo["state"], updated_at: number): SessionInfo =>
    ({ id, agent: "echo", title: id, cwd: "/", state, status_line: null, created_at: 0, updated_at, last_seq: 0, chat: false });

  it("puts sessions waiting on you first", () => {
    const sorted = sortSessions([s("a", "idle", 9), s("b", "working", 1), s("c", "needs_you", 0), s("d", "idle", 10)]);
    expect(sorted.map(x => x.id)).toEqual(["c", "b", "d", "a"]);
  });

  it("merges updates by id", () => {
    const list = mergeSession([s("a", "idle", 1)], s("a", "working", 2));
    expect(list).toHaveLength(1);
    expect(list[0]!.state).toBe("working");
    expect(mergeSession(list, s("b", "idle", 3))).toHaveLength(2);
  });
});

describe("notices", () => {
  it("shows the message inside a JSON error body", () => {
    expect(readableError('unexpected status 400 Bad Request: {"detail":"The model is not supported"}'))
      .toBe("The model is not supported（unexpected status 400 Bad Request）");
    expect(readableError('{"error":{"message":"quota","type":"x"}}')).toBe("quota");
    expect(readableError("plain {not json")).toBe("plain {not json");
  });

  it("collapses the same notice repeated", () => {
    const n = (seq: number, text: string): Event => ({ seq, at: 0, e: "notice", level: "error", text });
    const c = run([n(1, "boom"), n(2, "boom"), n(3, "other"), n(4, "boom")]);
    expect(c.items.map(x => (x.kind === "notice" ? x.text : ""))).toEqual(["boom", "other", "boom"]);
    expect(c.seq).toBe(4);
  });

  it("marks a turn that ended as done", () => {
    expect(finished("working", "idle")).toBe(true);
    expect(finished("idle", "idle")).toBe(false);
    expect(finished(undefined, "idle")).toBe(false);
  });
});

describe("base64", () => {
  it("round-trips arbitrary bytes", () => {
    const bytes = new Uint8Array(70_000).map((_, i) => (i * 7) & 255);
    expect(fromBase64(toBase64(bytes))).toEqual(bytes);
    expect(toBase64(new TextEncoder().encode("中"))).toBe("5Lit");
  });
});

describe("keys", () => {
  const none = { ctrl: false, shift: false };
  it("turns a letter typed with ctrl into its control character", () => {
    expect(withMods("c", { ctrl: true, shift: false })).toBe("\x03");
    expect(withMods("[", { ctrl: true, shift: false })).toBe("\x1b");
    expect(withMods(" ", { ctrl: true, shift: false })).toBe("\x00");
    expect(withMods("12", { ctrl: true, shift: false })).toBe("12");
  });

  it("capitalises a letter typed with shift and leaves the rest", () => {
    expect(withMods("a", { ctrl: false, shift: true })).toBe("A");
    expect(withMods("中", none)).toBe("中");
    expect(withMods("\r", none)).toBe("\r");
  });
});

describe("steps", () => {
  const items: ChatItem[] = [
    { kind: "user", key: "u1", text: "你是什么模型" },
    { kind: "tool", key: "t1", tool: "shell", summary: "codex-runtime", ok: true },
    { kind: "thinking", key: "th1", text: "**正在核对**" },
    { kind: "assistant", key: "a1", text: "我是 Codex", done: true },
    { kind: "thinking", key: "th2", text: "再想想" },
  ];

  it("folds each run of thinking and tools into one line, the last one live while working", () => {
    const shown = foldSteps(items, true);
    expect(shown.map(x => x.kind)).toEqual(["user", "steps", "assistant", "steps"]);
    expect(shown[1]).toMatchObject({ key: "s:t1", active: false });
    expect(shown[3]).toMatchObject({ key: "s:th2", active: true });
    expect(foldSteps(items, false).some(x => x.kind === "steps" && x.active)).toBe(false);
  });

  it("says what the steps were", () => {
    const shown = foldSteps(items, false);
    expect(shown[1]?.kind === "steps" && stepsLabel(shown[1].steps)).toBe("思考 · 运行 1 条命令");
    expect(stepsLabel([{ kind: "tool", key: "x", tool: "edit", summary: "a.ts" }, { kind: "tool", key: "y", tool: "mcp", summary: "" }])).toBe("改了 1 个文件 · 1 个其他步骤");
  });
});

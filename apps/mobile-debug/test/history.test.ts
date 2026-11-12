import { describe, expect, it } from "vitest";
import type { AgentInfo, HistoryReadData, HistoryThread, ModelInfo } from "@tether/client";
import { chatTitle, effortWord, HistoryCache, KEEP_THREADS, memoryKV, modelParts, pickCodex, preludeItems, resolvePick, speedDetail, speedWord, NO_PERMISSIONS, permissionOf } from "../src/history";

const thread = (id: string, chat = false, updated = 1): HistoryThread => ({
  id, title: `t ${id}`, preview: "", cwd: "/w", origin: "Codex Desktop", created_at: 1, updated_at: updated, chat,
});
const data = (id: string): HistoryReadData => ({
  thread: thread(id),
  events: [{ seq: 1, at: 1, e: "user_text", text: `hi ${id}` }],
  truncated: false,
});

describe("history cache", () => {
  it("keeps lists, threads and models, and survives a new instance", async () => {
    const kv = memoryKV();
    const cache = new HistoryCache(kv);
    expect(await cache.threads("codex")).toBeUndefined();
    await cache.saveThreads("codex", [thread("a")]);
    await cache.saveThread("codex", data("a"));
    await cache.saveModels("codex", [{ id: "m", name: "m", efforts: [], is_default: true, speeds: [] }]);
    const again = new HistoryCache(kv);
    expect(await again.threads("codex")).toEqual([thread("a")]);
    expect(await again.thread("codex", "a")).toEqual(data("a"));
    expect((await again.models("codex"))?.[0]?.id).toBe("m");
    expect(await again.codex()).toBeUndefined();
    await again.saveCodex("codex-desktop");
    expect(await new HistoryCache(kv).codex()).toBe("codex-desktop");
  });

  it("remembers which agent's work the drawer showed", async () => {
    const kv = memoryKV();
    expect(await new HistoryCache(kv).families()).toEqual({ list: [] });
    const list = [{ key: "codex" as const, label: "Codex", agent: "codex-desktop" }, { key: "dsh" as const, label: "DSH", agent: "dsh" }];
    await new HistoryCache(kv).saveFamilies(list, "dsh");
    expect(await new HistoryCache(kv).families()).toEqual({ list, current: "dsh" });
  });

  it("forgets the least recently saved threads past the limit", async () => {
    const kv = memoryKV();
    const cache = new HistoryCache(kv);
    for (let i = 0; i <= KEEP_THREADS; i++) await cache.saveThread("codex", data(`t${i}`));
    await cache.saveThread("codex", data("t0")); // t0 is fresh again; t1 goes
    expect(await cache.thread("codex", "t0")).toBeDefined();
    expect(await cache.thread("codex", "t1")).toBeUndefined();
    expect(await cache.thread("codex", "t2")).toBeDefined();
  });

  it("links sessions to conversations and prunes the gone ones", async () => {
    const cache = new HistoryCache(memoryKV());
    await cache.link("s1", { agent: "codex", thread: thread("a") });
    await cache.link("s2", { agent: "codex", thread: thread("b") });
    expect(await cache.sessionOf("codex", "a", ["s1", "s2"])).toBe("s1");
    expect(await cache.sessionOf("codex", "a", ["s2"])).toBeUndefined();
    expect(await cache.links()).toEqual({ s1: "a", s2: "b" });
    await cache.prune(["s2"]);
    expect(await cache.continued("s1")).toBeUndefined();
    expect((await cache.continued("s2"))?.thread.id).toBe("b");
  });

  it("treats a broken file as missing", async () => {
    const kv = memoryKV();
    kv.data.set("list/codex", "{not json");
    expect(await new HistoryCache(kv).threads("codex")).toBeUndefined();
  });
});

describe("views", () => {
  it("renders saved events with keys apart from live ones", () => {
    const items = preludeItems([
      { seq: 1, at: 1, e: "user_text", text: "a" },
      { seq: 2, at: 1, e: "assistant_text", item: "m", text: "b" },
      { seq: 3, at: 1, e: "turn_done", summary: null },
    ]);
    expect(items.map(x => x.key)).toEqual(["h:u1", "h:a:m", "h:d3"]);
  });

  it("prefers the desktop app's Codex", () => {
    const a = (id: string, available = true, kind: AgentInfo["kind"] = "codex"): AgentInfo => ({
      id, name: id, kind, available, chat: true, terminal_command: [],
    });
    expect(pickCodex([a("codex"), a("codex-desktop")])?.id).toBe("codex-desktop");
    expect(pickCodex([a("codex"), a("codex-desktop", false)])?.id).toBe("codex");
    expect(pickCodex([a("claude", true, "claude")])).toBeUndefined();
  });

  it("labels the model, effort and speed", () => {
    const fast = { id: "priority", name: "Fast", description: "2x speed" };
    const models: ModelInfo[] = [
      { id: "sol", name: "GPT-6.1-Sol", description: null, efforts: ["low", "xhigh"], default_effort: "max", is_default: true, speeds: [fast], default_speed: "priority" },
      { id: "luna", name: "GPT-6-Luna", description: null, efforts: ["low"], default_effort: null, is_default: false, speeds: [], default_speed: null },
    ];
    const none = { model: null, effort: null, speed: null };
    expect(modelParts(none, models)).toMatchObject({ model: "GPT-6.1-Sol", effort: "Max" });
    expect(modelParts(none, models).fast).toBe(true);
    expect(modelParts({ ...none, speed: "default" }, models).fast).toBe(false);
    expect(modelParts({ model: "x", effort: "xhigh", speed: null }, models)).toMatchObject({ model: "x", effort: "极高" });
    expect(modelParts({ ...none, model: "luna" }, models)).toMatchObject({ model: "GPT-6-Luna", effort: undefined });
    expect(modelParts({ ...none, model: "luna" }, models).fast).toBe(false);
    expect(modelParts(none, []).model).toBe("默认模型");
    expect(resolvePick({ ...none, effort: "low" }, models)).toMatchObject({ id: "sol", effort: "low", speed: "priority" });
    expect(speedWord("priority", models)).toBe("快速");
    expect(speedWord("default", models)).toBe("标准");
    expect(effortWord("off")).toBe("不思考"); // DSH turns thinking off
    expect(speedWord("turbo", models)).toBe("turbo");
    expect(speedDetail("2x speed, increased usage")).toBe("2 倍速度，用量更多");
    expect(speedDetail("1.5x speed")).toBe("1.5 倍速度");
  });

  it("remembers projects", async () => {
    const cache = new HistoryCache(memoryKV());
    await cache.saveProjects("codex", [{ id: "p", name: "社团", roots: ["/a", "/b"] }]);
    expect((await cache.projects("codex"))?.[0]?.roots).toEqual(["/a", "/b"]);
  });

  it("titles a chat by its first line", () => {
    expect(chatTitle("\n  你好  \n第二行")).toBe("你好");
    expect(chatTitle("   ")).toBe("新对话");
    expect([...chatTitle("字".repeat(50))]).toHaveLength(41);
  });
});

describe("permissions", () => {
  const offered = {
    list: [
      { id: ":read-only", name: "只读", description: "先问" },
      { id: ":workspace", name: "默认权限", description: null },
      { id: ":danger-full-access", name: "完全访问", description: "不问" },
    ],
    default: ":danger-full-access",
  };

  it("a new conversation gets what the computer's app uses now, unless chosen", () => {
    expect(permissionOf(offered, null, { chat: true, session: false })).toMatchObject({ id: ":danger-full-access", name: "完全访问" });
    expect(permissionOf(offered, ":read-only", { chat: false, session: false })).toMatchObject({ id: ":read-only", description: "先问" });
  });
  it("a session from before the choice keeps what it was told", () => {
    expect(permissionOf(offered, null, { chat: true, session: true }).id).toBe(":read-only");
    expect(permissionOf(offered, null, { chat: false, session: true }).id).toBe(":workspace");
    expect(permissionOf(offered, ":workspace", { chat: true, session: true }).name).toBe("默认权限");
  });
  it("an older computer offers none: the one that applies is still named", () => {
    expect(permissionOf(NO_PERMISSIONS, null, { chat: true, session: false })).toMatchObject({ id: ":read-only", name: "只读" });
    expect(permissionOf({ list: [], default: null }, "team", { chat: false, session: true }).name).toBe("team");
  });
});

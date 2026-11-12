import { setTimeout as pause } from "node:timers/promises";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WebSocket, WebSocketServer } from "ws";
import { TetherClient, TetherError, type ClientOptions, type ClientState, type WebSocketLike } from "../src/index";
import { decodeBinary, encodeBinary, TERM_INPUT, TERM_OUTPUT } from "../src/binary";
import type { AgentInfo, ClientMsg, Event, HistoryThread, RejectReason, ServerMsg, SessionInfo, TermInfo } from "../src/generated";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const session: SessionInfo = {
  id: "s1", agent: "echo", title: "Test", cwd: "/fixture", state: "idle",
  created_at: 1, updated_at: 1, last_seq: 0, chat: false,
};
const thread: HistoryThread = {
  id: "t1", title: "旧对话", preview: "你好", cwd: "/w", origin: "Codex Desktop", created_at: 1, updated_at: 2, chat: true,
};
const agent: AgentInfo = {
  id: "echo", name: "Echo", kind: "echo", available: true, chat: true, terminal_command: ["echo"],
};
const event = (seq: number): Event => ({ seq, at: seq, e: "assistant_text", item: `i${seq}`, text: `text ${seq}` });
const delta = (text: string, seq = 0): Event => ({ seq, at: 1, e: "assistant_delta", item: "i", text });

async function until(predicate: () => boolean): Promise<void> {
  for (let i = 0; i < 400; i++) {
    if (predicate()) return;
    await pause(5); // Real I/O yielding, deliberately independent of the fake SDK clock.
  }
  throw new Error("Mock server condition did not become true");
}

function storage(initial?: string): ClientOptions["storage"] & { values: Map<string, string> } {
  const values = new Map(initial ? [["token", initial]] : []);
  return {
    values,
    get: vi.fn(async key => values.get(key) ?? null),
    set: vi.fn(async (key, value) => { values.set(key, value); }),
  };
}

class MockHost {
  server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  sockets: WebSocket[] = [];
  frames: { connection: number; message: ClientMsg }[] = [];
  binary: { term: string; data: Uint8Array }[] = [];
  history: Event[] = [];
  subscriptions = new Map<WebSocket, Set<string>>();
  attachments = new Map<WebSocket, Set<string>>();
  terms = new Map<string, TermInfo>();
  pongs = true;
  reject?: RejectReason;
  intercept?: (socket: WebSocket, message: ClientMsg, connection: number) => boolean | void;
  url = "";
  private nextTerm = 0;

  constructor() {
    this.server.on("connection", socket => {
      this.sockets.push(socket);
      const connection = this.sockets.length;
      this.subscriptions.set(socket, new Set());
      this.attachments.set(socket, new Set());
      socket.on("message", (raw, isBinary) => {
        if (isBinary) {
          const frame = decodeBinary(new Uint8Array(raw as Buffer));
          this.binary.push({ term: frame.term, data: frame.data.slice() });
          if (frame.kind === TERM_INPUT && this.attachments.get(socket)?.has(frame.term)) {
            socket.send(encodeBinary(TERM_OUTPUT, frame.term, frame.data));
          }
          return;
        }
        const message = JSON.parse(raw.toString()) as ClientMsg;
        this.frames.push({ connection, message });
        if (this.intercept?.(socket, message, connection)) return;
        switch (message.t) {
          case "pair":
          case "hello":
            if (this.reject) {
              this.push(socket, { t: "rejected", reason: this.reject });
              socket.close(this.reject === "version_mismatch" ? 4426 : 4401);
              break;
            }
            if (message.t === "pair") this.push(socket, { t: "paired", device_id: "device", token: "saved-token" });
            this.push(socket, { t: "welcome", version: 1, host_name: "Mock", device_id: "device", server_time: 1 });
            break;
          case "list_agents": this.ok(socket, message.req, { agents: [agent] }); break;
          case "list_sessions": this.ok(socket, message.req, { sessions: [session] }); break;
          case "create_session": this.ok(socket, message.req, { session: { ...session, agent: message.agent, title: message.title ?? "Test", cwd: message.cwd ?? "/fixture" } }); break;
          case "subscribe":
            this.subscriptions.get(socket)!.add(message.session);
            this.ok(socket, message.req, { session });
            for (const entry of this.history) {
              if (entry.seq > message.after) this.push(socket, { t: "event", session: message.session, event: entry });
            }
            break;
          case "unsubscribe":
            this.subscriptions.get(socket)!.delete(message.session);
            this.ok(socket, message.req, {});
            break;
          case "list_history": this.ok(socket, message.req, { threads: [{ ...thread, title: message.search ?? thread.title }], next_cursor: message.cursor ? null : "c2" }); break;
          case "read_history": this.ok(socket, message.req, { thread: { ...thread, id: message.thread }, events: [event(1)], truncated: false }); break;
          case "list_models": this.ok(socket, message.req, { models: [{ id: "gpt-6.1-sol", name: "gpt-6.1-sol", efforts: ["low"], is_default: true }] }); break;
          case "set_model":
          case "reveal":
          case "send_message":
          case "answer":
          case "interrupt":
          case "close_session": this.ok(socket, message.req, {}); break;
          case "ping": if (this.pongs) this.push(socket, { t: "pong", n: message.n }); break;
          case "term_open": {
            const term: TermInfo = {
              id: `term-${++this.nextTerm}`, command: message.command ?? [message.agent ?? "shell"], cwd: message.cwd ?? "/fixture",
              cols: message.cols, rows: message.rows, running: true, created_at: 1,
            };
            this.terms.set(term.id, term);
            this.attach(socket, message.req, term);
            break;
          }
          case "term_attach": {
            const term = this.terms.get(message.term);
            if (term) this.attach(socket, message.req, term);
            else this.push(socket, { t: "error", req: message.req, code: "not_found", message: "Terminal gone" });
            break;
          }
          case "list_terms": this.ok(socket, message.req, { terms: [...this.terms.values()] }); break;
          case "term_resize": {
            const term = this.terms.get(message.term)!;
            term.cols = message.cols;
            term.rows = message.rows;
            this.ok(socket, message.req, {});
            break;
          }
          case "term_detach":
            this.attachments.get(socket)!.delete(message.term);
            this.ok(socket, message.req, {});
            break;
          case "term_kill":
            this.terms.delete(message.term);
            this.ok(socket, message.req, {});
            this.push(socket, { t: "term_exit", term: message.term });
            break;
        }
      });
    });
  }

  async listen(): Promise<void> {
    await new Promise<void>(resolve => this.server.on("listening", resolve));
    const address = this.server.address();
    if (typeof address !== "object" || !address) throw new Error("Missing mock address");
    this.url = `ws://127.0.0.1:${address.port}/v1/ws`;
  }

  push(socket: WebSocket, message: ServerMsg): void { socket.send(JSON.stringify(message)); }
  ok(socket: WebSocket, req: number, data: unknown): void { socket.send(JSON.stringify({ t: "ok", req, data })); }
  attach(socket: WebSocket, req: number, term: TermInfo): void {
    this.attachments.get(socket)!.add(term.id);
    this.ok(socket, req, { term });
    socket.send(encodeBinary(TERM_OUTPUT, term.id, encoder.encode("历史\u001b[0m")));
  }
  publish(entry: Event): void {
    if (entry.e !== "assistant_delta") this.history.push(entry);
    for (const socket of this.sockets) {
      if (socket.readyState === WebSocket.OPEN && this.subscriptions.get(socket)?.has("s1")) {
        this.push(socket, { t: "event", session: "s1", event: entry });
      }
    }
  }
  messages(t: ClientMsg["t"], connection?: number): ClientMsg[] {
    return this.frames.filter(frame => frame.message.t === t && (connection === undefined || frame.connection === connection)).map(frame => frame.message);
  }
  async close(): Promise<void> {
    for (const socket of this.server.clients) socket.terminate();
    await new Promise<void>(resolve => this.server.close(() => resolve()));
  }
}

let hosts: MockHost[];
let clients: TetherClient[];

beforeEach(() => {
  hosts = [];
  clients = [];
  vi.spyOn(Math, "random").mockReturnValue(0);
});

afterEach(async () => {
  for (const client of clients) client.close();
  for (const host of hosts) await host.close();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

async function setup(options: Partial<ClientOptions> = {}, native = false): Promise<{ host: MockHost; client: TetherClient }> {
  const host = new MockHost();
  hosts.push(host);
  await host.listen();
  const client = new TetherClient({
    url: host.url, deviceName: "Phone", token: "saved-token", storage: storage(),
    ...(!native ? { transport: { connect: url => new WebSocket(url) as unknown as WebSocketLike } } : {}),
    ...options,
  });
  clients.push(client);
  return { host, client };
}

function fakeClock(): void {
  vi.useFakeTimers({ toFake: ["Date", "setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
}

async function dropAndReconnect(host: MockHost, client: TetherClient): Promise<void> {
  const count = host.sockets.length;
  host.sockets.at(-1)!.terminate();
  await until(() => client.state === "reconnecting");
  await vi.advanceTimersByTimeAsync(1_000);
  await until(() => host.sockets.length > count && client.state === "open");
}

describe("connection and pairing", () => {
  it("starts closed, coalesces connect, and uses the default browser-compatible WebSocket", async () => {
    const { host, client } = await setup({}, true);
    expect(client.state).toBe("closed");
    const states: ClientState[] = [];
    const off = client.on("state", state => states.push(state));
    const first = client.connect();
    expect(client.connect()).toBe(first);
    expect(await first).toEqual({ version: 1, host_name: "Mock", device_id: "device", server_time: 1 });
    expect(await client.connect()).toEqual(await first);
    expect(host.messages("hello")).toEqual([{ t: "hello", version: 1, token: "saved-token", device_name: "Phone" }]);
    client.close();
    expect(states).toEqual(["connecting", "open", "closed"]);
    off();
    client.close();
    expect(states).toHaveLength(3);
  });

  it("pairs once, waits for durable token storage, then reconnects with hello", async () => {
    fakeClock();
    const saved = storage();
    let release!: () => void;
    const savedPromise = new Promise<void>(resolve => { release = resolve; });
    saved.set = vi.fn(async (key, value) => { await savedPromise; saved.values.set(key, value); });
    const { host, client } = await setup({ pairSecret: "one-time", token: undefined, storage: saved });
    let connected = false;
    const connecting = client.connect().then(() => { connected = true; });
    await until(() => vi.mocked(saved.set).mock.calls.length === 1);
    expect(connected).toBe(false);
    release();
    await connecting;
    expect(saved.set).toHaveBeenCalledWith("token", "saved-token");
    expect(host.messages("pair")).toEqual([{ t: "pair", version: 1, secret: "one-time", device_name: "Phone" }]);
    expect(host.messages("hello")).toHaveLength(0);
    await dropAndReconnect(host, client);
    expect(host.messages("pair")).toHaveLength(1);
    expect(host.messages("hello", 2)).toMatchObject([{ token: "saved-token" }]);
  });

  it("loads a saved token when no explicit credentials are supplied", async () => {
    const saved = storage("persisted-token");
    const { host, client } = await setup({ token: undefined, storage: saved });
    await client.connect();
    expect(saved.get).toHaveBeenCalledWith("token");
    expect(host.messages("hello")).toMatchObject([{ token: "persisted-token" }]);
  });

  it.each<RejectReason>(["invalid_secret", "invalid_token", "revoked", "version_mismatch", "locked"])("stops reconnecting after rejection: %s", async reason => {
    fakeClock();
    const { host, client } = await setup();
    host.reject = reason;
    await expect(client.connect()).rejects.toMatchObject({ name: "TetherError", code: reason, reason });
    expect(client.state).toBe("closed");
    await vi.advanceTimersByTimeAsync(100_000);
    expect(host.sockets).toHaveLength(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("rejects and cleans up when token storage fails", async () => {
    const saved = storage();
    saved.set = vi.fn(async () => { throw new Error("storage failed"); });
    const { client } = await setup({ token: undefined, pairSecret: "secret", storage: saved });
    await expect(client.connect()).rejects.toThrow("storage failed");
    expect(client.state).toBe("closed");
  });

  it("cancels a pending connect without opening a late socket after storage resolves", async () => {
    fakeClock();
    let release!: (token: string) => void;
    const saved = storage();
    saved.get = () => new Promise(resolve => { release = resolve; });
    const { host, client } = await setup({ token: undefined, storage: saved });
    const result = client.connect();
    const rejected = expect(result).rejects.toMatchObject({ code: "closed" });
    client.close();
    release("saved-token");
    await rejected;
    await vi.advanceTimersByTimeAsync(60_000);
    expect(host.sockets).toHaveLength(0);
    expect(client.state).toBe("closed");
  });

  it("times out a silent handshake and retries", async () => {
    fakeClock();
    const { host, client } = await setup();
    host.intercept = (_socket, message, connection) => connection === 1 && message.t === "hello";
    const connected = client.connect();
    await until(() => host.messages("hello").length === 1);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(client.state).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(1_000);
    await connected;
    expect(host.sockets).toHaveLength(2);
  });

  it.each([4401, 4426])("stops on authentication close code %i even without a rejected frame", async code => {
    fakeClock();
    const { host, client } = await setup();
    host.intercept = (socket, message) => {
      if (message.t !== "hello") return;
      socket.close(code);
      return true;
    };
    await expect(client.connect()).rejects.toMatchObject({ code: code === 4426 ? "version_mismatch" : "unauthorized" });
    await vi.advanceTimersByTimeAsync(60_000);
    expect(host.sockets).toHaveLength(1);
  });

  it("can be explicitly closed and connected again with the same saved token", async () => {
    const { host, client } = await setup();
    await client.connect();
    client.close();
    await client.connect();
    expect(client.state).toBe("open");
    expect(host.messages("hello")).toHaveLength(2);
  });

  it("reports missing credentials without opening a socket", async () => {
    const { host, client } = await setup({ token: undefined });
    await expect(client.connect()).rejects.toMatchObject({ code: "unauthorized" });
    expect(host.sockets).toHaveLength(0);
  });
});

describe("requests and events", () => {
  it("matches concurrent out-of-order replies with increasing req ids and maps every API", async () => {
    const { host, client } = await setup();
    await client.connect();
    const held: { socket: WebSocket; req: number }[] = [];
    host.intercept = (socket, message) => {
      if (message.t !== "list_agents" && message.t !== "list_sessions") return;
      held.push({ socket, req: message.req });
      if (held.length === 2) {
        host.ok(socket, held[1]!.req, { sessions: [session] });
        host.ok(socket, held[0]!.req, { agents: [agent] });
      }
      return true;
    };
    expect(await Promise.all([client.listAgents(), client.listSessions()])).toEqual([[agent], [session]]);
    host.intercept = undefined;
    expect(await client.createSession("echo", { title: "title", cwd: "/work" })).toMatchObject({ title: "title", cwd: "/work", agent: "echo" });
    await client.send("s1", "hello");
    await client.answer("s1", "approval-1", "allow_once");
    await client.interrupt("s1");
    await client.closeSession("s1");
    const requests = host.frames.map(frame => frame.message).filter((message): message is Extract<ClientMsg, { req: number }> => "req" in message);
    expect(requests.map(message => message.req)).toEqual([1, 2, 3, 4, 5, 6, 7]);
    expect(host.messages("send_message")).toMatchObject([{ session: "s1", text: "hello" }]);
    expect(host.messages("answer")).toMatchObject([{ session: "s1", approval: "approval-1", decision: "allow_once" }]);
  });

  it("maps history, models and chat sessions", async () => {
    const { host, client } = await setup();
    await client.connect();
    expect(await client.listHistory("codex")).toEqual({ threads: [thread], next_cursor: "c2" });
    expect(await client.listHistory("codex", { search: "微信", cursor: "c2" })).toMatchObject({ threads: [{ title: "微信" }], next_cursor: null });
    expect(await client.readHistory("codex", "t9")).toMatchObject({ thread: { id: "t9" }, events: [event(1)], truncated: false });
    expect(await client.listModels("codex")).toMatchObject([{ id: "gpt-6.1-sol", is_default: true }]);
    await client.setModel("s1", "gpt-6.1-sol");
    await client.setModel("s1", undefined, "high");
    await client.setModel("s1", undefined, undefined, "priority");
    await client.setModel("s1", undefined, undefined, undefined, ":read-only");
    // An older computer sends no permissions: none to choose.
    expect(await client.listModelChoices("codex")).toMatchObject({ models: [{ id: "gpt-6.1-sol" }], permissions: [], default_permission: null });
    await client.reveal("codex", "t9");
    await client.createSession("codex", { title: "hi", chat: true, model: "gpt-6.1-sol", effort: null, resume: "t1" });
    await client.createSession("codex", { cwd: "C:\lib", project: "p1", roots: ["C:\lib", "C:\meet"], speed: "default", permission: ":workspace", reveal: true });
    expect(host.messages("list_history")).toMatchObject([{ agent: "codex" }, { agent: "codex", search: "微信", cursor: "c2" }]);
    expect(host.messages("set_model")).toEqual([
      expect.objectContaining({ session: "s1", model: "gpt-6.1-sol", effort: null }),
      expect.objectContaining({ session: "s1", model: null, effort: "high", speed: null }),
      expect.objectContaining({ session: "s1", model: null, effort: null, speed: "priority", permission: null }),
      expect.objectContaining({ session: "s1", model: null, effort: null, speed: null, permission: ":read-only" }),
    ]);
    expect(host.messages("reveal")).toMatchObject([{ agent: "codex", thread: "t9" }]);
    expect(host.messages("create_session")).toMatchObject([
      { agent: "codex", title: "hi", chat: true, model: "gpt-6.1-sol", effort: null, resume: "t1" },
      { agent: "codex", project: "p1", roots: ["C:\lib", "C:\meet"], speed: "default", permission: ":workspace", reveal: true },
    ]);
  });

  it.each(["bad_request", "unauthorized", "not_found", "agent_unavailable", "busy", "internal"] as const)("propagates TetherError code %s", async code => {
    const { host, client } = await setup();
    await client.connect();
    host.intercept = (socket, message) => {
      if (!("req" in message)) return;
      host.push(socket, { t: "error", req: message.req, code, message: "detail" });
      return true;
    };
    await expect(client.listAgents()).rejects.toMatchObject({ name: "TetherError", code, message: "detail" });
  });

  it("times out at 30 seconds, ignores late/unmatched replies, and leaves later requests intact", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    host.intercept = (_socket, message) => message.t === "list_agents";
    const response = client.listAgents();
    const rejected = expect(response).rejects.toMatchObject({ code: "timeout" });
    await until(() => host.messages("list_agents").length === 1);
    await vi.advanceTimersByTimeAsync(29_999);
    let settled = false;
    void response.catch(() => { settled = true; });
    await Promise.resolve();
    expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    await rejected;
    const socket = host.sockets[0]!;
    host.ok(socket, 1, { agents: [agent] });
    host.push(socket, { t: "error", req: null, code: "internal", message: "unmatched" });
    host.ok(socket, 999, {});
    host.intercept = undefined;
    expect(await client.listSessions()).toEqual([session]);
  });

  it("rejects pending requests immediately on close and sends no requests when closed", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    host.intercept = (_socket, message) => message.t === "list_agents";
    const pending = client.listAgents();
    const rejected = expect(pending).rejects.toMatchObject({ code: "closed" });
    client.close();
    await rejected;
    await expect(client.listSessions()).rejects.toMatchObject({ code: "closed" });
    await until(() => host.sockets[0]!.readyState === WebSocket.CLOSED);
    await pause(5); // ws owns a close-handshake timeout until its TCP stream closes.
    expect(vi.getTimerCount()).toBe(0);
  });

  it("dispatches push events, isolates consumers, and honors listener removal", async () => {
    const { host, client } = await setup();
    await client.connect();
    const updates = vi.fn();
    const closed = vi.fn();
    const notices = vi.fn();
    client.on("session_update", () => { throw new Error("consumer error"); });
    const off = client.on("session_update", updates);
    client.on("session_closed", closed);
    client.on("notify", notices);
    const socket = host.sockets[0]!;
    host.push(socket, { t: "session_update", session });
    host.push(socket, { t: "session_closed", session: "s1" });
    host.push(socket, { t: "notify", kind: "needs_you", session: null, title: "Approval", body: "Please answer" });
    await until(() => notices.mock.calls.length === 1);
    expect(updates).toHaveBeenCalledWith(session);
    expect(closed).toHaveBeenCalledWith("s1");
    expect(notices).toHaveBeenCalledWith({ kind: "needs_you", session: null, term: undefined, title: "Approval", body: "Please answer" });
    off();
    host.push(socket, { t: "session_update", session });
    await client.listAgents();
    expect(updates).toHaveBeenCalledTimes(1);
    expect(client.state).toBe("open");
  });

  it("rejects oversized outgoing requests and malformed ok payloads without affecting other requests", async () => {
    const { host, client } = await setup();
    await client.connect();
    await expect(client.send("s1", "x".repeat(8 << 20))).rejects.toMatchObject({ code: "bad_request" });
    expect(host.messages("send_message")).toHaveLength(0);
    host.intercept = (socket, message) => {
      if (message.t !== "list_agents") return;
      host.ok(socket, message.req, { agents: null });
      return true;
    };
    await expect(client.listAgents()).rejects.toMatchObject({ code: "bad_response" });
    expect(await client.listSessions()).toEqual([session]);
  });
});

describe("reconnect and replay", () => {
  it("recovers committed events after a mid-send drop without repeating commands or durable seqs", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    host.history = [event(1), event(2)];
    const received: Event[] = [];
    const off = await client.subscribe("s1", entry => received.push(entry));
    await until(() => received.length === 2);
    // These transient seqs must neither be deduplicated nor advance the durable replay cursor.
    host.publish(delta("a", 100));
    host.publish(delta("b", 100));
    await until(() => received.length === 4);
    host.intercept = (socket, message) => {
      if (message.t !== "send_message") return;
      host.history.push(event(3)); // Committed, but neither event nor ok reaches the phone.
      socket.terminate();
      return true;
    };
    await expect(client.send("s1", "commit once")).rejects.toMatchObject({ code: "disconnected" });
    expect(client.state).toBe("reconnecting");
    host.history.push(event(4), event(5));
    host.intercept = (socket, message) => {
      if (message.t !== "subscribe") return;
      host.subscriptions.get(socket)!.add(message.session);
      host.ok(socket, message.req, { session });
      // Include a duplicate at the boundary to exercise client-side deduplication.
      for (const entry of host.history.filter(entry => entry.seq >= message.after)) host.push(socket, { t: "event", session: "s1", event: entry });
      return true;
    };
    await vi.advanceTimersByTimeAsync(1_000);
    await until(() => client.state === "open" && received.filter(entry => entry.e !== "assistant_delta").length === 5);
    host.publish(event(6));
    host.publish(event(6));
    await until(() => received.at(-1)?.seq === 6);
    await client.listAgents();
    expect(received.filter(entry => entry.e !== "assistant_delta").map(entry => entry.seq)).toEqual([1, 2, 3, 4, 5, 6]);
    expect(received.filter(entry => entry.e === "assistant_delta").map(entry => entry.text)).toEqual(["a", "b"]);
    expect(host.messages("subscribe", 2)).toMatchObject([{ after: 2 }]);
    expect(host.messages("send_message")).toHaveLength(1);
    off();
    await until(() => host.messages("unsubscribe").length === 1);
    await dropAndReconnect(host, client);
    expect(host.messages("subscribe", 3)).toHaveLength(0);
  });

  it("supports independent listener cursors, cancellation and an explicit after", async () => {
    fakeClock();
    const { host, client } = await setup();
    host.history = [event(1), event(2), event(3)];
    await client.connect();
    const a: number[] = [];
    const b: number[] = [];
    const offA = await client.subscribe("s1", entry => a.push(entry.seq), 1);
    await until(() => a.length === 2);
    const offB = await client.subscribe("s1", entry => b.push(entry.seq));
    await until(() => b.length === 3);
    expect(a).toEqual([2, 3]);
    expect(b).toEqual([1, 2, 3]);
    offA();
    host.publish(event(4));
    await until(() => b.length === 4);
    await dropAndReconnect(host, client);
    expect(host.messages("subscribe", 2)).toMatchObject([{ after: 4 }]);
    expect(a).toEqual([2, 3]);
    offB();
    offB();
    await until(() => host.messages("unsubscribe").length === 1);
    await expect(client.subscribe("s1", () => {}, -1)).rejects.toMatchObject({ code: "bad_request" });
  });

  it("uses exponential jittered backoff capped at 30 seconds and cancels retries on close", async () => {
    fakeClock();
    vi.mocked(Math.random).mockReturnValue(0.5);
    const attempts = vi.fn((): WebSocketLike => { throw new Error("offline"); });
    const { client } = await setup({ transport: { connect: attempts } });
    const connecting = client.connect();
    const rejected = expect(connecting).rejects.toMatchObject({ code: "closed" });
    await vi.advanceTimersByTimeAsync(0);
    expect(attempts).toHaveBeenCalledTimes(1);
    for (const [index, delay] of [1_125, 2_250, 4_500, 9_000, 18_000, 30_000, 30_000].entries()) {
      await vi.advanceTimersByTimeAsync(delay - 1);
      expect(attempts).toHaveBeenCalledTimes(index + 1);
      await vi.advanceTimersByTimeAsync(1);
      expect(attempts).toHaveBeenCalledTimes(index + 2);
    }
    client.close();
    await rejected;
    await vi.advanceTimersByTimeAsync(100_000);
    expect(attempts).toHaveBeenCalledTimes(8);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not resurrect a closed session on reconnect", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    await client.subscribe("s1", () => {});
    const closed = vi.fn();
    client.on("session_closed", closed);
    host.push(host.sockets[0]!, { t: "session_closed", session: "s1" });
    await until(() => closed.mock.calls.length === 1);
    await dropAndReconnect(host, client);
    expect(host.messages("subscribe", 2)).toHaveLength(0);
  });

  it("removes subscriptions whose session is not found during recovery", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    await client.subscribe("s1", () => {});
    host.intercept = (socket, message, connection) => {
      if (connection === 1 || message.t !== "subscribe") return;
      host.push(socket, { t: "error", req: message.req, code: "not_found", message: "Session gone" });
      return true;
    };
    await dropAndReconnect(host, client);
    await dropAndReconnect(host, client);
    expect(host.messages("subscribe", 2)).toHaveLength(1);
    expect(host.messages("subscribe", 3)).toHaveLength(0);
  });

  it("cleans up a failed initial subscription rather than restoring it later", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    host.intercept = (socket, message) => {
      if (message.t !== "subscribe") return;
      host.push(socket, { t: "error", req: message.req, code: "not_found", message: "Session gone" });
      return true;
    };
    await expect(client.subscribe("missing", () => {})).rejects.toMatchObject({ code: "not_found" });
    await dropAndReconnect(host, client);
    expect(host.messages("subscribe", 2)).toHaveLength(0);
  });

  it("recovers after malformed JSON instead of crashing the frame reader", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    host.sockets[0]!.send("{");
    await until(() => client.state === "reconnecting");
    await vi.advanceTimersByTimeAsync(1_000);
    await until(() => client.state === "open");
    expect(await client.listAgents()).toEqual([agent]);
  });
});

describe("heartbeat", () => {
  it("pings at 20 seconds and reconnects at 40 seconds without incoming frames", async () => {
    fakeClock();
    const { host, client } = await setup();
    host.pongs = false;
    await client.connect();
    await vi.advanceTimersByTimeAsync(19_999);
    expect(host.messages("ping")).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    await until(() => host.messages("ping").length === 1);
    expect(host.messages("ping")).toEqual([{ t: "ping", n: 1 }]);
    await vi.advanceTimersByTimeAsync(19_999);
    expect(client.state).toBe("open");
    await vi.advanceTimersByTimeAsync(1);
    expect(client.state).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(1_000);
    await until(() => host.sockets.length === 2 && client.state === "open");
  });

  it("counts any incoming frame as activity, not only pong", async () => {
    fakeClock();
    const { host, client } = await setup();
    host.pongs = false;
    await client.connect();
    await vi.advanceTimersByTimeAsync(30_000);
    const notice = vi.fn();
    client.on("notify", notice);
    host.push(host.sockets[0]!, { t: "notify", kind: "done", title: "done", body: "" });
    await until(() => notice.mock.calls.length === 1);
    await vi.advanceTimersByTimeAsync(39_999);
    expect(client.state).toBe("open");
    await vi.advanceTimersByTimeAsync(1);
    expect(client.state).toBe("reconnecting");
  });

  it("keeps an active pong connection open across several idle windows", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    for (let n = 1; n <= 5; n++) {
      await vi.advanceTimersByTimeAsync(20_000);
      await until(() => host.messages("ping").length === n);
      await client.listAgents(); // I/O barrier: the earlier pong has passed through the reader.
      expect(client.state).toBe("open");
    }
    expect(host.sockets).toHaveLength(1);
  });
});

describe("terminal binary streams", () => {
  it("preserves immediate scrollback, UTF-8 strings, arbitrary bytes, resize and reconnect attachment", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    const terminal = await client.openTerminal({ command: ["fixture", "--arg"], cwd: "/work", cols: 80, rows: 24 });
    expect(terminal.info).toMatchObject({ command: ["fixture", "--arg"], cwd: "/work", cols: 80, rows: 24 });
    await client.listAgents(); // Initial binary data arrives before onData is registered.
    const chunks: Uint8Array[] = [];
    const off = terminal.onData(bytes => chunks.push(bytes));
    expect(chunks.map(bytes => decoder.decode(bytes))).toEqual(["历史\u001b[0m"]);
    terminal.write("你好\r\n");
    const raw = new Uint8Array([0, 255, 128, 27, 91, 65]);
    terminal.write(raw);
    await until(() => chunks.length === 3);
    expect(chunks[1]).toEqual(encoder.encode("你好\r\n"));
    expect(chunks[2]).toEqual(raw);
    expect(host.binary.map(frame => frame.term)).toEqual([terminal.info.id, terminal.info.id]);
    await terminal.resize(120, 40);
    expect(terminal.info).toMatchObject({ cols: 120, rows: 40 });
    expect(await client.listTerminals()).toMatchObject([{ id: terminal.info.id, cols: 120, rows: 40 }]);
    await dropAndReconnect(host, client);
    await until(() => chunks.length === 4);
    expect(host.messages("term_attach", 2)).toMatchObject([{ term: terminal.info.id }]);
    terminal.write("after reconnect");
    await until(() => chunks.length === 5);
    expect(decoder.decode(chunks[4])).toBe("after reconnect");
    off();
    await terminal.detach();
    expect(() => terminal.write("detached")).toThrow(TetherError);
    await dropAndReconnect(host, client);
    expect(host.messages("term_attach", 3)).toHaveLength(0);
  });

  it("attaches an existing terminal, emits exit codes including null and never reattaches exited terms", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    const opened = await client.openTerminal({ agent: "echo", cols: 80, rows: 24 });
    const terminal = await client.attachTerminal(opened.info.id);
    expect(terminal).toBe(opened);
    const exit = vi.fn();
    const off = terminal.onExit(exit);
    host.push(host.sockets[0]!, { t: "term_exit", term: terminal.info.id, code: 7 });
    await until(() => exit.mock.calls.length === 1);
    expect(exit).toHaveBeenCalledWith(7);
    expect(terminal.info.running).toBe(false);
    expect(() => terminal.write("exit")).toThrow(TetherError);
    const late = vi.fn();
    terminal.onExit(late);
    expect(late).toHaveBeenCalledWith(7);
    off();
    await dropAndReconnect(host, client);
    expect(host.messages("term_attach", 2)).toHaveLength(0);
    const killed = await client.openTerminal({ cols: 80, rows: 24 });
    const killedExit = vi.fn();
    killed.onExit(killedExit);
    await killed.kill();
    await until(() => killedExit.mock.calls.length === 1);
    expect(killedExit).toHaveBeenCalledWith(null);
  });

  it("handles a terminal disappearing while disconnected without endless recovery", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    const terminal = await client.openTerminal({ cols: 80, rows: 24 });
    const exit = vi.fn();
    terminal.onExit(exit);
    host.terms.delete(terminal.info.id);
    await dropAndReconnect(host, client);
    expect(exit).toHaveBeenCalledWith(null);
    await dropAndReconnect(host, client);
    expect(host.messages("term_attach", 3)).toHaveLength(0);
  });

  it("does not reattach a terminal detached while offline", async () => {
    fakeClock();
    const { host, client } = await setup();
    await client.connect();
    const terminal = await client.openTerminal({ cols: 80, rows: 24 });
    host.sockets[0]!.terminate();
    await until(() => client.state === "reconnecting");
    expect(() => terminal.write("offline")).toThrow(TetherError);
    await expect(terminal.detach()).rejects.toMatchObject({ code: "disconnected" });
    await vi.advanceTimersByTimeAsync(1_000);
    await until(() => client.state === "open");
    expect(host.messages("term_attach", 2)).toHaveLength(0);
  });

  it("round-trips binary through the default global WebSocket", async () => {
    const { client } = await setup({}, true);
    await client.connect();
    const terminal = await client.openTerminal({ cols: 80, rows: 24 });
    const chunks: Uint8Array[] = [];
    terminal.onData(bytes => chunks.push(bytes));
    terminal.write(new Uint8Array([0, 255, 128]));
    await until(() => chunks.length === 2);
    expect(chunks[1]).toEqual(new Uint8Array([0, 255, 128]));
  });

  it("rejects oversized terminal input before sending a binary frame", async () => {
    const { host, client } = await setup();
    await client.connect();
    const terminal = await client.openTerminal({ cols: 80, rows: 24 });
    expect(() => terminal.write(new Uint8Array(8 << 20))).toThrowError(expect.objectContaining({ code: "bad_request" }));
    expect(host.binary).toHaveLength(0);
  });
});

// Tether TypeScript client: connection, auth, reconnect with replay, typed
// requests and terminal streams over docs/PROTOCOL.md.
// The public API below is frozen (docs/CONTRACTS.md).

import type {
  AgentInfo,
  AgentsData,
  ClientMsg,
  Decision,
  Event,
  FileContent,
  FileData,
  HistoryData,
  HistoryReadData,
  ModelInfo,
  ModelsData,
  NotifyKind,
  ServerMsg,
  SessionData,
  SessionInfo,
  SessionsData,
  TermData,
  TermInfo,
  TermsData,
} from "./generated";
import { PROTOCOL_VERSION } from "./generated";
import { decodeBinary, encodeBinary, TERM_INPUT, TERM_OUTPUT } from "./binary";

export * from "./generated";
export { encodeBinary, decodeBinary, TERM_OUTPUT, TERM_INPUT } from "./binary";

/** The subset of the WebSocket API the client uses (browser, React Native and `ws` all fit). */
export interface WebSocketLike {
  binaryType: string;
  readonly readyState: number;
  send(data: string | ArrayBufferLike | ArrayBufferView): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onclose: ((ev: { code: number; reason: string }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
}

export interface Transport {
  connect(url: string): WebSocketLike;
}

export interface KeyValueStorage {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
}

export interface ClientOptions {
  /** `ws://127.0.0.1:<port>/v1/ws`, the local address react-native-tailpocket gives. */
  url: string;
  deviceName: string;
  /** Present once paired. */
  token?: string;
  /** Present for the first connection only. */
  pairSecret?: string;
  /** The token is saved under the key `"token"` after pairing. */
  storage: KeyValueStorage;
  /** Defaults to the global `WebSocket`. */
  transport?: Transport;
}

export interface Welcome {
  version: number;
  host_name: string;
  device_id: string;
  server_time: number;
}

export type ClientState = "connecting" | "open" | "reconnecting" | "closed";

export interface NotifyMessage {
  kind: NotifyKind;
  session?: string | null;
  term?: string | null;
  title: string;
  body: string;
}

export interface ClientEvents {
  state: ClientState;
  session_update: SessionInfo;
  session_closed: string;
  notify: NotifyMessage;
}

/** An `error` reply, or a rejected handshake (`code` is then the reject reason). */
export class TetherError extends Error {
  constructor(
    readonly code: string,
    message?: string,
  ) {
    super(message ?? code);
    this.name = "TetherError";
  }
}

export interface TerminalHandle {
  info: TermInfo;
  onData(fn: (bytes: Uint8Array) => void): () => void;
  onExit(fn: (code: number | null) => void): () => void;
  write(data: string | Uint8Array): void;
  resize(cols: number, rows: number): Promise<void>;
  detach(): Promise<void>;
  kill(): Promise<void>;
}

const REQUEST_TIMEOUT = 30_000;
const HANDSHAKE_TIMEOUT = 5_000;
const HEARTBEAT_INTERVAL = 20_000;
const IDLE_TIMEOUT = 40_000;
const MAX_FRAME = 8 << 20;
const encoder = new TextEncoder();

// Derive request bodies from the generated union, rather than duplicating the wire protocol.
type WithoutReq<T> = T extends { req: number } ? Omit<T, "req"> : never;
type RequestBody = WithoutReq<ClientMsg>;
type Timer = ReturnType<typeof setTimeout>;
type Pending = { resolve(data: unknown): void; reject(error: Error): void; timer: Timer };
type Subscriber = { fn(event: Event): void; seq: number };
type Subscription = { subscribers: Set<Subscriber> };
type Terminal = {
  handle: TerminalHandle;
  active: boolean;
  attached: number;
  data: Set<(bytes: Uint8Array) => void>;
  exits: Set<(code: number | null) => void>;
  backlog: Uint8Array[];
  backlogBytes: number;
  exit?: { code: number | null };
};

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TetherError("bad_response", "Expected an object");
  }
  return value as Record<string, unknown>;
}

function arrayField<T>(data: unknown, field: string): T[] {
  const value = record(data)[field];
  if (!Array.isArray(value)) throw new TetherError("bad_response", `Expected ${field} array`);
  return value as T[];
}

function objectField<T>(data: unknown, field: string): T {
  const value = record(record(data)[field]);
  if (typeof value.id !== "string") throw new TetherError("bad_response", `Invalid ${field} id`);
  return value as T;
}

function stringField<T>(data: unknown, field: string): T {
  if (typeof record(data)[field] !== "string") throw new TetherError("bad_response", `Expected ${field} string`);
  return data as T;
}

function emptyReply(data: unknown): void {
  record(data);
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

// A consumer exception must not stop other consumers or interrupt the protocol reader.
function call<T>(fn: (value: T) => void, value: T): void {
  try { fn(value); } catch { /* Consumer-owned callback. */ }
}

export class TetherClient {
  private status: ClientState = "closed";
  private socket?: WebSocketLike;
  private generation = 0;
  private stopped = true;
  private authenticated = false;
  private welcome?: Welcome;
  private token?: string;
  private pairSecret?: string;
  private credentials?: Promise<void>;
  private connecting?: { promise: Promise<Welcome>; resolve(value: Welcome): void; reject(error: Error): void };
  private retry = 0;
  private reconnectTimer?: Timer;
  private handshakeTimer?: Timer;
  private heartbeatTimer?: ReturnType<typeof setInterval>;
  private idleTimer?: Timer;
  private nextReq = 0;
  private nextPing = 0;
  private pending = new Map<number, Pending>();
  private listeners: { [K in keyof ClientEvents]: Set<(value: ClientEvents[K]) => void> } = {
    state: new Set(), session_update: new Set(), session_closed: new Set(), notify: new Set(),
  };
  private subscriptions = new Map<string, Subscription>();
  private terminals = new Map<string, Terminal>();

  constructor(readonly options: ClientOptions) {
    this.token = options.token;
    this.pairSecret = options.pairSecret;
  }

  get state(): ClientState {
    return this.status;
  }

  /** Pairs first when `pairSecret` is set; the same socket receives welcome without another hello. */
  connect(): Promise<Welcome> {
    if (this.status === "open" && this.welcome) return Promise.resolve(this.welcome);
    if (this.connecting) return this.connecting.promise;
    let resolve!: (value: Welcome) => void;
    let reject!: (error: Error) => void;
    const promise = new Promise<Welcome>((ok, fail) => { resolve = ok; reject = fail; });
    this.connecting = { promise, resolve, reject };
    if (this.stopped) {
      this.stopped = false;
      this.retry = 0;
      this.setState("connecting");
      if (!this.stopped) void this.startAttempt();
    }
    return promise;
  }

  close(): void {
    this.stop(new TetherError("closed", "Client closed"));
  }

  on<K extends keyof ClientEvents>(event: K, fn: (value: ClientEvents[K]) => void): () => void {
    this.listeners[event].add(fn);
    return () => { this.listeners[event].delete(fn); };
  }

  listAgents(): Promise<AgentInfo[]> {
    return this.request({ t: "list_agents" }, data => arrayField<AgentsData["agents"][number]>(data, "agents"));
  }

  listSessions(): Promise<SessionInfo[]> {
    return this.request({ t: "list_sessions" }, data => arrayField<SessionsData["sessions"][number]>(data, "sessions"));
  }

  /**
   * `resume`: a `HistoryThread.id` to continue. `chat`: a conversation without
   * a project (its own folder, read only unless `permission` says otherwise).
   * `model` / `effort` / `speed` / `permission`: `null` is the agent's default. `project` / `roots`: a `ProjectInfo` to file it
   * under (`cwd` one of its roots). `reveal`: show it in the agent's desktop
   * app after the first message.
   */
  createSession(agent: string, opts?: {
    cwd?: string;
    title?: string;
    resume?: string;
    model?: string | null;
    effort?: string | null;
    chat?: boolean;
    project?: string | null;
    roots?: string[] | null;
    speed?: string | null;
    /** A `PermissionInfo.id` from `listModelChoices`. */
    permission?: string | null;
    reveal?: boolean;
    /** With `resume`: continue in a copy of that conversation, leaving the original alone. */
    fork?: boolean;
  }): Promise<SessionInfo> {
    return this.request({ t: "create_session", agent, ...opts }, data => objectField<SessionData["session"]>(data, "session"));
  }

  /** The agent's own saved conversations, newest first; pass `next_cursor` for more. */
  listHistory(agent: string, opts?: { search?: string; cursor?: string }): Promise<HistoryData> {
    return this.request({ t: "list_history", agent, ...opts }, data => {
      arrayField<HistoryData["threads"][number]>(data, "threads");
      return data as HistoryData;
    });
  }

  /** One saved conversation as it is on the computer now. */
  readHistory(agent: string, thread: string): Promise<HistoryReadData> {
    return this.request({ t: "read_history", agent, thread }, data => {
      objectField<HistoryReadData["thread"]>(data, "thread");
      arrayField<Event>(data, "events");
      return data as HistoryReadData;
    });
  }

  /** Empty when the agent offers no list; a typed name still works. */
  listModels(agent: string): Promise<ModelInfo[]> {
    return this.listModelChoices(agent).then(data => data.models);
  }

  /** The models, and what the agent may be allowed to do (empty from an older computer: no choice). */
  listModelChoices(agent: string): Promise<ModelsData> {
    return this.request({ t: "list_models", agent }, data => {
      const models = arrayField<ModelsData["models"][number]>(data, "models");
      const o = data as Partial<ModelsData>;
      return { models, permissions: Array.isArray(o.permissions) ? o.permissions : [], default_permission: o.default_permission ?? null };
    });
  }

  /** From the next turn on; `null`/omitted leaves that one as it is. */
  setModel(session: string, model?: string | null, effort?: string | null, speed?: string | null, permission?: string | null): Promise<void> {
    return this.request(
      { t: "set_model", session, model: model ?? null, effort: effort ?? null, speed: speed ?? null, permission: permission ?? null },
      emptyReply,
    );
  }

  /** Opens one of the agent's conversations in its desktop app on the computer. */
  reveal(agent: string, thread: string): Promise<void> {
    return this.request({ t: "reveal", agent, thread }, emptyReply);
  }

  /** Replays from `after`, resubscribes after reconnects, never repeats a seq. Returns an unsubscribe function. */
  subscribe(session: string, onEvent: (e: Event) => void, after?: number): Promise<() => void> {
    if (!Number.isSafeInteger(after ?? 0) || (after ?? 0) < 0) {
      return Promise.reject(new TetherError("bad_request", "after must be a nonnegative safe integer"));
    }
    let subscription = this.subscriptions.get(session);
    if (!subscription) {
      subscription = { subscribers: new Set() };
      this.subscriptions.set(session, subscription);
    }
    const target = subscription;
    const subscriber: Subscriber = { fn: onEvent, seq: after ?? 0 };
    target.subscribers.add(subscriber);
    // Register before sending, since history can follow ok in the very same socket read.
    return this.request({ t: "subscribe", session, after: this.cursor(target) }, data => {
      objectField<SessionData["session"]>(data, "session");
    }).then(() => () => {
      target.subscribers.delete(subscriber);
      if (target.subscribers.size === 0 && this.subscriptions.get(session) === target) {
        this.subscriptions.delete(session);
        if (this.authenticated) void this.request({ t: "unsubscribe", session }, emptyReply).catch(() => {});
      }
    }, error => {
      target.subscribers.delete(subscriber);
      if (target.subscribers.size === 0 && this.subscriptions.get(session) === target) {
        this.subscriptions.delete(session);
      }
      throw error;
    });
  }

  /** `images`: paths on the computer, as `putFile` returns them. */
  send(session: string, text: string, images?: string[]): Promise<void> {
    const body: RequestBody = images?.length ? { t: "send_message", session, text, images } : { t: "send_message", session, text };
    return this.request(body, emptyReply);
  }

  /** Saves a picture (base64) on the computer, to send with a message. */
  putFile(name: string, data: string): Promise<FileData> {
    return this.request({ t: "put_file", name, data }, reply => stringField<FileData>(reply, "path"));
  }

  /** A picture on the computer, such as one a message was sent with. */
  getFile(path: string): Promise<FileContent> {
    return this.request({ t: "get_file", path }, reply => {
      stringField(reply, "mime");
      return stringField<FileContent>(reply, "data");
    });
  }

  answer(session: string, approval: string, decision: Decision): Promise<void> {
    return this.request({ t: "answer", session, approval, decision }, emptyReply);
  }

  interrupt(session: string): Promise<void> {
    return this.request({ t: "interrupt", session }, emptyReply);
  }

  closeSession(session: string): Promise<void> {
    return this.request({ t: "close_session", session }, emptyReply);
  }

  openTerminal(o: { command?: string[]; agent?: string; cwd?: string; cols: number; rows: number }): Promise<TerminalHandle> {
    return this.request({ t: "term_open", ...o }, data => this.terminal(objectField<TermData["term"]>(data, "term")).handle);
  }

  attachTerminal(term: string): Promise<TerminalHandle> {
    return this.request({ t: "term_attach", term }, data => this.terminal(objectField<TermData["term"]>(data, "term")).handle);
  }

  listTerminals(): Promise<TermInfo[]> {
    return this.request({ t: "list_terms" }, data => arrayField<TermsData["terms"][number]>(data, "terms"));
  }

  private setState(state: ClientState): void {
    if (this.status === state) return;
    this.status = state;
    this.emit("state", state);
  }

  private emit<K extends keyof ClientEvents>(event: K, value: ClientEvents[K]): void {
    for (const fn of [...this.listeners[event]]) call(fn, value);
  }

  private current(generation: number): boolean {
    return !this.stopped && this.generation === generation;
  }

  private async startAttempt(): Promise<void> {
    const generation = ++this.generation;
    try {
      this.credentials ??= (async () => {
        if (this.token === undefined && this.pairSecret === undefined) {
          this.token = (await this.options.storage.get("token")) ?? undefined;
        }
      })();
      await this.credentials;
    } catch (error) {
      this.credentials = undefined;
      if (this.current(generation)) this.stop(asError(error));
      return;
    }
    if (!this.current(generation)) return;
    if (this.pairSecret === undefined && !this.token) {
      this.stop(new TetherError("unauthorized", "No stored device token or pairing secret"));
      return;
    }
    try {
      const socket: WebSocketLike = this.options.transport
        ? this.options.transport.connect(this.options.url)
        // The frozen transport also permits SharedArrayBuffer; we only send strings/owned Uint8Arrays.
        : new WebSocket(this.options.url) as unknown as WebSocketLike;
      this.socket = socket;
      socket.binaryType = "arraybuffer";
      let paired = false;
      let authSent = false;
      let welcomeReceived = false;
      let messages = Promise.resolve();
      this.handshakeTimer = setTimeout(() => this.lost(generation, new TetherError("timeout", "Handshake timed out")), HANDSHAKE_TIMEOUT);
      socket.onopen = () => {
        if (!this.current(generation)) return;
        try {
          authSent = true;
          this.sendFrame(this.pairSecret !== undefined
            ? { t: "pair", version: PROTOCOL_VERSION, secret: this.pairSecret, device_name: this.options.deviceName }
            : { t: "hello", version: PROTOCOL_VERSION, token: this.token!, device_name: this.options.deviceName });
        } catch (error) { this.lost(generation, asError(error)); }
      };
      socket.onmessage = ({ data }) => {
        if (!this.current(generation)) return;
        if (this.authenticated) this.touch(generation);
        // Serialize pairing storage and welcome, but never wait for recovery replies in this queue.
        messages = messages.then(async () => {
          if (!this.current(generation)) return;
          if (typeof data !== "string") {
            if (!this.authenticated) throw new TetherError("bad_response", "Binary frame before welcome");
            this.receiveBinary(data);
            return;
          }
          if (encoder.encode(data).length > MAX_FRAME) throw new TetherError("bad_response", "Frame exceeds 8 MiB");
          const raw = record(JSON.parse(data));
          if (typeof raw.t !== "string") throw new TetherError("bad_response", "Missing message type");
          const message = raw as ServerMsg;
          if (message.t === "rejected") {
            const error = Object.assign(new TetherError(message.reason), { reason: message.reason });
            this.stop(error);
          } else if (message.t === "paired") {
            if (!authSent || paired || this.pairSecret === undefined || typeof message.token !== "string" || !message.token) {
              throw new TetherError("bad_response", "Unexpected paired reply");
            }
            paired = true;
            this.token = message.token;
            this.pairSecret = undefined;
            try { await this.options.storage.set("token", message.token); }
            catch (error) { if (this.current(generation)) this.stop(asError(error)); }
          } else if (message.t === "welcome") {
            if (!authSent || welcomeReceived || this.pairSecret !== undefined) throw new TetherError("bad_response", "Unexpected welcome");
            if (message.version !== PROTOCOL_VERSION) {
              this.stop(Object.assign(new TetherError("version_mismatch"), { reason: "version_mismatch" }));
              return;
            }
            welcomeReceived = true;
            void this.finishHandshake(message, generation).catch(error => this.lost(generation, asError(error)));
          } else {
            if (!this.authenticated) throw new TetherError("bad_response", "Message before welcome");
            this.receive(message);
          }
        }).catch(error => this.lost(generation, asError(error)));
      };
      socket.onerror = () => this.lost(generation, new TetherError("disconnected", "WebSocket error"));
      socket.onclose = ({ code, reason }) => {
        if (!this.current(generation)) return;
        if (code === 4401 || code === 4426) {
          const failure = code === 4426 ? "version_mismatch" : "unauthorized";
          this.stop(Object.assign(new TetherError(failure, reason || failure), { reason: failure }));
        } else this.lost(generation, new TetherError("disconnected", reason || `WebSocket closed (${code})`));
      };
    } catch (error) { this.lost(generation, asError(error)); }
  }

  private async finishHandshake(message: Welcome, generation: number): Promise<void> {
    clearTimeout(this.handshakeTimer);
    this.handshakeTimer = undefined;
    this.authenticated = true;
    this.welcome = {
      version: message.version, host_name: message.host_name,
      device_id: message.device_id, server_time: message.server_time,
    };
    this.touch(generation);
    this.heartbeatTimer = setInterval(() => {
      if (!this.current(generation)) return;
      try { this.sendFrame({ t: "ping", n: ++this.nextPing }); }
      catch (error) { this.lost(generation, asError(error)); }
    }, HEARTBEAT_INTERVAL);
    const recovery: Promise<unknown>[] = [];
    for (const [session, subscription] of this.subscriptions) {
      recovery.push(this.request({ t: "subscribe", session, after: this.cursor(subscription) }, data => {
        objectField<SessionData["session"]>(data, "session");
      }).catch(error => {
        if (!this.current(generation)) return;
        if (error instanceof TetherError && error.code === "not_found") {
          if (this.subscriptions.get(session) === subscription) this.subscriptions.delete(session);
        } else throw error;
      }));
    }
    for (const [term, terminal] of this.terminals) {
      if (!terminal.active) continue;
      recovery.push(this.request({ t: "term_attach", term }, data => {
        const info = objectField<TermData["term"]>(data, "term");
        if (terminal.active && this.terminals.get(term) === terminal) {
          terminal.handle.info = info;
          terminal.attached = generation;
        }
      }).catch(error => {
        if (!this.current(generation)) return;
        if (error instanceof TetherError && error.code === "not_found") this.exitTerminal(term, null);
        else throw error;
      }));
    }
    await Promise.all(recovery);
    if (!this.current(generation)) return;
    this.retry = 0;
    const welcome = this.welcome!;
    const connecting = this.connecting;
    this.connecting = undefined;
    this.setState("open");
    if (this.current(generation)) connecting?.resolve(welcome);
    else connecting?.reject(new TetherError("closed", "Client closed while connecting"));
  }

  private touch(generation: number): void {
    clearTimeout(this.idleTimer);
    this.idleTimer = setTimeout(() => this.lost(generation, new TetherError("timeout", "No frames received for 40 seconds")), IDLE_TIMEOUT);
  }

  private disposeSocket(error: Error): void {
    ++this.generation;
    this.authenticated = false;
    this.welcome = undefined;
    clearTimeout(this.handshakeTimer);
    clearTimeout(this.idleTimer);
    clearInterval(this.heartbeatTimer);
    this.handshakeTimer = this.idleTimer = this.heartbeatTimer = undefined;
    const socket = this.socket;
    this.socket = undefined;
    if (socket) {
      socket.onopen = socket.onclose = socket.onmessage = null;
      // ws also exposes onerror; keeping a sink avoids a late connecting-socket error.
      socket.onerror = () => {};
      try { socket.close(); } catch { /* Already closed by the transport. */ }
    }
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  private lost(generation: number, error: Error): void {
    if (!this.current(generation)) return;
    this.disposeSocket(error);
    const disposed = this.generation;
    this.setState("reconnecting");
    if (!this.current(disposed)) return;
    const base = Math.min(30_000, 1_000 * 2 ** Math.min(this.retry++, 5));
    const delay = Math.min(30_000, base + Math.floor(Math.random() * base * 0.25));
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = undefined;
      if (!this.stopped) void this.startAttempt();
    }, delay);
  }

  private stop(error: Error): void {
    this.stopped = true;
    clearTimeout(this.reconnectTimer);
    this.reconnectTimer = undefined;
    this.disposeSocket(error);
    const connecting = this.connecting;
    this.connecting = undefined;
    connecting?.reject(error);
    for (const subscription of this.subscriptions.values()) subscription.subscribers.clear();
    this.subscriptions.clear();
    for (const terminal of this.terminals.values()) terminal.active = false;
    this.terminals.clear();
    this.setState("closed");
  }

  private sendFrame(message: ClientMsg): void {
    const text = JSON.stringify(message);
    if (encoder.encode(text).length > MAX_FRAME) throw new TetherError("bad_request", "Frame exceeds 8 MiB");
    if (this.socket?.readyState !== 1) throw new TetherError("disconnected", "WebSocket is not open");
    this.socket.send(text);
  }

  private request<T>(body: RequestBody, read: (data: unknown) => T): Promise<T> {
    if (!this.authenticated || this.socket?.readyState !== 1) {
      return Promise.reject(new TetherError(this.stopped ? "closed" : "disconnected", "Client is not connected"));
    }
    if (this.nextReq === Number.MAX_SAFE_INTEGER) return Promise.reject(new TetherError("bad_request", "Request counter exhausted"));
    const req = ++this.nextReq;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(req);
        reject(new TetherError("timeout", `Request ${req} (${body.t}) timed out`));
      }, REQUEST_TIMEOUT);
      this.pending.set(req, {
        timer, reject,
        resolve: data => {
          try { resolve(read(data)); } catch (error) { reject(asError(error)); }
        },
      });
      try { this.sendFrame({ ...body, req }); }
      catch (error) {
        clearTimeout(timer);
        this.pending.delete(req);
        reject(asError(error));
      }
    });
  }

  private receive(message: ServerMsg): void {
    switch (message.t) {
      case "ok":
      case "error": {
        const req = message.req;
        if (req === null || req === undefined) return;
        const pending = this.pending.get(req);
        if (!pending) return; // Late reply to a timed-out request or a previous connection.
        this.pending.delete(req);
        clearTimeout(pending.timer);
        if (message.t === "ok") pending.resolve(message.data);
        else pending.reject(new TetherError(message.code, message.message));
        break;
      }
      case "event": {
        const subscription = this.subscriptions.get(message.session);
        if (!subscription) return;
        const event = message.event;
        if (!Number.isSafeInteger(event.seq) || event.seq < 0) throw new TetherError("bad_response", "Invalid event seq");
        for (const subscriber of [...subscription.subscribers]) {
          if (!subscription.subscribers.has(subscriber)) continue;
          if (event.e !== "assistant_delta") {
            if (event.seq <= subscriber.seq) continue;
            subscriber.seq = event.seq;
          }
          call(subscriber.fn, event);
        }
        break;
      }
      case "session_update": this.emit("session_update", message.session); break;
      case "session_closed":
        this.subscriptions.delete(message.session);
        this.emit("session_closed", message.session);
        break;
      case "notify": this.emit("notify", {
        kind: message.kind, session: message.session, term: message.term, title: message.title, body: message.body,
      }); break;
      case "term_exit": this.exitTerminal(message.term, message.code ?? null); break;
      case "pong": break;
      default: throw new TetherError("bad_response", "Unexpected server message");
    }
  }

  private cursor(subscription: Subscription): number {
    // Usually one listener. A late listener may require an earlier replay; each listener deduplicates independently.
    let seq = Number.MAX_SAFE_INTEGER;
    for (const subscriber of subscription.subscribers) seq = Math.min(seq, subscriber.seq);
    return seq;
  }

  private terminal(info: TermInfo): Terminal {
    const existing = this.terminals.get(info.id);
    if (existing) {
      existing.handle.info = info;
      existing.active = true;
      existing.attached = this.generation;
      return existing;
    }
    const terminal: Terminal = {
      handle: {
        info,
        onData: fn => {
          terminal.data.add(fn);
          const backlog = terminal.backlog;
          terminal.backlog = [];
          terminal.backlogBytes = 0;
          for (const bytes of backlog) {
            if (terminal.data.has(fn)) call(fn, bytes);
          }
          return () => { terminal.data.delete(fn); };
        },
        onExit: fn => {
          terminal.exits.add(fn);
          if (terminal.exit) call(fn, terminal.exit.code);
          return () => { terminal.exits.delete(fn); };
        },
        write: data => {
          if (!terminal.active) throw new TetherError("closed", "Terminal is detached or exited");
          if (!this.authenticated || terminal.attached !== this.generation || this.socket?.readyState !== 1) {
            throw new TetherError("disconnected", "Terminal is not attached");
          }
          const frame = encodeBinary(TERM_INPUT, info.id, typeof data === "string" ? encoder.encode(data) : data);
          if (frame.length > MAX_FRAME) throw new TetherError("bad_request", "Frame exceeds 8 MiB");
          this.socket.send(frame);
        },
        resize: (cols, rows) => {
          if (!terminal.active) return Promise.reject(new TetherError("closed", "Terminal is detached or exited"));
          return this.request({ t: "term_resize", term: info.id, cols, rows }, emptyReply).then(() => {
            terminal.handle.info = { ...terminal.handle.info, cols, rows };
          });
        },
        detach: () => this.removeTerminal(info.id, terminal, "term_detach"),
        kill: () => this.removeTerminal(info.id, terminal, "term_kill"),
      },
      active: true, attached: this.generation, data: new Set(), exits: new Set(), backlog: [], backlogBytes: 0,
    };
    this.terminals.set(info.id, terminal);
    return terminal;
  }

  private removeTerminal(term: string, terminal: Terminal, t: "term_detach" | "term_kill"): Promise<void> {
    if (!terminal.active) return Promise.resolve();
    // Record intent immediately: a disconnect before the reply must not undo a detach/kill.
    terminal.active = false;
    if (t === "term_detach" && this.terminals.get(term) === terminal) this.terminals.delete(term);
    terminal.backlog = [];
    terminal.backlogBytes = 0;
    return this.request({ t, term }, emptyReply).then(() => {
      if (t === "term_kill") terminal.handle.info = { ...terminal.handle.info, running: false };
    });
  }

  private receiveBinary(value: unknown): void {
    let bytes: Uint8Array;
    if (value instanceof ArrayBuffer) bytes = new Uint8Array(value);
    else if (ArrayBuffer.isView(value)) bytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    else throw new TetherError("bad_response", "Expected an ArrayBuffer or typed array");
    if (bytes.length > MAX_FRAME) throw new TetherError("bad_response", "Frame exceeds 8 MiB");
    const frame = decodeBinary(bytes);
    if (frame.kind !== TERM_OUTPUT) throw new TetherError("bad_response", "Expected terminal output");
    const terminal = this.terminals.get(frame.term);
    if (!terminal?.active) return;
    if (terminal.data.size > 0) {
      for (const fn of [...terminal.data]) call(fn, frame.data.slice());
    } else {
      // Retain initial scrollback until the caller can install onData after await open/attach.
      terminal.backlog.push(frame.data.slice());
      terminal.backlogBytes += frame.data.length;
      while (terminal.backlogBytes > MAX_FRAME) terminal.backlogBytes -= terminal.backlog.shift()!.length;
    }
  }

  private exitTerminal(term: string, code: number | null): void {
    const terminal = this.terminals.get(term);
    if (!terminal) return;
    terminal.active = false;
    terminal.exit = { code };
    terminal.handle.info = { ...terminal.handle.info, running: false };
    this.terminals.delete(term);
    for (const fn of [...terminal.exits]) call(fn, code);
  }
}

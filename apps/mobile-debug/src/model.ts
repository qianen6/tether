// Pure state for the screens: what a session's event stream looks like as a
// chat, and how sessions are ordered. No React Native imports, so vitest runs it.
import type { Approval, Decision, Event, SessionInfo, SessionState } from "@tether/client";

export type ChatItem =
  | { kind: "user"; key: string; text: string; images?: string[] }
  | { kind: "assistant"; key: string; text: string; done: boolean }
  | { kind: "thinking"; key: string; text: string }
  | { kind: "tool"; key: string; tool: string; summary: string; output?: string; ok?: boolean }
  | { kind: "approval"; key: string; approval: Approval; decision?: Decision }
  | { kind: "notice"; key: string; level: string; text: string }
  | { kind: "turn"; key: string; summary?: string | null };

export interface Chat {
  items: ChatItem[];
  state: SessionState;
  statusLine?: string | null;
  /** Highest durable seq applied; deltas do not move it. */
  seq: number;
}

export const emptyChat: Chat = { items: [], state: "idle", seq: 0 };

function upsert(items: ChatItem[], key: string, make: (old?: ChatItem) => ChatItem): ChatItem[] {
  const i = items.findIndex(x => x.key === key);
  if (i < 0) return [...items, make()];
  const next = items.slice();
  next[i] = make(items[i]);
  return next;
}

/** Applies one event. Durable events at or below `chat.seq` are replays and ignored. */
export function applyEvent(chat: Chat, e: Event): Chat {
  if (e.e !== "assistant_delta") {
    if (e.seq <= chat.seq) return chat;
    chat = { ...chat, seq: e.seq };
  }
  switch (e.e) {
    case "user_text":
      return {
        ...chat,
        items: [...chat.items, { kind: "user", key: `u${e.seq}`, text: e.text, ...(e.images?.length ? { images: e.images } : {}) }],
      };
    case "assistant_delta":
      return {
        ...chat,
        items: upsert(chat.items, `a:${e.item}`, old =>
          old?.kind === "assistant"
            ? (old.done ? old : { ...old, text: old.text + e.text })
            : { kind: "assistant", key: `a:${e.item}`, text: e.text, done: false }),
      };
    case "assistant_text":
      return {
        ...chat,
        items: upsert(chat.items, `a:${e.item}`, () => ({ kind: "assistant", key: `a:${e.item}`, text: e.text, done: true })),
      };
    case "thinking":
      return {
        ...chat,
        items: upsert(chat.items, `th:${e.item}`, () => ({ kind: "thinking", key: `th:${e.item}`, text: e.text })),
      };
    case "tool_start":
      return {
        ...chat,
        items: upsert(chat.items, `t:${e.item}`, () => ({ kind: "tool", key: `t:${e.item}`, tool: e.tool, summary: e.summary })),
      };
    case "tool_end":
      return {
        ...chat,
        items: upsert(chat.items, `t:${e.item}`, old =>
          old?.kind === "tool"
            ? { ...old, output: e.output, ok: e.ok }
            : { kind: "tool", key: `t:${e.item}`, tool: "tool", summary: "", output: e.output, ok: e.ok }),
      };
    case "approval":
      return {
        ...chat,
        items: upsert(chat.items, `p:${e.approval.id}`, () => ({ kind: "approval", key: `p:${e.approval.id}`, approval: e.approval })),
      };
    case "approval_done":
      return {
        ...chat,
        items: chat.items.map(x => (x.kind === "approval" && x.approval.id === e.approval ? { ...x, decision: e.decision } : x)),
      };
    case "state":
      return { ...chat, state: e.state, statusLine: e.status_line };
    case "turn_done":
      return { ...chat, items: [...chat.items, { kind: "turn", key: `d${e.seq}`, summary: e.summary }] };
    case "notice": {
      const text = readableError(e.text);
      // An agent that retries says the same thing each time: once is enough.
      const last = chat.items[chat.items.length - 1];
      if (last?.kind === "notice" && last.text === text) return chat;
      return { ...chat, items: [...chat.items, { kind: "notice", key: `n${e.seq}`, level: e.level, text }] };
    }
  }
}

/** The message inside an error that carries a JSON body, such as
 * `unexpected status 400: {"detail":"…"}` or `{"error":{"message":"…"}}`. */
export function readableError(text: string): string {
  const start = text.indexOf("{");
  if (start < 0) return text;
  let body: unknown;
  try {
    body = JSON.parse(text.slice(start, text.lastIndexOf("}") + 1));
  } catch {
    return text;
  }
  const message = messageOf(body);
  if (!message) return text;
  const head = text.slice(0, start).trim().replace(/[:：]$/, "").trim();
  // The computer's own explanation (in Chinese) stays first.
  if (/[一-鿿]/.test(head)) return `${head}\n${message}`;
  return head ? `${message}（${head}）` : message;
}

function messageOf(v: unknown): string | undefined {
  if (typeof v === "string") return v.trim() || undefined;
  if (!v || typeof v !== "object") return undefined;
  const o = v as Record<string, unknown>;
  for (const k of ["message", "detail", "error"]) {
    const m = messageOf(o[k]);
    if (m) return m;
  }
  return undefined;
}

type Step = Extract<ChatItem, { kind: "thinking" | "tool" }>;
/** What Codex did on the way to an answer, as one line (chatgpt.com's "Thought for…"). */
export type Steps = { kind: "steps"; key: string; steps: Step[]; active: boolean };

/**
 * Folds each run of thinking and tool calls into one Steps line, for a chat: there the
 * answer matters, and how Codex got there is a tap away. `working`: the last run may
 * still grow.
 */
export function foldSteps(items: ChatItem[], working: boolean): (ChatItem | Steps)[] {
  const out: (ChatItem | Steps)[] = [];
  for (const x of items) {
    if (x.kind !== "thinking" && x.kind !== "tool") {
      out.push(x);
      continue;
    }
    const last = out[out.length - 1];
    if (last?.kind === "steps") last.steps.push(x);
    else out.push({ kind: "steps", key: `s:${x.key}`, steps: [x], active: false });
  }
  const last = out[out.length - 1];
  if (working && last?.kind === "steps") last.active = true;
  return out;
}

/** "思考 · 运行 2 条命令 · 改了 1 个文件". */
export function stepsLabel(steps: Step[]): string {
  const count = (tools: string[]) => steps.filter(s => s.kind === "tool" && tools.includes(s.tool)).length;
  const run = count(["shell", "Bash"]);
  const edit = count(["edit", "Write", "Edit"]);
  const search = count(["webSearch", "WebSearch"]);
  const other = steps.filter(s => s.kind === "tool").length - run - edit - search;
  return [
    steps.some(s => s.kind === "thinking") ? "思考" : "",
    run ? `运行 ${run} 条命令` : "",
    edit ? `改了 ${edit} 个文件` : "",
    search ? `搜索 ${search} 次` : "",
    other ? `${other} 个其他步骤` : "",
  ].filter(Boolean).join(" · ");
}

/** Approvals still waiting for an answer. */
export function pendingApprovals(chat: Chat): Approval[] {
  return chat.items.flatMap(x => (x.kind === "approval" && !x.decision ? [x.approval] : []));
}

const order: Record<SessionState, number> = { needs_you: 0, error: 1, working: 2, idle: 3, exited: 4 };

/** Waiting on you first, then errors, working, idle; newest activity first within each. */
export function sortSessions(list: SessionInfo[]): SessionInfo[] {
  return list.slice().sort((a, b) => order[a.state] - order[b.state] || b.updated_at - a.updated_at);
}

/** A turn ended: what Herdr marks "done" (blue) until you look at it. */
export function finished(prev: SessionState | undefined, next: SessionState): boolean {
  return (prev === "working" || prev === "needs_you") && next === "idle";
}

/** Replaces or adds `s` by id. */
export function mergeSession(list: SessionInfo[], s: SessionInfo): SessionInfo[] {
  const i = list.findIndex(x => x.id === s.id);
  if (i < 0) return [...list, s];
  const next = list.slice();
  next[i] = s;
  return next;
}

export const decisionLabel: Record<Decision, string> = {
  allow_once: "允许一次",
  allow_session: "本会话都允许",
  deny: "拒绝",
  abort: "拒绝并停止",
};


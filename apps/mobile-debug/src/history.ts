// The computer's own Codex conversations on the phone: a local copy shown at
// once, refreshed from the computer each time; which phone session continues
// which conversation; and the model choices. No React Native imports, so
// vitest runs it with a memory store.
import type { AgentInfo, Event, HistoryReadData, HistoryThread, ModelInfo, PermissionInfo, ProjectInfo } from "@tether/client";
import { applyEvent, emptyChat, type ChatItem } from "./model";

/** Where the copy lives: files on the phone, a map in tests. */
export interface KV {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  remove(key: string): Promise<void>;
}

/** Conversations kept in full; the least recently opened go first. */
export const KEEP_THREADS = 40;

/** A phone session that continues a conversation of the computer's. */
export interface Continued {
  agent: string;
  thread: HistoryThread;
}

interface Index {
  /** `agent/thread`, most recently saved last. */
  threads: string[];
  /** Phone session id → what it continues. */
  links: Record<string, Continued>;
}

async function readJson<T>(kv: KV, key: string): Promise<T | undefined> {
  const text = await kv.get(key).catch(() => null);
  if (!text) return undefined;
  try {
    return JSON.parse(text) as T;
  } catch {
    return undefined;
  }
}

export class HistoryCache {
  private index?: Promise<Index>;
  /** Writes in order, so a slow one never lands after a newer one. */
  private queue: Promise<unknown> = Promise.resolve();

  constructor(private kv: KV) {}

  private loadIndex(): Promise<Index> {
    this.index ??= readJson<Index>(this.kv, "index").then(i => ({ threads: i?.threads ?? [], links: i?.links ?? {} }));
    return this.index;
  }

  private write(f: () => Promise<void>): Promise<void> {
    const next = this.queue.then(f, f);
    this.queue = next.catch(() => {});
    return next;
  }

  private saveIndex(index: Index): Promise<void> {
    return this.kv.set("index", JSON.stringify(index));
  }

  threads(agent: string): Promise<HistoryThread[] | undefined> {
    return readJson<HistoryThread[]>(this.kv, `list/${agent}`);
  }

  saveThreads(agent: string, threads: HistoryThread[]): Promise<void> {
    return this.write(() => this.kv.set(`list/${agent}`, JSON.stringify(threads)));
  }

  thread(agent: string, id: string): Promise<HistoryReadData | undefined> {
    return readJson<HistoryReadData>(this.kv, `thread/${agent}/${id}`);
  }

  saveThread(agent: string, data: HistoryReadData): Promise<void> {
    return this.write(async () => {
      const key = `${agent}/${data.thread.id}`;
      await this.kv.set(`thread/${key}`, JSON.stringify(data));
      const index = await this.loadIndex();
      index.threads = [...index.threads.filter(k => k !== key), key];
      for (const old of index.threads.splice(0, Math.max(0, index.threads.length - KEEP_THREADS))) {
        await this.kv.remove(`thread/${old}`).catch(() => {});
      }
      await this.saveIndex(index);
    });
  }

  async continued(session: string): Promise<Continued | undefined> {
    return (await this.loadIndex()).links[session];
  }

  /** The phone session continuing `thread`, among those still on the computer. */
  async sessionOf(agent: string, thread: string, live: string[]): Promise<string | undefined> {
    const links = (await this.loadIndex()).links;
    return live.find(id => links[id]?.agent === agent && links[id]?.thread.id === thread);
  }

  /** Phone session id → the conversation it continues. */
  async links(): Promise<Record<string, string>> {
    const links = (await this.loadIndex()).links;
    return Object.fromEntries(Object.entries(links).map(([s, c]) => [s, c.thread.id]));
  }

  link(session: string, what: Continued): Promise<void> {
    return this.write(async () => {
      const index = await this.loadIndex();
      index.links[session] = what;
      await this.saveIndex(index);
    });
  }

  /** Forgets links to sessions the computer no longer has. */
  prune(live: string[]): Promise<void> {
    return this.write(async () => {
      const index = await this.loadIndex();
      const keep = new Set(live);
      const stale = Object.keys(index.links).filter(id => !keep.has(id));
      if (!stale.length) return;
      for (const id of stale) delete index.links[id];
      await this.saveIndex(index);
    });
  }

  /** The Codex agent last talked to, so its saved list shows before the computer answers. */
  async codex(): Promise<string | undefined> {
    return (await this.kv.get("codex").catch(() => null)) ?? undefined;
  }

  saveCodex(agent: string): Promise<void> {
    return this.write(() => this.kv.set("codex", agent));
  }

  models(agent: string): Promise<ModelInfo[] | undefined> {
    return readJson<ModelInfo[]>(this.kv, `models/${agent}`);
  }

  saveModels(agent: string, models: ModelInfo[]): Promise<void> {
    return this.write(() => this.kv.set(`models/${agent}`, JSON.stringify(models)));
  }

  permissions(agent: string): Promise<Permissions | undefined> {
    return readJson<Permissions>(this.kv, `permissions/${agent}`);
  }

  savePermissions(agent: string, permissions: Permissions): Promise<void> {
    return this.write(() => this.kv.set(`permissions/${agent}`, JSON.stringify(permissions)));
  }

  /** The computer's projects, as the first page of the list last brought them. */
  projects(agent: string): Promise<ProjectInfo[] | undefined> {
    return readJson<ProjectInfo[]>(this.kv, `projects/${agent}`);
  }

  saveProjects(agent: string, projects: ProjectInfo[]): Promise<void> {
    return this.write(() => this.kv.set(`projects/${agent}`, JSON.stringify(projects)));
  }
}

/** A map, for tests and as a fallback when files fail. */
export function memoryKV(): KV & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    get: async k => data.get(k) ?? null,
    set: async (k, v) => void data.set(k, v),
    remove: async k => void data.delete(k),
  };
}

/** A saved conversation as chat items, keyed apart from the live ones. */
export function preludeItems(events: Event[]): ChatItem[] {
  return events.reduce(applyEvent, emptyChat).items.map(x => ({ ...x, key: `h:${x.key}` }));
}

/** The Codex to talk to: the desktop app's when it is there, else the command line's. */
export function pickCodex(agents: AgentInfo[]): AgentInfo | undefined {
  const ok = agents.filter(a => a.available && a.chat);
  return ok.find(a => a.id === "codex-desktop") ?? ok.find(a => a.kind === "codex");
}

/** What the phone picks for the next turns; `null` leaves the computer's default. */
export interface Pick {
  model: string | null;
  effort: string | null;
  speed: string | null;
  /** A `PermissionInfo.id`. */
  permission?: string | null;
}

/** What the computer's agent may be allowed to do, and what its own app uses now. */
export interface Permissions {
  list: PermissionInfo[];
  default: string | null;
}

export const NO_PERMISSIONS: Permissions = { list: [], default: null };

/**
 * The permission in effect: the one chosen, else (a new conversation) what the computer's app
 * uses now. A session from before there was a choice (`legacy`) reads only as a chat and
 * works in its folder otherwise.
 */
export function permissionOf(
  p: Permissions,
  chosen: string | null | undefined,
  o: { chat: boolean; session: boolean },
): { id: string; info?: PermissionInfo; name: string; description: string } {
  const legacy = o.chat ? ":read-only" : ":workspace";
  // Nothing chosen and no default from the computer: the computer applies the legacy one.
  const id = chosen ?? (o.session ? legacy : p.default ?? legacy);
  const info = p.list.find(x => x.id === id);
  const fallback = PERMISSION_WORD[id] ?? { name: id.replace(/^:/, ""), description: "" };
  return { id, info, name: info?.name ?? fallback.name, description: info?.description ?? fallback.description };
}

/** For a profile the computer did not name (an older computer, or not listed now). */
const PERMISSION_WORD: Record<string, { name: string; description: string }> = {
  ":read-only": { name: "只读", description: "能看能答；改文件、运行会改动东西的命令前先问你" },
  ":workspace": { name: "默认权限", description: "在工作文件夹里直接改；文件夹以外和联网前先问你" },
  ":danger-full-access": { name: "完全访问", description: "不受限制：改任何文件、运行任何命令都不问" },
};

/** What actually applies: the pick, else the model's (or the computer's) defaults. */
export function resolvePick(pick: Pick, models: ModelInfo[] = []): { model?: ModelInfo; id?: string; effort?: string; speed: string } {
  const model = (pick.model ? models.find(m => m.id === pick.model) : undefined) ?? (pick.model ? undefined : models.find(m => m.is_default));
  const id = pick.model ?? model?.id;
  const effort = pick.effort ?? model?.default_effort ?? undefined;
  const speed = pick.speed ?? model?.default_speed ?? "default";
  return { model, id, effort, speed };
}

/** The model in use, how hard it thinks and whether it runs fast, as the composer shows them. */
export function modelParts(pick: Pick, models: ModelInfo[] = []): { model: string; effort?: string; fast: boolean } {
  const r = resolvePick(pick, models);
  const fast = r.speed !== "default";
  if (!r.id) return { model: "默认模型", effort: r.effort ? effortWord(r.effort) : undefined, fast };
  return { model: r.model?.name ?? r.id, effort: r.effort ? effortWord(r.effort) : undefined, fast };
}

/** As the Codex app names them. */
export const EFFORT_WORD: Record<string, string> = {
  none: "不思考",
  minimal: "最少",
  low: "低",
  medium: "中",
  high: "高",
  xhigh: "极高",
  max: "Max",
  ultra: "Ultra",
};

export function effortWord(effort: string): string {
  return EFFORT_WORD[effort] ?? effort;
}

/** "default" is standard; the computer names faster ones ("Fast"). */
export function speedWord(speed: string, models: ModelInfo[] = []): string {
  if (speed === "default") return "标准";
  const name = models.flatMap(m => m.speeds ?? []).find(s => s.id === speed)?.name ?? speed;
  return name.toLowerCase() === "fast" ? "快速" : name;
}

/** The computer's note on a speed ("2x speed, increased usage") in Chinese, as far as known. */
export function speedDetail(text: string): string {
  return text
    .replace(/(\d+(?:\.\d+)?)x speed/gi, "$1 倍速度")
    .replace(/increased usage/gi, "用量更多")
    .replace(/,\s*/g, "，");
}

/** A title from the first message, for a new chat. */
export function chatTitle(text: string): string {
  const line = text.trim().split(/\r?\n/).find(l => l.trim()) ?? "";
  const chars = [...line.trim()];
  return chars.length > 40 ? `${chars.slice(0, 40).join("")}…` : line.trim() || "新对话";
}

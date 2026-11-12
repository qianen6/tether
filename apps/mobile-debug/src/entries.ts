// What the drawer lists, after Remodex's sidebar (SidebarThreadGrouping): the computer's
// Codex conversations and the phone's sessions as one list, chats apart from project work,
// project work grouped by the computer's projects (a name, one or more folders), else by
// folder. A session that continues or started a conversation shows
// as that conversation. No React Native imports, so vitest runs it.
import type { HistoryThread, ProjectInfo, SessionInfo } from "@tether/client";

export interface Entry {
  key: string;
  title: string;
  /** Unix ms of the last change. */
  updated: number;
  chat: boolean;
  cwd: string;
  /** The computer's conversation, when there is one. */
  thread?: HistoryThread;
  /** The phone session on it, when one is live. */
  session?: SessionInfo;
}

export interface Group {
  key: string;
  label: string;
  /** Where a new conversation in it starts. */
  cwd: string;
  /** The computer's project, when the work is in one. */
  project?: ProjectInfo;
  entries: Entry[];
  updated: number;
}

/** A folder as one key: no `\\?\`, one kind of slash, no trailing slash, any case. */
export function folderKey(cwd: string): string {
  return cwd.replace(/^\\\\\?\\/, "").replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

export function folderName(cwd: string): string {
  const parts = cwd.replace(/^\\\\\?\\/, "").split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? cwd;
}

/** A conversation counts as begun by a session when it started at most this long before. */
const SLACK_MS = 15_000;

/**
 * Pairs sessions with conversations: the conversation the computer says a session is on;
 * else a saved link (a continued conversation); else, for the Codex agent, the earliest
 * unclaimed conversation in the same folder begun after the session.
 */
export function pairSessions(
  threads: HistoryThread[],
  sessions: SessionInfo[],
  links: Record<string, string>,
  codex?: string,
): Map<string, string> {
  const paired = new Map<string, string>();
  const claimed = new Set<string>();
  const ids = new Set(threads.map(t => t.id));
  for (const s of sessions) {
    if (s.thread && ids.has(s.thread) && !claimed.has(s.thread)) {
      paired.set(s.id, s.thread);
      claimed.add(s.thread);
    }
  }
  for (const s of sessions) {
    if (paired.has(s.id)) continue;
    const linked = links[s.id];
    if (linked && ids.has(linked) && !claimed.has(linked)) {
      paired.set(s.id, linked);
      claimed.add(linked);
    }
  }
  const byAge = sessions.filter(s => !paired.has(s.id) && s.agent === codex).sort((a, b) => a.created_at - b.created_at);
  for (const s of byAge) {
    const folder = folderKey(s.cwd);
    const match = threads
      .filter(t => !claimed.has(t.id) && folderKey(t.cwd) === folder && t.created_at >= s.created_at - SLACK_MS)
      .sort((a, b) => a.created_at - b.created_at)[0];
    if (match) {
      paired.set(s.id, match.id);
      claimed.add(match.id);
    }
  }
  return paired;
}

/** Sessions settle this long before their conversation counts as missing from the list. */
const SETTLE_MS = 15_000;

/**
 * Sessions on a conversation of `codex` the computer's list leaves out: archived there, or
 * kept under another model provider than the one Codex now uses (Codex lists only the
 * current provider's, as the desktop app does). The list is the computer's first page,
 * which arrived at `loadedAt` (0: not yet, or a search): a conversation newer than the
 * oldest on it would be on it. A session at work or waiting for you stays.
 */
export function hiddenSessions(
  threads: HistoryThread[],
  sessions: SessionInfo[],
  codex: string | undefined,
  loadedAt: number,
  complete: boolean,
): Set<string> {
  const hidden = new Set<string>();
  if (!codex || !loadedAt || !threads.length) return hidden;
  const listed = new Set(threads.map(t => t.id));
  const oldest = Math.min(...threads.map(t => t.updated_at));
  for (const s of sessions) {
    if (s.agent !== codex || !s.thread || listed.has(s.thread)) continue;
    if (s.state === "working" || s.state === "needs_you") continue;
    if (s.updated_at > loadedAt - SETTLE_MS) continue;
    if (complete || s.updated_at >= oldest) hidden.add(s.id);
  }
  return hidden;
}

/** Every conversation and session once, newest first. Closed sessions are left out. */
export function entries(threads: HistoryThread[], sessions: SessionInfo[], paired: Map<string, string>): Entry[] {
  const live = sessions.filter(s => s.state !== "exited");
  const onThread = new Map<string, SessionInfo>();
  for (const s of live) {
    const id = paired.get(s.id);
    if (id) onThread.set(id, s);
  }
  const out: Entry[] = threads.map(t => {
    const s = onThread.get(t.id);
    return {
      key: `t:${t.id}`,
      title: t.title || s?.title || "对话",
      updated: Math.max(t.updated_at, s?.updated_at ?? 0),
      chat: t.chat,
      cwd: t.cwd,
      thread: t,
      session: s,
    };
  });
  for (const s of live) {
    if (paired.has(s.id)) continue;
    out.push({ key: `s:${s.id}`, title: s.title || s.agent, updated: s.updated_at, chat: s.chat, cwd: s.cwd, session: s });
  }
  return out.sort((a, b) => b.updated - a.updated);
}

/**
 * The project an entry belongs to: the one the computer filed it under, else the one whose
 * folder holds it (the deepest folder; on a tie the first project listed).
 */
export function projectOf(e: Entry, projects: ProjectInfo[]): ProjectInfo | undefined {
  const id = e.thread?.project;
  const filed = id ? projects.find(p => p.id === id) : undefined;
  if (filed) return filed;
  const here = folderKey(e.cwd);
  let best: ProjectInfo | undefined;
  let depth = -1;
  for (const p of projects) {
    for (const root of p.roots) {
      const r = folderKey(root);
      if ((here === r || here.startsWith(`${r}/`)) && r.length > depth) {
        best = p;
        depth = r.length;
      }
    }
  }
  return best;
}

/** Project work by project (else by folder), the most recently active first. */
export function groups(list: Entry[], projects: ProjectInfo[] = []): Group[] {
  const map = new Map<string, Group>();
  for (const e of list) {
    if (e.chat) continue;
    const project = projectOf(e, projects);
    const key = project ? `p:${project.id}` : `f:${folderKey(e.cwd)}`;
    let g = map.get(key);
    if (!g) {
      g = project
        ? { key, label: project.name, cwd: project.roots[0] ?? e.cwd, project, entries: [], updated: 0 }
        : { key, label: folderName(e.cwd), cwd: e.cwd, entries: [], updated: 0 };
      map.set(key, g);
    }
    g.entries.push(e);
    g.updated = Math.max(g.updated, e.updated);
  }
  return [...map.values()].sort((a, b) => b.updated - a.updated);
}

/** Matches typed text against title, preview and folder. */
export function matches(e: Entry, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [e.title, e.thread?.preview ?? "", e.cwd].some(x => x.toLowerCase().includes(q));
}

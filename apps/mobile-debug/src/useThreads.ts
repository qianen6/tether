// The conversations the computer's agent (Codex, DSH) keeps, wherever they were started (its desktop
// app, the command line, this phone). The phone's copy shows at once; the computer's list
// replaces it when connected.
import { useCallback, useEffect, useRef, useState } from "react";
import type { HistoryThread, ProjectInfo } from "@tether/client";
import { history } from "./files";
import { link } from "./link";
import { useLink } from "./useLink";

/** The first page for `agent` and the computer's projects, saved copy first; `more` loads the next. */
export function useThreads(agent: string | undefined, search = "") {
  const snap = useLink();
  const [threads, setThreads] = useState<HistoryThread[]>([]);
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  /** When the computer's list last arrived (phone clock), for `hiddenSessions`. */
  const [loadedAt, setLoadedAt] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const open = snap.phase === "open";
  /** The agent shown now: an answer for another one (switched away meanwhile) is dropped. */
  const shown = useRef(agent);
  shown.current = agent;

  // Another agent: nothing of the last one's stays on screen.
  useEffect(() => {
    setThreads([]);
    setProjects([]);
    setCursor(null);
    setLoadedAt(0);
    setError(undefined);
  }, [agent]);

  const load = useCallback(async () => {
    const client = link.client;
    if (!agent || !client || link.snapshot.phase !== "open") return;
    setLoading(true);
    try {
      const page = await client.listHistory(agent, search ? { search } : undefined);
      if (shown.current !== agent) return;
      setThreads(page.threads);
      setCursor(page.next_cursor ?? null);
      setLoadedAt(search ? 0 : Date.now());
      setError(undefined);
      if (!search) void history.saveThreads(agent, page.threads);
      // An older computer sends none; keep what was known.
      if (page.projects?.length) {
        setProjects(page.projects);
        void history.saveProjects(agent, page.projects);
      }
    } catch (e) {
      if (shown.current === agent) setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [agent, search]);

  useEffect(() => {
    if (!agent || search) return;
    void history.threads(agent).then(saved => saved && shown.current === agent && setThreads(t => (t.length ? t : saved)));
    void history.projects(agent).then(saved => saved && shown.current === agent && setProjects(p => (p.length ? p : saved)));
  }, [agent, search]);
  useEffect(() => {
    // Typing settles before asking the computer.
    const id = setTimeout(() => void load(), search ? 400 : 0);
    return () => clearTimeout(id);
  }, [load, open]);

  const more = async () => {
    const client = link.client;
    if (!agent || !client || !cursor || loading) return;
    setLoading(true);
    try {
      const page = await client.listHistory(agent, { ...(search ? { search } : {}), cursor });
      if (shown.current !== agent) return;
      setThreads(t => [...t, ...page.threads.filter(x => !t.some(y => y.id === x.id))]);
      setCursor(page.next_cursor ?? null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };
  return { threads, projects, loading, error, more: cursor ? more : undefined, reload: load, complete: !cursor, loadedAt };
}

// The drawer, after Remodex's SidebarView: the app's name with the computer's connection,
// a search field, "新对话", then the computer's projects (each with its conversations and
// a button for a new one in it), the chats, the terminals, and at the bottom which
// computer it is connected to.
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { ActivityIndicator, Alert, Pressable, RefreshControl, ScrollView, TextInput, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import type { ProjectInfo, SessionInfo, TermInfo } from "@tether/client";
import { history } from "./files";
import { link } from "./link";
import { useLink } from "./useLink";
import { useThreads } from "./useThreads";
import { entries, groups, hiddenSessions, matches, pairSessions, type Entry } from "./entries";
import type { Opening } from "./ChatScreen";
import { CircleButton, duration, Icon, Menu, ProgramIcon, programOf, shortPath, Spinner, StatusMark, T, type IconName } from "./kit";
import { alpha, FONT, useTheme } from "./theme";

const SHOWN = 5;
const CHATS_SHOWN = 8;

export function Sidebar(props: {
  visible: boolean;
  codex?: string;
  /** The open conversation: its session id or thread id. */
  current?: string;
  onClose: () => void;
  onOpen: (o: Opening) => void;
  onNewChat: () => void;
  onNewInProject: (cwd: string, project?: ProjectInfo) => void;
  onPickProject: (projects: ProjectInfo[]) => void;
  onNewSession: () => void;
  onTerminal: (term?: string) => void;
  onNewTerminal: () => void;
  onSettings: () => void;
}) {
  const t = useTheme();
  const insets = useSafeAreaInsets();
  const snap = useLink();
  const open = snap.phase === "open";
  const [search, setSearch] = useState("");
  const [menu, setMenu] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [refreshing, setRefreshing] = useState(false);
  const [terms, setTerms] = useState<TermInfo[]>([]);
  const [links, setLinks] = useState<Record<string, string>>({});
  const [now, setNow] = useState(Date.now());
  const query = search.trim();
  const { threads, projects, loading, error, more, reload, complete, loadedAt } = useThreads(props.codex, query);

  const loadTerms = useCallback(() => {
    if (link.snapshot.phase === "open") void link.client?.listTerminals().then(setTerms).catch(() => {});
  }, []);
  useEffect(() => {
    if (!props.visible) return;
    setNow(Date.now());
    loadTerms();
    void reload();
  }, [props.visible, snap.phase]);
  useEffect(() => {
    void history.links().then(setLinks);
  }, [snap.sessions]);

  const list = useMemo(() => {
    // Archived on the computer, or under the provider it switched away from: gone, as there.
    const hidden = hiddenSessions(threads, snap.sessions, props.codex, loadedAt, complete);
    const sessions = hidden.size ? snap.sessions.filter(s => !hidden.has(s.id)) : snap.sessions;
    const paired = pairSessions(threads, sessions, links, props.codex);
    return entries(threads, sessions, paired)
      // The computer already searched its conversations; sessions match here.
      .filter(e => !!e.thread || matches(e, query));
  }, [threads, snap.sessions, links, props.codex, query, loadedAt, complete]);
  const waiting = list.filter(e => e.session?.state === "needs_you");
  const rest = list.filter(e => e.session?.state !== "needs_you");
  const chats = rest.filter(e => e.chat);
  const folders = groups(rest, projects);
  const shownTerms = terms.filter(x => x.running && (!query || x.command.join(" ").toLowerCase().includes(query.toLowerCase())));

  const refresh = () => {
    setRefreshing(true);
    loadTerms();
    void Promise.all([reload(), link.refresh().catch(() => {})]).finally(() => {
      setNow(Date.now());
      setRefreshing(false);
    });
  };
  const openEntry = (e: Entry) => {
    if (e.session) props.onOpen({ session: e.session.id });
    else if (e.thread && props.codex) props.onOpen({ agent: props.codex, thread: e.thread });
  };
  const selected = (e: Entry) => !!props.current && (props.current === e.session?.id || props.current === e.thread?.id);
  const toggle = (set: Set<string>, key: string) => {
    const next = new Set(set);
    if (!next.delete(key)) next.add(key);
    return next;
  };
  const row = (e: Entry, inProject = false) => (
    <EntryRow key={e.key} e={e} now={now} codex={props.codex} indent={inProject ? 36 : 12} selected={selected(e)} done={!!e.session && snap.done.includes(e.session.id)} onPress={() => openEntry(e)} />
  );
  const canStart = open && !!props.codex;
  const chatsOpen = expanded.has("chats");

  return (
    <View style={{ flex: 1 }}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 10, paddingLeft: 18, paddingRight: 12, paddingTop: 10, paddingBottom: 8 }}>
        <View style={{ width: 26, height: 26, borderRadius: 8, backgroundColor: t.primary, alignItems: "center", justifyContent: "center" }}>
          <Icon name="sparkles" size={15} color={t.primaryForeground} />
        </View>
        <T size={21} weight="700" style={{ flex: 1 }}>Tether</T>
        {loading ? <ActivityIndicator size="small" color={t.sidebarForeground} /> : null}
        <CircleButton name="more" label="更多" onPress={() => setMenu(true)} />
      </View>

      <View style={{ flexDirection: "row", alignItems: "center", gap: 6, height: 40, marginHorizontal: 16, marginBottom: 6, paddingLeft: 12, paddingRight: 8, borderRadius: 12, backgroundColor: t.muted }}>
        <Icon name="search" size={16} color={t.sidebarForeground} />
        <TextInput
          value={search}
          onChangeText={setSearch}
          placeholder="搜索对话"
          placeholderTextColor={t.sidebarForeground}
          autoCorrect={false}
          autoCapitalize="none"
          returnKeyType="search"
          style={{ flex: 1, paddingVertical: 0, color: t.foreground, fontFamily: FONT, fontSize: 15.5 }}
        />
        {search ? (
          <Pressable accessibilityLabel="清除搜索" onPress={() => setSearch("")} hitSlop={8}>
            <Icon name="x" size={15} color={t.sidebarForeground} />
          </Pressable>
        ) : null}
      </View>
      <Banner />

      <ScrollView
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingBottom: 16 }}
        keyboardShouldPersistTaps="handled"
        refreshControl={<RefreshControl refreshing={refreshing} onRefresh={refresh} />}
      >
        <ActionRow icon="square-pen" label="新对话" disabled={!props.codex} onPress={props.onNewChat} />
        {error && !threads.length ? <T size={12.5} color={t.destructive} style={{ paddingHorizontal: 22, paddingVertical: 6 }}>{error}</T> : null}

        {waiting.length ? (
          <View style={{ paddingBottom: 4 }}>
            <Heading label="需要你处理" color={t.chart2} />
            {waiting.map(e => row(e))}
          </View>
        ) : null}

        <Heading
          label="项目"
          right={projects.length ? (
            <Pressable accessibilityLabel="在项目里开始" disabled={!canStart} onPress={() => props.onPickProject(projects)} hitSlop={8}
              style={({ pressed }) => ({ width: 30, height: 30, borderRadius: 15, alignItems: "center", justifyContent: "center", opacity: canStart ? 1 : 0.35, backgroundColor: pressed ? t.accent : "transparent" })}>
              <Icon name="folder-plus" size={16} color={t.sidebarForeground} />
            </Pressable>
          ) : undefined}
        />
        {folders.length ? (
          folders.map(g => {
            const shut = collapsed.has(g.key);
            const all = expanded.has(g.key);
            return (
              <View key={g.key} style={{ marginBottom: 2 }}>
                <Pressable onPress={() => setCollapsed(c => toggle(c, g.key))}
                  style={({ pressed }) => ({ flexDirection: "row", alignItems: "center", gap: 9, marginHorizontal: 8, paddingLeft: 10, paddingRight: 2, height: 42, borderRadius: 12, backgroundColor: pressed ? t.sidebarAccent : "transparent" })}>
                  <Icon name="folder" size={17} color={t.foreground} />
                  <T size={15.5} weight="600" lines={1} style={{ flexShrink: 1 }}>{g.label}</T>
                  {g.project && g.project.roots.length > 1 ? <T size={11.5} color={t.sidebarForeground}>{`${g.project.roots.length} 个文件夹`}</T> : null}
                  <View style={{ flex: 1 }} />
                  <Pressable accessibilityLabel={`在 ${g.label} 里新建`} disabled={!canStart} onPress={() => props.onNewInProject(g.cwd, g.project)} hitSlop={6}
                    style={({ pressed }) => ({ width: 32, height: 32, alignItems: "center", justifyContent: "center", borderRadius: 16, opacity: canStart ? 1 : 0.35, backgroundColor: pressed ? t.accent : t.muted })}>
                    <Icon name="plus" size={15} color={t.foreground} />
                  </Pressable>
                </Pressable>
                {shut ? null : (all ? g.entries : g.entries.slice(0, SHOWN)).map(e => row(e, true))}
                {!shut && g.entries.length > SHOWN ? (
                  <Pressable onPress={() => setExpanded(c => toggle(c, g.key))} hitSlop={6} style={{ paddingLeft: 40, paddingVertical: 9 }}>
                    <T size={13} color={t.sidebarForeground}>{all ? "收起" : `显示更多（${g.entries.length - SHOWN}）`}</T>
                  </Pressable>
                ) : null}
              </View>
            );
          })
        ) : (
          <T size={13} color={t.sidebarForeground} style={{ paddingHorizontal: 22, paddingVertical: 6, lineHeight: 19 }}>
            {query ? "没有找到" : "电脑上 Codex 里的项目会列在这里。"}
          </T>
        )}

        <Heading label="对话" />
        {chats.length ? (
          <>
            {(chatsOpen ? chats : chats.slice(0, CHATS_SHOWN)).map(e => row(e))}
            {chats.length > CHATS_SHOWN ? (
              <Pressable onPress={() => setExpanded(c => toggle(c, "chats"))} hitSlop={6} style={{ paddingLeft: 22, paddingVertical: 9 }}>
                <T size={13} color={t.sidebarForeground}>{chatsOpen ? "收起" : `显示更多（${chats.length - CHATS_SHOWN}）`}</T>
              </Pressable>
            ) : null}
          </>
        ) : (
          <T size={13} color={t.sidebarForeground} style={{ paddingHorizontal: 22, paddingVertical: 6 }}>{query ? "没有找到" : "还没有对话"}</T>
        )}

        {shownTerms.length ? (
          <>
            <Heading label="终端" />
            {shownTerms.map(x => (
              <Pressable key={x.id} onPress={() => props.onTerminal(x.id)}
                style={({ pressed }) => ({ flexDirection: "row", alignItems: "center", gap: 9, marginHorizontal: 8, paddingHorizontal: 12, paddingVertical: 10, borderRadius: 12, backgroundColor: pressed ? t.sidebarAccent : "transparent" })}>
                <ProgramIcon program={programOf(x.command)} size={15} />
                <View style={{ flex: 1 }}>
                  <T size={15} lines={1}>{x.command.join(" ")}</T>
                  <T size={12} color={t.sidebarForeground} lines={1}>{shortPath(x.cwd)}</T>
                </View>
              </Pressable>
            ))}
          </>
        ) : null}

        {list.some(e => e.session) ? (
          <T size={11.5} color={t.sidebarForeground} style={{ textAlign: "center", paddingTop: 14, paddingHorizontal: 22 }}>长按正在手机上进行的对话可以关闭它</T>
        ) : null}

        {more ? (
          <Pressable onPress={() => void more()} style={({ pressed }) => ({ marginHorizontal: 16, marginTop: 10, height: 38, alignItems: "center", justifyContent: "center", borderRadius: 19, backgroundColor: pressed ? t.accent : t.muted })}>
            <T size={13} color={t.mutedForeground}>加载更早的对话</T>
          </Pressable>
        ) : null}
      </ScrollView>

      <Pressable onPress={props.onSettings} style={{ alignItems: "center", paddingTop: 8, paddingBottom: 10, gap: 1 }}>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 5 }}>
          <View style={{ width: 6, height: 6, borderRadius: 3, backgroundColor: open ? t.ok : t.chart4 }} />
          <T size={12} color={t.sidebarForeground}>{open ? "已连接到电脑" : "未连接"}</T>
        </View>
        <T size={12.5} weight="500" color={t.mutedForeground} lines={1}>{snap.hostName ?? "电脑"}</T>
      </Pressable>

      <Menu
        visible={menu}
        onClose={() => setMenu(false)}
        top={insets.top + 58}
        side="right"
        items={[
          { label: "新对话", icon: "square-pen", onPress: props.onNewChat, disabled: !props.codex },
          { label: "在项目里开始", icon: "folder-plus", onPress: () => props.onPickProject(projects), disabled: !canStart || !projects.length },
          { label: "其他 agent / 文件夹", icon: "folder", onPress: props.onNewSession, disabled: !open },
          { label: "打开终端", icon: "terminal", onPress: props.onNewTerminal, disabled: !open },
          { label: "设置", icon: "settings", onPress: props.onSettings },
        ]}
      />
    </View>
  );
}

function ActionRow({ icon, label, onPress, disabled }: { icon: IconName; label: string; onPress: () => void; disabled?: boolean }) {
  const t = useTheme();
  return (
    <Pressable onPress={onPress} disabled={disabled}
      style={({ pressed }) => ({ flexDirection: "row", alignItems: "center", gap: 10, marginHorizontal: 8, paddingHorizontal: 10, height: 44, borderRadius: 12, opacity: disabled ? 0.4 : 1, backgroundColor: pressed ? t.sidebarAccent : "transparent" })}>
      <Icon name={icon} size={18} color={t.foreground} />
      <T size={15.5} weight="500">{label}</T>
    </Pressable>
  );
}

/** One conversation or session: its title, and on the right what it is doing or how long ago. */
function EntryRow({ e, now, codex, indent, selected, done, onPress }: { e: Entry; now: number; codex?: string; indent: number; selected: boolean; done: boolean; onPress: () => void }) {
  const t = useTheme();
  const s = e.session;
  const other = s && s.agent !== codex;
  const close = (session: SessionInfo) =>
    Alert.alert(e.title, "关闭这个会话？电脑上的 agent 会停止，对话记录还在电脑上。", [
      { text: "取消", style: "cancel" },
      {
        text: "关闭",
        style: "destructive",
        onPress: () => {
          const client = link.client;
          if (!client) return Alert.alert("未连接电脑", "连上电脑后再关闭。");
          void client.closeSession(session.id).catch(e => Alert.alert("没有关掉", e instanceof Error ? e.message : String(e)));
        },
      },
    ]);
  return (
    <Pressable onPress={onPress} onLongPress={s ? () => close(s) : undefined} accessibilityHint={s ? "长按可以关闭这个会话" : undefined}
      style={({ pressed }) => ({
        flexDirection: "row", alignItems: "center", gap: 8, marginHorizontal: 8, paddingLeft: indent, paddingRight: 12, paddingVertical: 10, borderRadius: 12,
        backgroundColor: selected ? t.fill : pressed ? t.sidebarAccent : "transparent",
      })}>
      {other ? <ProgramIcon program={programOf(s.agent)} size={14} /> : null}
      <T size={14.5} lines={1} style={{ flex: 1 }}>{e.title}</T>
      {s?.state === "working" ? (
        <Spinner size={14} color={t.mutedForeground} />
      ) : s?.state === "needs_you" ? (
        <StatusMark state="needs_you" />
      ) : done ? (
        <View style={{ width: 8, height: 8, borderRadius: 4, backgroundColor: t.chart3 }} />
      ) : (
        <T size={12} color={t.sidebarForeground}>{duration(Math.max(0, now - e.updated))}</T>
      )}
    </Pressable>
  );
}

function Heading({ label, color, right }: { label: string; color?: string; right?: ReactNode }) {
  const t = useTheme();
  return (
    <View style={{ flexDirection: "row", alignItems: "center", paddingLeft: 20, paddingRight: 14, paddingTop: 12, paddingBottom: 2, minHeight: 38 }}>
      <T size={12.5} weight="600" color={color ?? t.sidebarForeground} style={{ flex: 1 }}>{label}</T>
      {right}
    </View>
  );
}

/** Says when the computer is out of reach, and offers to try now. */
function Banner() {
  const t = useTheme();
  const snap = useLink();
  if (snap.phase === "open") return null;
  const failed = snap.phase === "failed";
  return (
    <Pressable onPress={() => void link.retry()}
      style={{ marginHorizontal: 16, marginBottom: 6, flexDirection: "row", alignItems: "center", gap: 10, borderRadius: 12, backgroundColor: alpha(t.chart2, 0.08), paddingHorizontal: 14, paddingVertical: 10 }}>
      {failed ? <View style={{ width: 6, height: 6, borderRadius: 3, backgroundColor: t.chart2 }} /> : <ActivityIndicator size="small" color={t.chart2} />}
      <View style={{ flex: 1 }}>
        <T size={13.5} weight="500" color={t.chart2}>{failed ? "连不上电脑" : snap.phase === "reconnecting" ? "重连中…" : "连接中…"}</T>
        {failed && snap.error ? <T size={12} color={t.mutedForeground} lines={3}>{snap.error}</T> : null}
      </View>
      {failed ? <T size={13.5} weight="500">重试</T> : null}
    </Pressable>
  );
}

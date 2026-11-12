// Tether on the phone, laid out as Remodex (github.com/Emanuele-web04/remodex): pair, then
// a conversation fills the screen (a new chat with Codex or DSH to start with), and the
// drawer on the left lists, for the agent chosen at its top, the computer's projects with
// their conversations, the chats, and the terminals. On a tablet the drawer stays open
// beside the conversation.
import { useEffect, useMemo, useState, type ReactNode } from "react";
import { ActivityIndicator, BackHandler, ScrollView, StyleSheet, useWindowDimensions, View } from "react-native";
import { SafeAreaProvider, useSafeAreaInsets } from "react-native-safe-area-context";
import { StatusBar } from "expo-status-bar";
import * as Notifications from "expo-notifications";
import { link, openLaunchNotification } from "./src/link";
import { useLink } from "./src/useLink";
import { ChatScreen, type Opening } from "./src/ChatScreen";
import { TerminalScreen } from "./src/TerminalScreen";
import { Drawer } from "./src/Drawer";
import { Sidebar } from "./src/Sidebar";
import { NewSessionSheet, NewTerminalSheet, ProjectSheet, SettingsSheet, useAgents } from "./src/sheets";
import type { ProjectInfo } from "@tether/client";
import { history } from "./src/files";
import { families, familyOf, type Family, type FamilyKey } from "./src/agents";
import { AgentsContext, PaneContext } from "./src/context";
import { Button, CircleButton, Icon, T } from "./src/kit";
import { alpha, useTheme } from "./src/theme";

type Main =
  | { name: "home" }
  | { name: "chat"; opening: Opening; n: number }
  | { name: "terminal"; term?: string; agent?: string };

/** Each opening its own screen, so state never leaks from one to the next. */
function openingKey(o: Opening, n: number): string {
  if ("session" in o) return `s:${o.session}`;
  if ("thread" in o) return `t:${o.agent}:${o.thread.id}`;
  return "cwd" in o ? `p:${o.agent}:${o.project?.id ?? o.cwd}:${n}` : `c:${o.agent}:${n}`;
}

export default function App() {
  return (
    <SafeAreaProvider>
      <Root />
    </SafeAreaProvider>
  );
}

function Root() {
  const t = useTheme();
  const insets = useSafeAreaInsets();
  const snap = useLink();

  useEffect(() => {
    void Notifications.requestPermissionsAsync();
    void link.start().then(openLaunchNotification);
  }, []);

  const paired = !["loading", "unpaired", "pairing"].includes(snap.phase);
  let body: ReactNode;
  if (snap.phase === "loading") body = <View style={{ flex: 1, alignItems: "center", justifyContent: "center" }}><ActivityIndicator color={t.mutedForeground} /></View>;
  else if (!paired) body = <PairScreen />;
  else body = <Shell />;
  return (
    <View style={{ flex: 1, backgroundColor: t.background, paddingTop: insets.top, paddingBottom: insets.bottom }}>
      <StatusBar style="auto" />
      {body}
    </View>
  );
}

/** At least this wide (a tablet, or a big phone on its side), the sidebar stays beside the content. */
const WIDE = 768;

function Shell() {
  const t = useTheme();
  const snap = useLink();
  const open = snap.phase === "open";
  const agents = useAgents(true);
  const { width } = useWindowDimensions();
  const wide = width >= WIDE;
  const side = wide ? Math.min(380, Math.max(300, Math.round(width * 0.3))) : 0;

  // Codex and DSH each keep their own conversations: the drawer shows one family at a time.
  const live = useMemo(() => families(agents, snap.sessions), [agents, snap.sessions]);
  const liveKey = JSON.stringify(live);
  const [saved, setSaved] = useState<{ list: Family[]; current?: FamilyKey }>({ list: [] });
  const [chosen, setChosen] = useState<FamilyKey>();
  const list = live.length ? live : saved.list;
  const family = list.find(f => f.key === (chosen ?? saved.current)) ?? list[0];
  const agent = family?.agent;

  const [main, setMain] = useState<Main>({ name: "home" });
  const [prev, setPrev] = useState<Main>({ name: "home" });
  const [current, setCurrent] = useState<string>();
  const [drawer, setDrawer] = useState(false);
  const [sheet, setSheet] = useState<
    { name: "session"; cwd?: string } | { name: "terminal" } | { name: "settings" } | { name: "projects"; projects: ProjectInfo[] }
  >();

  useEffect(() => {
    void history.families().then(setSaved);
  }, []);
  useEffect(() => {
    if (live.length) void history.saveFamilies(live, family?.key);
  }, [liveKey, family?.key]);
  useEffect(() => {
    if (open) void link.refresh().then(() => history.prune(link.snapshot.sessions.map(s => s.id))).catch(() => {});
  }, [open]);

  /** The family a conversation belongs to, when the drawer has a place for it. */
  const familyOfOpening = (o: Opening): FamilyKey | undefined => {
    const id = "agent" in o ? o.agent : link.snapshot.sessions.find(s => s.id === o.session)?.agent;
    const key = id ? familyOf(id, agents) : undefined;
    return key && list.some(f => f.key === key) ? key : undefined;
  };
  const show = (next: Main) => {
    if (next.name === "terminal") setPrev(main.name === "terminal" ? prev : main);
    if (next.name === "chat") {
      const key = familyOfOpening(next.opening);
      if (key) setChosen(key);
    }
    setMain(next);
    setCurrent(next.name === "chat" ? ("session" in next.opening ? next.opening.session : "thread" in next.opening ? next.opening.thread.id : undefined) : undefined);
    setDrawer(false);
  };
  const openChat = (opening: Opening) => show({ name: "chat", opening, n: Date.now() });
  const newChat = () => (agent ? openChat({ agent, chat: true }) : show({ name: "home" }));
  /** Another family's list; a conversation of the one left gives way to a new chat in this one. */
  const pickFamily = (key: FamilyKey) => {
    setChosen(key);
    const next = list.find(f => f.key === key);
    if (main.name === "chat" && next?.agent && familyOfOpening(main.opening) !== key) {
      setMain({ name: "chat", opening: { agent: next.agent, chat: true }, n: Date.now() });
      setCurrent(undefined);
    }
  };

  // The first screen is a new chat, once there is an agent to talk to.
  useEffect(() => {
    if (agent && main.name === "home") setMain({ name: "chat", opening: { agent, chat: true }, n: Date.now() });
  }, [agent]);
  useEffect(() => {
    link.onOpenSession = session => openChat({ session });
  }, [agent, main, liveKey]);

  // Android's back closes the drawer, then a terminal, before it leaves the app.
  useEffect(() => {
    const sub = BackHandler.addEventListener("hardwareBackPress", () => {
      if (drawer && !wide) {
        setDrawer(false);
        return true;
      }
      if (main.name === "terminal") {
        setMain(prev);
        return true;
      }
      return false;
    });
    return () => sub.remove();
  }, [drawer, main, prev, wide]);

  // On a wide screen the sidebar is always there: no button to open it.
  const onMenu = wide ? undefined : () => setDrawer(true);
  let body: ReactNode;
  if (main.name === "chat") {
    body = (
      <ChatScreen key={openingKey(main.opening, main.n)} opening={main.opening} onMenu={onMenu} onNewChat={newChat} onSession={setCurrent} />
    );
  } else if (main.name === "terminal") body = <TerminalScreen term={main.term} agent={main.agent} onBack={() => setMain(prev)} />;
  else body = <Home onMenu={onMenu} />;

  const sidebar = (
    <Sidebar
      visible={wide || drawer}
      wide={wide}
      width={side}
      families={list}
      family={family?.key}
      agent={agent}
      onFamily={pickFamily}
      current={main.name === "chat" ? current : undefined}
      onClose={() => setDrawer(false)}
      onOpen={openChat}
      onNewChat={agent ? newChat : () => setSheet({ name: "session" })}
      onNewInProject={(cwd, project) => agent && openChat({ agent, cwd, project })}
      onPickProject={projects => setSheet({ name: "projects", projects })}
      onNewSession={() => setSheet({ name: "session" })}
      onTerminal={term => show({ name: "terminal", term })}
      onNewTerminal={() => setSheet({ name: "terminal" })}
      onSettings={() => setSheet({ name: "settings" })}
    />
  );

  return (
    <AgentsContext.Provider value={agents}>
      <PaneContext.Provider value={{ left: side, width: width - side, wide }}>
        <View style={{ flex: 1, flexDirection: "row" }}>
          {wide ? (
            <View style={{ width: side, backgroundColor: t.sidebar, borderRightWidth: StyleSheet.hairlineWidth, borderColor: t.border }}>{sidebar}</View>
          ) : null}
          <View style={{ flex: 1 }}>
            <Drawer open={drawer && !wide} disabled={wide} onOpenChange={setDrawer} panel={wide ? null : sidebar}>
              {body}
            </Drawer>
          </View>
        </View>
        <NewSessionSheet
          visible={sheet?.name === "session"}
          cwd={sheet?.name === "session" ? sheet.cwd : undefined}
          onClose={() => setSheet(undefined)}
          onCreated={id => openChat({ session: id })}
          onChat={agent ? () => { setSheet(undefined); newChat(); } : undefined}
        />
        <NewTerminalSheet visible={sheet?.name === "terminal"} onClose={() => setSheet(undefined)} onPick={agent => show({ name: "terminal", agent })} />
        <SettingsSheet visible={sheet?.name === "settings"} onClose={() => setSheet(undefined)} />
        <ProjectSheet
          visible={sheet?.name === "projects"}
          projects={sheet?.name === "projects" ? sheet.projects : []}
          label={family?.label}
          onClose={() => setSheet(undefined)}
          onPick={p => agent && openChat({ agent, cwd: p.roots[0] ?? "", project: p })}
        />
      </PaneContext.Provider>
    </AgentsContext.Provider>
  );
}

/** Remodex's home before there is a conversation: the app, whether the computer is there, and the way in. */
function Home({ onMenu }: { onMenu?: () => void }) {
  const t = useTheme();
  const snap = useLink();
  const open = snap.phase === "open";
  const busy = snap.phase === "connecting" || snap.phase === "reconnecting";
  return (
    <View style={{ flex: 1 }}>
      <View style={{ paddingHorizontal: 12, paddingVertical: 8 }}>
        {onMenu ? <CircleButton name="menu" label="打开侧边栏" onPress={onMenu} /> : <View style={{ height: 40 }} />}
      </View>
      <View style={{ flex: 1, alignItems: "center", justifyContent: "center", gap: 18, paddingHorizontal: 40, alignSelf: "center", width: "100%", maxWidth: 520 }}>
        <View style={{ width: 88, height: 88, borderRadius: 24, backgroundColor: t.primary, alignItems: "center", justifyContent: "center" }}>
          <Icon name="terminal" size={40} color={t.primaryForeground} />
        </View>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 6, paddingHorizontal: 14, paddingVertical: 7, borderRadius: 20, borderWidth: 1, borderColor: t.border }}>
          {busy ? <ActivityIndicator size="small" color={t.chart1} /> : <View style={{ width: 6, height: 6, borderRadius: 3, backgroundColor: open ? t.ok : t.chart4 }} />}
          <T size={12.5} weight="500" color={t.mutedForeground}>{open ? `已连接 ${snap.hostName ?? "电脑"}` : busy ? "正在连接电脑…" : "未连接"}</T>
        </View>
        <T size={13} color={t.mutedForeground} style={{ textAlign: "center", lineHeight: 19 }}>
          {open ? "电脑上没有找到可用的 Codex 或 DSH。可以从侧边栏打开终端，或在文件夹里用其他 agent 开始。" : snap.error ?? "确认电脑上 Tether 正在运行。"}
        </T>
        <Button label={open ? (onMenu ? "打开侧边栏" : "在左侧选择") : "重试连接"} onPress={open ? onMenu ?? (() => {}) : () => void link.retry()} style={{ alignSelf: "stretch", borderRadius: 18 }} />
      </View>
    </View>
  );
}

function PairScreen() {
  const t = useTheme();
  const snap = useLink();
  const busy = snap.phase === "pairing";
  return (
    <ScrollView style={{ flex: 1 }} contentContainerStyle={{ flexGrow: 1, justifyContent: "center", paddingHorizontal: 32, paddingVertical: 40, gap: 14, width: "100%", maxWidth: 560, alignSelf: "center" }}>
      <View style={{ alignItems: "center", gap: 16, marginBottom: 8 }}>
        <View style={{ width: 88, height: 88, borderRadius: 24, backgroundColor: t.primary, alignItems: "center", justifyContent: "center" }}>
          <Icon name="terminal" size={40} color={t.primaryForeground} />
        </View>
        <T size={26} weight="600">连接你的电脑</T>
        <T size={14.5} color={t.mutedForeground} style={{ textAlign: "center", lineHeight: 21 }}>
          手机通过加密隧道连到电脑上的 Tether，在手机上和电脑里的 Codex、DSH 对话、批准操作、打开终端。
        </T>
      </View>
      <View style={{ gap: 8, borderRadius: 18, backgroundColor: t.muted, padding: 16 }}>
        <T size={13} weight="500">在电脑上</T>
        <T size={13} color={t.mutedForeground}>从开始菜单打开 Tether，窗口里会显示配对二维码；或在终端里运行：</T>
        <T size={12.5} mono selectable>tether serve</T>
        <T size={12.5} mono selectable>tether pair</T>
      </View>
      <Button label={busy ? "正在建立隧道并配对…" : "扫码配对"} disabled={busy} onPress={() => void link.pair()} style={{ marginTop: 8, borderRadius: 18 }} />
      {busy ? <ActivityIndicator color={t.mutedForeground} /> : null}
      {snap.error ? (
        <View style={{ borderRadius: 14, backgroundColor: alpha(t.destructive, 0.08), padding: 12 }}>
          <T size={13.5} color={t.destructive} style={{ lineHeight: 20 }}>{snap.error}</T>
        </View>
      ) : null}
    </ScrollView>
  );
}

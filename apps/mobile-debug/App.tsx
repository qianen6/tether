// Tether on the phone, laid out as Remodex (github.com/Emanuele-web04/remodex): pair, then
// a conversation fills the screen (a new chat with Codex to start with), and the drawer
// on the left lists the computer's projects with their conversations, the chats, and the
// terminals.
import { useEffect, useState, type ReactNode } from "react";
import { ActivityIndicator, BackHandler, ScrollView, View } from "react-native";
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
import { pickCodex } from "./src/history";
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

function Shell() {
  const snap = useLink();
  const open = snap.phase === "open";
  const agents = useAgents(true);
  const found = pickCodex(agents)?.id;
  const [saved, setSaved] = useState<string>();
  const codex = found ?? saved;
  const [main, setMain] = useState<Main>({ name: "home" });
  const [prev, setPrev] = useState<Main>({ name: "home" });
  const [current, setCurrent] = useState<string>();
  const [drawer, setDrawer] = useState(false);
  const [sheet, setSheet] = useState<
    { name: "session"; cwd?: string } | { name: "terminal" } | { name: "settings" } | { name: "projects"; projects: ProjectInfo[] }
  >();

  useEffect(() => {
    void history.codex().then(setSaved);
  }, []);
  useEffect(() => {
    if (found) void history.saveCodex(found);
  }, [found]);
  useEffect(() => {
    if (open) void link.refresh().then(() => history.prune(link.snapshot.sessions.map(s => s.id))).catch(() => {});
  }, [open]);

  const show = (next: Main) => {
    if (next.name === "terminal") setPrev(main.name === "terminal" ? prev : main);
    setMain(next);
    setCurrent(next.name === "chat" ? ("session" in next.opening ? next.opening.session : "thread" in next.opening ? next.opening.thread.id : undefined) : undefined);
    setDrawer(false);
  };
  const openChat = (opening: Opening) => show({ name: "chat", opening, n: Date.now() });
  const newChat = () => (codex ? openChat({ agent: codex, chat: true }) : show({ name: "home" }));

  // The first screen is a new chat, once there is a Codex to talk to.
  useEffect(() => {
    if (codex && main.name === "home") setMain({ name: "chat", opening: { agent: codex, chat: true }, n: Date.now() });
  }, [codex]);
  useEffect(() => {
    link.onOpenSession = session => openChat({ session });
  }, [codex, main]);

  // Android's back closes the drawer, then a terminal, before it leaves the app.
  useEffect(() => {
    const sub = BackHandler.addEventListener("hardwareBackPress", () => {
      if (drawer) {
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
  }, [drawer, main, prev]);

  let body: ReactNode;
  if (main.name === "chat") {
    body = (
      <ChatScreen key={openingKey(main.opening, main.n)} opening={main.opening} onMenu={() => setDrawer(true)} onNewChat={newChat} onSession={setCurrent} />
    );
  } else if (main.name === "terminal") body = <TerminalScreen term={main.term} agent={main.agent} onBack={() => setMain(prev)} />;
  else body = <Home onMenu={() => setDrawer(true)} />;

  return (
    <>
      <Drawer
        open={drawer}
        onOpenChange={setDrawer}
        panel={
          <Sidebar
            visible={drawer}
            codex={codex}
            current={main.name === "chat" ? current : undefined}
            onClose={() => setDrawer(false)}
            onOpen={openChat}
            onNewChat={newChat}
            onNewInProject={(cwd, project) => codex && openChat({ agent: codex, cwd, project })}
            onPickProject={projects => setSheet({ name: "projects", projects })}
            onNewSession={() => setSheet({ name: "session" })}
            onTerminal={term => show({ name: "terminal", term })}
            onNewTerminal={() => setSheet({ name: "terminal" })}
            onSettings={() => setSheet({ name: "settings" })}
          />
        }
      >
        {body}
      </Drawer>
      <NewSessionSheet
        visible={sheet?.name === "session"}
        cwd={sheet?.name === "session" ? sheet.cwd : undefined}
        onClose={() => setSheet(undefined)}
        onCreated={id => openChat({ session: id })}
        onChat={codex ? () => { setSheet(undefined); newChat(); } : undefined}
      />
      <NewTerminalSheet visible={sheet?.name === "terminal"} onClose={() => setSheet(undefined)} onPick={agent => show({ name: "terminal", agent })} />
      <SettingsSheet visible={sheet?.name === "settings"} onClose={() => setSheet(undefined)} />
      <ProjectSheet
        visible={sheet?.name === "projects"}
        projects={sheet?.name === "projects" ? sheet.projects : []}
        onClose={() => setSheet(undefined)}
        onPick={p => codex && openChat({ agent: codex, cwd: p.roots[0] ?? "", project: p })}
      />
    </>
  );
}

/** Remodex's home before there is a conversation: the app, whether the computer is there, and the way in. */
function Home({ onMenu }: { onMenu: () => void }) {
  const t = useTheme();
  const snap = useLink();
  const open = snap.phase === "open";
  const busy = snap.phase === "connecting" || snap.phase === "reconnecting";
  return (
    <View style={{ flex: 1 }}>
      <View style={{ paddingHorizontal: 12, paddingVertical: 8 }}>
        <CircleButton name="menu" label="打开侧边栏" onPress={onMenu} />
      </View>
      <View style={{ flex: 1, alignItems: "center", justifyContent: "center", gap: 18, paddingHorizontal: 40 }}>
        <View style={{ width: 88, height: 88, borderRadius: 24, backgroundColor: t.primary, alignItems: "center", justifyContent: "center" }}>
          <Icon name="terminal" size={40} color={t.primaryForeground} />
        </View>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 6, paddingHorizontal: 14, paddingVertical: 7, borderRadius: 20, borderWidth: 1, borderColor: t.border }}>
          {busy ? <ActivityIndicator size="small" color={t.chart1} /> : <View style={{ width: 6, height: 6, borderRadius: 3, backgroundColor: open ? t.ok : t.chart4 }} />}
          <T size={12.5} weight="500" color={t.mutedForeground}>{open ? `已连接 ${snap.hostName ?? "电脑"}` : busy ? "正在连接电脑…" : "未连接"}</T>
        </View>
        <T size={13} color={t.mutedForeground} style={{ textAlign: "center", lineHeight: 19 }}>
          {open ? "电脑上没有找到可用的 Codex。可以从侧边栏打开终端，或在文件夹里用其他 agent 开始。" : snap.error ?? "确认电脑上 Tether 正在运行。"}
        </T>
        <Button label={open ? "打开侧边栏" : "重试连接"} onPress={open ? onMenu : () => void link.retry()} style={{ alignSelf: "stretch", borderRadius: 18 }} />
      </View>
    </View>
  );
}

function PairScreen() {
  const t = useTheme();
  const snap = useLink();
  const busy = snap.phase === "pairing";
  return (
    <ScrollView style={{ flex: 1 }} contentContainerStyle={{ flexGrow: 1, justifyContent: "center", paddingHorizontal: 32, paddingVertical: 40, gap: 14 }}>
      <View style={{ alignItems: "center", gap: 16, marginBottom: 8 }}>
        <View style={{ width: 88, height: 88, borderRadius: 24, backgroundColor: t.primary, alignItems: "center", justifyContent: "center" }}>
          <Icon name="terminal" size={40} color={t.primaryForeground} />
        </View>
        <T size={26} weight="600">连接你的电脑</T>
        <T size={14.5} color={t.mutedForeground} style={{ textAlign: "center", lineHeight: 21 }}>
          手机通过加密隧道连到电脑上的 Tether，在手机上和电脑里的 Codex 对话、批准操作、打开终端。
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

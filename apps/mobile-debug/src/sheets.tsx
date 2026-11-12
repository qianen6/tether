// The sheets the drawer opens: a session with any agent in a folder, a terminal, and
// the settings.
import { useEffect, useState } from "react";
import { Alert, ScrollView, TextInput, View } from "react-native";
import type { AgentInfo, ProjectInfo } from "@tether/client";
import { link } from "./link";
import { useLink } from "./useLink";
import { Button, Empty, Icon, Notice, ProgramIcon, programOf, Row, Sheet, T } from "./kit";
import { FONT, MONO, RADIUS, useTheme } from "./theme";

/** The computer's agents, asked while `when` and connected; `problem` is why there are none to show. */
export function useAgentList(when: boolean): { agents: AgentInfo[]; loading: boolean; problem?: string } {
  const snap = useLink();
  const open = snap.phase === "open";
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  useEffect(() => {
    const client = link.client;
    if (!when || !open || !client) return;
    let gone = false;
    setLoading(true);
    setError(undefined);
    client
      .listAgents()
      .then(list => !gone && setAgents(list), e => !gone && setError(e instanceof Error ? e.message : String(e)))
      .finally(() => !gone && setLoading(false));
    return () => {
      gone = true;
    };
  }, [when, open, snap.generation]);
  const problem = !open ? "还没连上电脑，连上后会列出来。" : error ? `读取失败：${error}` : undefined;
  return { agents, loading: loading && !agents.length, problem: agents.length ? undefined : problem };
}

export function useAgents(when: boolean): AgentInfo[] {
  return useAgentList(when).agents;
}

/** Under a sheet's list of agents: still asking, why there are none, or that none fit. */
function AgentsState({ list, shown, none }: { list: ReturnType<typeof useAgentList>; shown: number; none: string }) {
  if (shown) return null;
  if (list.loading) return <Empty busy text="正在读取电脑上的 agent…" />;
  return <Empty text={list.problem ?? none} />;
}

function AgentRow({ a, detail, onPress }: { a: AgentInfo; detail: string; onPress: () => void }) {
  const t = useTheme();
  return (
    <Row onPress={onPress} disabled={!a.available}>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
        <ProgramIcon program={programOf(a.id) === "program" ? programOf(a.terminal_command) : programOf(a.id)} size={18} />
        <View style={{ flex: 1 }}>
          <T size={14.5} weight="500">{a.name}</T>
          <T size={12} color={a.available ? t.sidebarForeground : t.destructive} lines={1}>{a.available ? detail : a.detail ?? "不可用"}</T>
        </View>
        {a.available ? <Icon name="chevron-right" size={16} color={t.sidebarForeground} /> : null}
      </View>
    </Row>
  );
}

/** Two Codexes sit side by side: say which is which. */
const DETAIL: Record<string, string> = {
  codex: "命令行里的 codex（npm 安装，自己更新）",
  "codex-desktop": "Codex 桌面 App 自带的 codex，随 App 更新",
};
const KIND: Record<string, string> = { codex: "Codex app-server", claude: "Claude Code", acp: "ACP", dsh: "DeepSeek Harness（dsh web）", terminal: "终端" };

export function NewSessionSheet({ visible, onClose, onCreated, onChat, cwd: start }: { visible: boolean; onClose: () => void; onCreated: (id: string) => void; onChat?: () => void; cwd?: string }) {
  const t = useTheme();
  const list = useAgentList(visible);
  const agents = list.agents.filter(a => a.chat);
  const [cwd, setCwd] = useState("");
  const [error, setError] = useState<string>();
  useEffect(() => {
    if (visible) setCwd(start ?? "");
  }, [visible, start]);
  const [busy, setBusy] = useState(false);
  const create = async (agent: AgentInfo) => {
    if (busy) return;
    setBusy(true);
    setError(undefined);
    try {
      const session = await link.client!.createSession(agent.id, cwd.trim() ? { cwd: cwd.trim() } : undefined);
      onClose();
      onCreated(session.id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Sheet visible={visible} onClose={onClose} title="新建会话">
      {onChat ? (
        <Row onPress={onChat}>
          <View style={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
            <Icon name="message" size={18} />
            <View style={{ flex: 1 }}>
              <T size={14.5} weight="500">新对话</T>
              <T size={12} color={t.sidebarForeground} lines={1}>只聊天、可选模型，不改电脑上的文件</T>
            </View>
            <Icon name="chevron-right" size={16} color={t.sidebarForeground} />
          </View>
        </Row>
      ) : null}
      <View style={{ paddingHorizontal: 16, paddingTop: onChat ? 8 : 0, paddingBottom: 8, gap: 6 }}>
        <T size={12} color={t.sidebarForeground}>{onChat ? "或在项目里工作 · 工作目录" : "工作目录"}</T>
        <TextInput
          value={cwd}
          onChangeText={setCwd}
          autoCapitalize="none"
          autoCorrect={false}
          placeholder="留空 = 电脑上的用户目录，可写 ~/project"
          placeholderTextColor={t.sidebarForeground}
          style={{ borderRadius: RADIUS, borderWidth: 1, borderColor: t.input, paddingHorizontal: 12, paddingVertical: 10, color: t.foreground, fontFamily: MONO, fontSize: 13.5 }}
        />
        {error ? <Notice text={error} onClose={() => setError(undefined)} /> : null}
      </View>
      <ScrollView keyboardShouldPersistTaps="handled" style={{ opacity: busy ? 0.5 : 1 }}>
        <AgentsState list={list} shown={agents.length} none="电脑上没有找到可以对话的 agent。" />
        {agents.map(a => <AgentRow key={a.id} a={a} detail={DETAIL[a.id] ?? KIND[a.kind] ?? a.kind} onPress={() => void create(a)} />)}
      </ScrollView>
    </Sheet>
  );
}

export function NewTerminalSheet({ visible, onClose, onPick }: { visible: boolean; onClose: () => void; onPick: (agent: string) => void }) {
  const list = useAgentList(visible);
  const agents = list.agents.filter(a => a.terminal_command.length > 0);
  return (
    <Sheet visible={visible} onClose={onClose} title="打开终端">
      <ScrollView>
        <AgentsState list={list} shown={agents.length} none="电脑上没有找到可以在终端里打开的程序。" />
        {agents.map(a => (
          <AgentRow key={a.id} a={a} detail={a.terminal_command.join(" ")} onPress={() => { onClose(); onPick(a.id); }} />
        ))}
      </ScrollView>
    </Sheet>
  );
}

/** The computer's projects, to start a conversation in one. */
export function ProjectSheet({ visible, onClose, projects, onPick, label }: { visible: boolean; onClose: () => void; projects: ProjectInfo[]; onPick: (p: ProjectInfo) => void; label?: string }) {
  const t = useTheme();
  const [query, setQuery] = useState("");
  useEffect(() => {
    if (visible) setQuery("");
  }, [visible]);
  const q = query.trim().toLowerCase();
  const shown = projects.filter(p => !q || [p.name, ...p.roots].some(x => x.toLowerCase().includes(q)));
  return (
    <Sheet visible={visible} onClose={onClose} title="在项目里开始">
      <View style={{ paddingHorizontal: 16, paddingBottom: 8 }}>
        <TextInput
          value={query}
          onChangeText={setQuery}
          autoCapitalize="none"
          autoCorrect={false}
          placeholder="搜索项目"
          placeholderTextColor={t.sidebarForeground}
          style={{ borderRadius: 12, backgroundColor: t.muted, paddingHorizontal: 12, paddingVertical: 9, color: t.foreground, fontFamily: FONT, fontSize: 15 }}
        />
      </View>
      <ScrollView keyboardShouldPersistTaps="handled">
        {shown.map(p => (
          <Row key={p.id} onPress={() => { onClose(); onPick(p); }}>
            <View style={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
              <Icon name="folder" size={18} color={t.foreground} />
              <View style={{ flex: 1 }}>
                <T size={15} weight="500" lines={1}>{p.name}</T>
                <T size={12} mono color={t.sidebarForeground} lines={2}>{p.roots.join("  ·  ")}</T>
              </View>
            </View>
          </Row>
        ))}
        {!shown.length ? <Empty text={q ? "没有找到" : `电脑上的 ${label ?? "Codex"} 里还没有项目。`} /> : null}
      </ScrollView>
    </Sheet>
  );
}

export function SettingsSheet({ visible, onClose }: { visible: boolean; onClose: () => void }) {
  const t = useTheme();
  const snap = useLink();
  const line = (label: string, value: string) => (
    <View style={{ flexDirection: "row", alignItems: "center", minHeight: 44, paddingHorizontal: 16, borderBottomWidth: 1, borderColor: t.border }}>
      <T size={14} style={{ flex: 1 }}>{label}</T>
      <T size={14} color={t.mutedForeground} lines={1} style={{ maxWidth: "60%" }}>{value}</T>
    </View>
  );
  const phase: Record<string, string> = { open: "已连接", connecting: "连接中", reconnecting: "重连中", failed: "未连接", closed: "已断开" };
  return (
    <Sheet visible={visible} onClose={onClose} title="设置">
      {line("电脑", snap.hostName ?? "—")}
      {line("连接", phase[snap.phase] ?? snap.phase)}
      {line("配对耗时", snap.pairMs ? `${(snap.pairMs / 1000).toFixed(1)} 秒` : "—")}
      {line("隧道", "TailPocket（端到端加密）")}
      <View style={{ padding: 16, gap: 4 }}>
        <Button
          label="移除配对"
          kind="danger"
          onPress={() =>
            Alert.alert("移除配对？", "手机会忘记这台电脑，需要重新扫码。电脑上的记录请在电脑端移除。", [
              { text: "取消", style: "cancel" },
              { text: "移除", style: "destructive", onPress: () => { onClose(); void link.forget(); } },
            ])
          }
        />
        <T size={11.5} color={t.sidebarForeground} style={{ textAlign: "center", fontFamily: FONT }}>界面参考 Remodex 与 Herdr Anywhere（均为 Apache-2.0）</T>
      </View>
    </Sheet>
  );
}

// The composer's model menu, as the Codex app shows it: how hard it thinks ("智能") with a
// check on the current one, then the model and the speed, each a page of its own. Picks
// apply at once. The models come from the computer (its relay's model first); any other
// name can be typed in.
import { useEffect, useState } from "react";
import { ActivityIndicator, AppState, Modal, Pressable, ScrollView, TextInput, View } from "react-native";
import type { ModelInfo, PermissionInfo } from "@tether/client";
import { history } from "./files";
import { effortWord, NO_PERMISSIONS, resolvePick, speedDetail, speedWord, type Permissions, type Pick } from "./history";
import { link } from "./link";
import { Icon, T } from "./kit";
import { MONO, useTheme } from "./theme";

/** Agents whose list was asked for since the app started: asking starts a Codex on the computer. */
const asked = new Set<string>();

/**
 * The agent's models: the saved list at once, then the computer's (with `once`, only the first
 * time this run, and again each time `refresh` turns true: the computer may have switched to a
 * relay with other models).
 */
export function useModels(
  agent: string | undefined,
  when: boolean,
  once = false,
  refresh = false,
): { models: ModelInfo[]; permissions: Permissions; loading: boolean; error?: string } {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [permissions, setPermissions] = useState<Permissions>(NO_PERMISSIONS);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  // Back in front: the computer's settings may have changed meanwhile (CC Switch).
  const [woke, setWoke] = useState(0);
  useEffect(() => {
    const sub = AppState.addEventListener("change", s => s === "active" && setWoke(n => n + 1));
    return () => sub.remove();
  }, []);
  useEffect(() => {
    if (!agent || !when) return;
    let gone = false;
    void history.models(agent).then(saved => !gone && saved && setModels(saved));
    void history.permissions(agent).then(saved => !gone && saved && setPermissions(saved));
    const client = link.client;
    if (!client || (once && asked.has(agent) && !refresh && !woke)) return;
    asked.add(agent);
    setLoading(true);
    client
      .listModelChoices(agent)
      .then(data => {
        const fresh = data.models;
        const choices = { list: data.permissions, default: data.default_permission ?? null };
        // Kept even when the menu closed first: the next look shows it.
        void history.saveModels(agent, fresh);
        void history.savePermissions(agent, choices);
        if (gone) return;
        setModels(fresh);
        setPermissions(choices);
        setError(undefined);
      })
      .catch(e => !gone && setError(e instanceof Error ? e.message : String(e)))
      .finally(() => !gone && setLoading(false));
    return () => {
      gone = true;
    };
  }, [agent, when, refresh, woke]);
  return { models, permissions, loading, error };
}

/** What the agent may do without asking: the levels the computer's Codex offers, the current one checked. */
export function PermissionMenu(props: {
  visible: boolean;
  onClose: () => void;
  bottom: number;
  list: PermissionInfo[];
  current?: string;
  onPick: (id: string) => void;
}) {
  const t = useTheme();
  return (
    <Modal visible={props.visible} transparent animationType="fade" onRequestClose={props.onClose} statusBarTranslucent>
      <Pressable style={{ flex: 1, backgroundColor: "rgba(0,0,0,0.06)" }} onPress={props.onClose} />
      <View
        style={{
          position: "absolute", left: 14, bottom: props.bottom, width: 300, maxHeight: 420, borderRadius: 18,
          backgroundColor: t.popover, borderWidth: 1, borderColor: t.border, paddingVertical: 4,
          shadowColor: "#000", shadowOpacity: 0.16, shadowRadius: 24, shadowOffset: { width: 0, height: 8 }, elevation: 14,
        }}
      >
        <ScrollView bounces={false}>
          <T size={13} weight="500" color={t.sidebarForeground} style={{ paddingHorizontal: 16, paddingTop: 10, paddingBottom: 4 }}>权限</T>
          {props.list.map(p => (
            <Pressable
              key={p.id}
              onPress={() => {
                props.onClose();
                if (p.id !== props.current) props.onPick(p.id);
              }}
              style={({ pressed }) => ({ flexDirection: "row", alignItems: "center", gap: 10, minHeight: 48, paddingHorizontal: 16, paddingVertical: 8, backgroundColor: pressed ? t.accent : "transparent" })}
            >
              <View style={{ flex: 1 }}>
                <T size={15.5} lines={1} color={p.id === ":danger-full-access" ? t.destructive : t.foreground}>{p.name}</T>
                {p.description ? <T size={12} color={t.sidebarForeground} lines={2}>{p.description}</T> : null}
              </View>
              {p.id === props.current ? <Icon name="check" size={17} color={t.foreground} /> : null}
            </Pressable>
          ))}
          <T size={11.5} color={t.sidebarForeground} style={{ paddingHorizontal: 16, paddingTop: 4, paddingBottom: 10, lineHeight: 16 }}>
            选项来自电脑上的 Codex；新对话默认用电脑上 Codex 桌面版当前的选择。
          </T>
        </ScrollView>
      </View>
    </Modal>
  );
}

type Page = "main" | "model" | "speed" | "other";

const FALLBACK_EFFORTS = ["low", "medium", "high"];

export function RuntimeMenu(props: {
  visible: boolean;
  onClose: () => void;
  /** Distance from the bottom of the screen to the menu's bottom edge. */
  bottom: number;
  models: ModelInfo[];
  loading?: boolean;
  error?: string;
  current: Pick;
  onPick: (p: Pick) => void;
}) {
  const t = useTheme();
  const [page, setPage] = useState<Page>("main");
  const [typed, setTyped] = useState("");
  useEffect(() => {
    if (props.visible) {
      setPage("main");
      setTyped("");
    }
  }, [props.visible]);

  const { models, current } = props;
  const now = resolvePick(current, models);
  const efforts = now.model?.efforts.length ? now.model.efforts : FALLBACK_EFFORTS;
  const speeds = now.model?.speeds ?? [];
  const pick = (p: Partial<Pick>, close: boolean) => {
    props.onPick({ ...current, ...p });
    if (close) props.onClose();
    else setPage("main");
  };
  const chooseModel = (id: string) => {
    const next = models.find(m => m.id === id);
    // An effort or speed the new model lacks goes back to its default.
    const effort = current.effort && next && !next.efforts.includes(current.effort) ? null : current.effort;
    const speed = current.speed && current.speed !== "default" && next && !next.speeds?.some(s => s.id === current.speed) ? null : current.speed;
    pick({ model: id, effort, speed }, false);
  };

  const row = (o: { key: string; label: string; detail?: string; on?: boolean; chevron?: string; onPress: () => void }) => (
    <Pressable
      key={o.key}
      onPress={o.onPress}
      style={({ pressed }) => ({
        flexDirection: "row", alignItems: "center", gap: 10, minHeight: 40, paddingHorizontal: 16, paddingVertical: 7,
        backgroundColor: pressed ? t.accent : "transparent",
      })}
    >
      <View style={{ flex: 1 }}>
        <T size={15.5} lines={1}>{o.label}</T>
        {o.detail ? <T size={12} color={t.sidebarForeground} lines={2}>{o.detail}</T> : null}
      </View>
      {o.chevron !== undefined ? (
        <>
          <T size={14} color={t.mutedForeground} lines={1} style={{ maxWidth: 150 }}>{o.chevron}</T>
          <Icon name="chevron-right" size={14} color={t.sidebarForeground} />
        </>
      ) : o.on ? (
        <Icon name="check" size={17} color={t.foreground} />
      ) : null}
    </Pressable>
  );
  const back = (label: string) => (
    <Pressable onPress={() => setPage("main")} style={({ pressed }) => ({ flexDirection: "row", alignItems: "center", gap: 4, height: 40, paddingHorizontal: 12, backgroundColor: pressed ? t.accent : "transparent" })}>
      <Icon name="chevron-left" size={16} color={t.mutedForeground} />
      <T size={13} weight="500" color={t.mutedForeground}>{label}</T>
    </Pressable>
  );

  let body;
  if (page === "main") {
    body = (
      <>
        <T size={13} weight="500" color={t.sidebarForeground} style={{ paddingHorizontal: 16, paddingTop: 10, paddingBottom: 4 }}>智能</T>
        {efforts.map(e => row({ key: e, label: effortWord(e), on: e === now.effort, onPress: () => pick({ effort: e }, true) }))}
        <View style={{ height: 1, backgroundColor: t.border, marginVertical: 6, marginHorizontal: 12 }} />
        {row({ key: "model", label: "模型", chevron: now.model?.name ?? now.id ?? "默认", onPress: () => setPage("model") })}
        {row({ key: "speed", label: "速度", chevron: speedWord(now.speed, models), onPress: () => setPage("speed") })}
      </>
    );
  } else if (page === "model") {
    body = (
      <>
        {back("模型")}
        {props.loading && !models.length ? <ActivityIndicator style={{ margin: 12 }} color={t.mutedForeground} /> : null}
        {props.error && !models.length ? <T size={12.5} color={t.destructive} style={{ paddingHorizontal: 16, paddingBottom: 8 }}>{props.error}</T> : null}
        {models.map(m =>
          row({ key: m.id, label: m.name, detail: m.is_default ? "电脑上的默认" : undefined, on: m.id === now.id, onPress: () => chooseModel(m.id) }),
        )}
        {row({ key: "other", label: "其他模型…", detail: "中转提供的模型名", chevron: "", onPress: () => setPage("other") })}
      </>
    );
  } else if (page === "speed") {
    body = (
      <>
        {back("速度")}
        {row({ key: "default", label: "标准", detail: "默认速度", on: now.speed === "default", onPress: () => pick({ speed: "default" }, false) })}
        {speeds.map(s => row({ key: s.id, label: speedWord(s.id, models), detail: s.description ? speedDetail(s.description) : undefined, on: now.speed === s.id, onPress: () => pick({ speed: s.id }, false) }))}
        {!speeds.length ? <T size={12.5} color={t.sidebarForeground} style={{ paddingHorizontal: 16, paddingVertical: 8 }}>这个模型只有标准速度</T> : null}
      </>
    );
  } else {
    body = (
      <>
        {back("其他模型")}
        <View style={{ paddingHorizontal: 12, paddingBottom: 12, gap: 8 }}>
          <TextInput
            value={typed}
            onChangeText={setTyped}
            autoFocus
            autoCapitalize="none"
            autoCorrect={false}
            placeholder="例如 gpt-6.1-sol"
            placeholderTextColor={t.sidebarForeground}
            onSubmitEditing={() => typed.trim() && chooseModel(typed.trim())}
            style={{ borderRadius: 12, borderWidth: 1, borderColor: t.input, paddingHorizontal: 12, paddingVertical: 9, color: t.foreground, fontFamily: MONO, fontSize: 14 }}
          />
          <Pressable disabled={!typed.trim()} onPress={() => chooseModel(typed.trim())}
            style={({ pressed }) => ({ height: 40, borderRadius: 20, alignItems: "center", justifyContent: "center", backgroundColor: t.primary, opacity: !typed.trim() ? 0.4 : pressed ? 0.8 : 1 })}>
            <T size={14.5} weight="500" color={t.primaryForeground}>使用</T>
          </Pressable>
        </View>
      </>
    );
  }

  return (
    <Modal visible={props.visible} transparent animationType="fade" onRequestClose={props.onClose} statusBarTranslucent>
      <Pressable style={{ flex: 1, backgroundColor: "rgba(0,0,0,0.06)" }} onPress={props.onClose} />
      <View
        style={{
          position: "absolute", left: 14, bottom: props.bottom, width: 280, maxHeight: 460, borderRadius: 18,
          backgroundColor: t.popover, borderWidth: 1, borderColor: t.border, paddingVertical: 4,
          shadowColor: "#000", shadowOpacity: 0.16, shadowRadius: 24, shadowOffset: { width: 0, height: 8 }, elevation: 14,
        }}
      >
        <ScrollView keyboardShouldPersistTaps="handled" bounces={false}>{body}</ScrollView>
      </View>
    </Modal>
  );
}

// A computer terminal in a WebView running xterm.js (scripts/terminal-boot.js), and under
// it Herdr's composer and key row (.ref/herdr-anywhere/app/src/ui/pane-bar.tsx): what you
// write goes in the phone's own text box, where its keyboard composes Chinese and
// corrects words as anywhere else, and is pasted into the program with Enter; the row has
// the keys a phone keyboard lacks. The keyboard button types into the terminal itself.
import { useEffect, useMemo, useRef, useState } from "react";
import { Alert, Keyboard, KeyboardAvoidingView, Linking, Platform, Pressable, TextInput, ToastAndroid, View, type TextInputInstance } from "react-native";
import { WebView as RNWebView } from "react-native-webview";
import * as Clipboard from "expo-clipboard";
import { TetherError, type TerminalHandle, type TetherClient } from "@tether/client";
import { link } from "./link";
import { fromBase64, toBase64 } from "./base64";
import { TERMINAL_HTML } from "./terminal/page.generated";
import { Icon, IconButton, ProgramIcon, programOf, T } from "./kit";
import { MONO, RADIUS, useTheme } from "./theme";
import { useLink } from "./useLink";
import { ROW, withMods, type Key } from "./keys";

// react-native-webview 14's props type collapses to `never` against React
// Native 0.88's types; this states the part we use.
interface WebHandle { injectJavaScript(script: string): void }
type WebViewMessageEvent = { nativeEvent: { data: string } };
const WebView = RNWebView as unknown as React.ComponentType<{
  ref: React.Ref<WebHandle>;
  style: object;
  originWhitelist: string[];
  source: { html: string };
  onMessage: (e: WebViewMessageEvent) => void;
  keyboardDisplayRequiresUserAction: boolean;
  hideKeyboardAccessoryView: boolean;
  overScrollMode: string;
}>;

const ANSI = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
const NAMES = [...ANSI, ...ANSI.map(n => `bright${n[0]!.toUpperCase()}${n.slice(1)}`)];
/** The composer grows with its text up to this, then scrolls. */
const COMPOSER_MIN = 38;
const COMPOSER_MAX = 132;

type PageMessage =
  | { t: "ready" | "size"; cols: number; rows: number }
  | { t: "input"; data?: string; b64?: string }
  | { t: "selected"; text: string }
  | { t: "link"; url: string }
  | { t: "tap" }
  | { t: "away"; away: boolean };

const toast = (text: string) => {
  if (Platform.OS === "android") ToastAndroid.show(text, ToastAndroid.SHORT);
};

export function TerminalScreen({ term, agent, onBack }: { term?: string; agent?: string; onBack: () => void }) {
  const t = useTheme();
  const snap = useLink();
  const web = useRef<WebHandle>(null);
  const input = useRef<TextInputInstance>(null);
  const handle = useRef<TerminalHandle | undefined>(undefined);
  /** The client `handle` came from: a connection started over has a new one. */
  const owner = useRef<TetherClient | undefined>(undefined);
  /** The page's size, once it is ready. */
  const size = useRef<{ cols: number; rows: number } | undefined>(undefined);
  /** What the page shows is about to be sent again (the computer replays a terminal on attach). */
  const stale = useRef(false);
  const starting = useRef(false);
  const [command, setCommand] = useState<string[]>(agent ? [agent] : []);
  const [exited, setExited] = useState<number | null | undefined>(undefined);
  const [mods, setMods] = useState({ ctrl: false, shift: false });
  const modsRef = useRef(mods);
  modsRef.current = mods;
  const [text, setText] = useState("");
  const [height, setHeight] = useState(COMPOSER_MIN);
  const [away, setAway] = useState(false);
  const [keyboard, setKeyboard] = useState(false);
  // Typing straight into the terminal (the keyboard button), not into the composer.
  const direct = useRef(false);

  useEffect(() => () => void handle.current?.detach().catch(() => {}), []);
  useEffect(() => {
    const show = Keyboard.addListener("keyboardDidShow", () => setKeyboard(true));
    const hide = Keyboard.addListener("keyboardDidHide", () => {
      setKeyboard(false);
      direct.current = false;
    });
    return () => {
      show.remove();
      hide.remove();
    };
  }, []);

  const html = useMemo(() => {
    const theme: Record<string, string> = {
      background: t.terminal.background,
      foreground: t.terminal.foreground,
      cursor: t.terminal.cursor,
      cursorAccent: t.terminal.background,
      selectionBackground: t.terminal.selection,
      overviewRulerBorder: t.terminal.background,
    };
    t.terminal.ansi.forEach((c, i) => (theme[NAMES[i]!] = c));
    return TERMINAL_HTML.replace("__THEME__", JSON.stringify(theme));
  }, [t]);

  const page = (call: string) => web.current?.injectJavaScript(`window.tether&&window.tether.${call};true;`);

  const write = (bytes: Uint8Array) => {
    if (stale.current) {
      stale.current = false;
      page("reset()");
    }
    // Chunks keep each injected script small.
    for (let i = 0; i < bytes.length; i += 32 << 10) page(`write("${toBase64(bytes.subarray(i, i + (32 << 10)))}")`);
  };

  /**
   * Opens the terminal, or takes it up again: when the page is ready, when the computer
   * comes into reach, and when the connection was started over. `reloaded`: the page is
   * new (the theme changed) and has nothing on it.
   */
  const start = async (reloaded = false) => {
    const client = link.client;
    const at = size.current;
    if (!client || !at || starting.current || link.snapshot.phase !== "open") return;
    const old = handle.current;
    const same = !!old && owner.current === client;
    // The client attaches its own terminals again after a reconnect.
    if (same && !reloaded) return;
    starting.current = true;
    try {
      const id = old?.info.id ?? term;
      if (old) stale.current = true;
      const h = id ? await client.attachTerminal(id) : await client.openTerminal({ agent, cols: at.cols, rows: at.rows });
      if (!same) {
        handle.current = h;
        owner.current = client;
        setCommand(h.info.command);
        setExited(undefined);
        h.onData(write);
        h.onExit(code => setExited(code));
      }
      if (id) await h.resize(at.cols, at.rows).catch(() => {});
    } catch (e) {
      // It ended while the phone was away.
      if (old && e instanceof TetherError && e.code === "not_found") setExited(null);
      else if (!old) Alert.alert("无法打开终端", e instanceof Error ? e.message : String(e));
    } finally {
      starting.current = false;
      // The connection was started over meanwhile: take the terminal up on the new one.
      if (link.client && link.client !== client) void start();
    }
  };

  const open = snap.phase === "open";
  useEffect(() => {
    if (open) void start();
    else if (handle.current) stale.current = true;
  }, [open, snap.generation]);

  const send = (data: string | Uint8Array) => {
    try {
      handle.current?.write(data);
    } catch {
      /* reconnecting: the client re-attaches by itself */
    }
  };

  const onMessage = (e: WebViewMessageEvent) => {
    let m: PageMessage;
    try {
      m = JSON.parse(e.nativeEvent.data);
    } catch {
      return;
    }
    if (m.t === "ready") {
      const reloaded = !!size.current;
      size.current = { cols: m.cols, rows: m.rows };
      void start(reloaded);
    } else if (m.t === "size") {
      size.current = { cols: m.cols, rows: m.rows };
      void handle.current?.resize(m.cols, m.rows).catch(() => {});
    }
    else if (m.t === "input") {
      if (m.b64) return send(fromBase64(m.b64));
      if (m.data === undefined) return;
      const { ctrl, shift } = modsRef.current;
      if (ctrl || shift) setMods({ ctrl: false, shift: false });
      send(withMods(m.data, { ctrl, shift }));
    } else if (m.t === "selected") {
      void Clipboard.setStringAsync(m.text).then(() => toast("已复制"));
    } else if (m.t === "link") {
      void Linking.openURL(m.url).catch(() => {});
    } else if (m.t === "tap") {
      // A tap on the screen is for writing to it: the composer, unless typing straight in.
      if (!direct.current) input.current?.focus();
    } else if (m.t === "away") setAway(m.away);
  };

  const press = (key: Key) => {
    if (key === "ctrl" || key === "shift") return setMods(m => ({ ...m, [key]: !m[key] }));
    page(`key(${JSON.stringify(key)},${mods.ctrl},${mods.shift})`);
    setMods({ ctrl: false, shift: false });
    // Typing in the terminal goes on there after a key; typing in the composer stays in it.
    if (direct.current) page("focus()");
  };

  // With nothing written, the button is Enter: "Press enter to continue", a menu's choice.
  const submit = () => {
    if (!text.trim()) return press("enter");
    page(`submit("${toBase64(new TextEncoder().encode(text))}")`);
    setText("");
    setHeight(COMPOSER_MIN);
  };

  const typeDirect = () => {
    direct.current = true;
    page("focus()");
  };

  const kill = () =>
    Alert.alert("结束这个终端？", "电脑上的程序会被终止。", [
      { text: "取消", style: "cancel" },
      { text: "结束", style: "destructive", onPress: () => void handle.current?.kill().then(onBack, onBack) },
    ]);

  const offline = !open;
  const program = programOf(command);
  // Every key fits on one row, as Herdr's: none is off the edge.
  const keyStyle = (on: boolean, grow = true) => ({ pressed }: { pressed: boolean }) => ({
    ...(grow ? { flex: 1 } : { width: 34 }), height: 36, alignItems: "center" as const, justifyContent: "center" as const, borderRadius: RADIUS - 2,
    backgroundColor: on ? t.primary : pressed ? t.accent : t.secondary,
  });
  return (
    // Edge-to-edge Android does not resize the window for the keyboard: the composer rides on it.
    // The page keeps its rows when it gets shorter (terminal-boot.js), so the program does not redraw.
    <KeyboardAvoidingView style={{ flex: 1, backgroundColor: t.sidebar }} behavior="padding">
      <View style={{ flexDirection: "row", alignItems: "center", height: 48, paddingHorizontal: 4, gap: 4 }}>
        <IconButton name="chevron-left" label="返回" onPress={onBack} size={20} />
        <ProgramIcon program={program} size={16} />
        <View style={{ flex: 1, marginLeft: 4 }}>
          <T size={14.5} weight="600" lines={1}>{command.join(" ") || "终端"}</T>
          {exited !== undefined || offline ? (
            <T size={11.5} color={exited !== undefined ? t.sidebarForeground : t.chart2} lines={1}>
              {exited !== undefined ? `已退出${exited !== null ? ` · 退出码 ${exited}` : ""}` : "重连中…"}
            </T>
          ) : null}
        </View>
        <IconButton name="square" label="结束终端" onPress={kill} size={16} />
      </View>
      <View style={{ flex: 1, backgroundColor: t.terminal.background, borderTopWidth: 1, borderColor: t.border }}>
        <WebView
          ref={web}
          style={{ flex: 1, backgroundColor: t.terminal.background }}
          originWhitelist={["*"]}
          source={{ html }}
          onMessage={onMessage}
          keyboardDisplayRequiresUserAction={false}
          hideKeyboardAccessoryView
          overScrollMode="never"
        />
        {away ? (
          <Pressable
            accessibilityLabel="回到最新"
            onPress={() => page("bottom()")}
            style={({ pressed }) => ({
              position: "absolute", right: 12, bottom: 12, height: 32, paddingHorizontal: 12, flexDirection: "row", alignItems: "center", gap: 4,
              borderRadius: 16, backgroundColor: pressed ? t.accent : t.secondary, borderWidth: 1, borderColor: t.border,
            })}
          >
            <Icon name="chevron-down" size={14} color={t.foreground} />
            <T size={12.5} weight="500">最新</T>
          </Pressable>
        ) : null}
      </View>
      <View style={{ borderTopWidth: 1, borderColor: t.border, backgroundColor: t.sidebar, paddingBottom: 4 }}>
        <View style={{ flexDirection: "row", alignItems: "flex-end", gap: 6, paddingHorizontal: 8, paddingTop: 6 }}>
          <TextInput
            ref={input}
            style={{
              flex: 1, height: text ? Math.min(COMPOSER_MAX, Math.max(COMPOSER_MIN, height)) : COMPOSER_MIN,
              borderRadius: RADIUS - 2, borderWidth: 1, borderColor: t.input, backgroundColor: t.background,
              paddingHorizontal: 12, paddingVertical: 8, color: t.foreground, fontFamily: MONO, fontSize: 14, textAlignVertical: "top",
            }}
            multiline
            value={text}
            onChangeText={setText}
            onFocus={() => (direct.current = false)}
            onContentSizeChange={e => setHeight(e.nativeEvent.contentSize.height + 16)}
            placeholder={["shell", "program", "agent"].includes(program) ? "输入命令…" : `告诉 ${program[0]!.toUpperCase()}${program.slice(1)} 要做什么…`}
            placeholderTextColor={t.sidebarForeground}
          />
          <Pressable
            accessibilityLabel={text.trim() ? "发送" : "回车"}
            onPress={submit}
            style={({ pressed }) => ({
              width: 38, height: 38, borderRadius: 19, alignItems: "center", justifyContent: "center",
              backgroundColor: text.trim() ? t.primary : pressed ? t.accent : t.muted,
            })}
          >
            {text.trim() ? <Icon name="arrow-up" size={18} color={t.primaryForeground} /> : <T size={17} mono color={t.foreground}>↵</T>}
          </Pressable>
        </View>
        <View style={{ flexDirection: "row", gap: 3, paddingHorizontal: 6, paddingTop: 6 }}>
          {ROW.map(k => {
            const on = (k.key === "ctrl" && mods.ctrl) || (k.key === "shift" && mods.shift);
            return (
              <Pressable key={k.key} accessibilityLabel={k.key} onPress={() => press(k.key)} style={keyStyle(on)}>
                <T size={k.label.length > 2 ? 12 : 14} mono weight="500" color={on ? t.primaryForeground : t.foreground}>{k.label}</T>
              </Pressable>
            );
          })}
          <Pressable
            accessibilityLabel={keyboard ? "收起键盘" : "直接在终端里打字"}
            onPress={() => (keyboard ? Keyboard.dismiss() : typeDirect())}
            style={keyStyle(false, false)}
          >
            <Icon name={keyboard ? "keyboard-down" : "keyboard"} size={16} color={t.foreground} />
          </Pressable>
        </View>
      </View>
    </KeyboardAvoidingView>
  );
}

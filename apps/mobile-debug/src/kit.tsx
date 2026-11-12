// What every screen shares, after Herdr Anywhere's kit (.ref/herdr-anywhere/app/src/ui/kit.tsx):
// text, icons, status marks, buttons, section labels and bottom sheets.
import { useEffect, useRef, type ReactNode } from "react";
import {
  Animated,
  Easing,
  Modal,
  Pressable,
  Text,
  View,
  type StyleProp,
  type TextStyle,
  type ViewStyle,
} from "react-native";
import { SvgXml } from "react-native-svg";
import type { SessionState } from "@tether/client";
import { PROGRAM_ICONS, UI_ICONS } from "./icons.generated";
import { alpha, FONT, MONO, RADIUS, useTheme } from "./theme";

export type IconName = keyof typeof UI_ICONS;

export function T(props: {
  children?: ReactNode;
  size?: number;
  weight?: "400" | "500" | "600" | "700";
  color?: string;
  mono?: boolean;
  lines?: number;
  selectable?: boolean;
  style?: StyleProp<TextStyle>;
  onPress?: () => void;
}) {
  const t = useTheme();
  const size = props.size ?? 14;
  return (
    <Text
      numberOfLines={props.lines}
      selectable={props.selectable}
      onPress={props.onPress}
      style={[
        {
          fontFamily: props.mono ? MONO : FONT,
          fontSize: size,
          lineHeight: Math.round(size * (props.mono ? 1.45 : 1.4)),
          fontWeight: props.weight ?? "400",
          color: props.color ?? t.foreground,
        },
        props.style,
      ]}
    >
      {props.children}
    </Text>
  );
}

export function Icon(props: { name: IconName; size?: number; color?: string }) {
  const t = useTheme();
  return <SvgXml xml={UI_ICONS[props.name]} width={props.size ?? 18} height={props.size ?? 18} color={props.color ?? t.mutedForeground} />;
}

/** Which Herdr program icon a Tether agent id or command line gets. */
export function programOf(agentOrCommand: string | string[] | undefined): string {
  const first = Array.isArray(agentOrCommand) ? agentOrCommand[0] ?? "" : agentOrCommand ?? "";
  const name = first.split(/[\\/]/).pop()!.toLowerCase().replace(/\.(exe|cmd|bat|ps1)$/, "");
  if (name in PROGRAM_ICONS) return name;
  if (name.startsWith("codex")) return "codex";
  if (name === "cursor-agent") return "cursor";
  if (["powershell", "pwsh", "cmd", "bash", "zsh", "sh", "fish"].includes(name)) return "shell";
  if (name === "echo") return "agent";
  return "program";
}

export function ProgramIcon(props: { program: string; size?: number }) {
  const t = useTheme();
  const xml = PROGRAM_ICONS[props.program] ?? PROGRAM_ICONS.program!;
  return <SvgXml xml={xml} width={props.size ?? 16} height={props.size ?? 16} color={t.mutedForeground} />;
}

export function Spinner(props: { size?: number; color?: string }) {
  const turn = useRef(new Animated.Value(0)).current;
  useEffect(() => {
    const loop = Animated.loop(Animated.timing(turn, { toValue: 1, duration: 900, easing: Easing.linear, useNativeDriver: true }));
    loop.start();
    return () => loop.stop();
  }, [turn]);
  const rotate = turn.interpolate({ inputRange: [0, 1], outputRange: ["0deg", "360deg"] });
  return (
    <Animated.View style={{ transform: [{ rotate }] }}>
      <Icon name="spinner" size={props.size ?? 14} color={props.color} />
    </Animated.View>
  );
}

/** Herdr's status: needs you (red badge), working (spinner), done (blue dot), idle (grey ring). */
export function StatusMark(props: { state: SessionState | "done"; compact?: boolean; size?: number }) {
  const t = useTheme();
  const size = props.size ?? 8;
  switch (props.state) {
    case "working":
      return <Spinner size={size + 6} color={t.chart1} />;
    case "needs_you":
      return (
        <View style={{ paddingHorizontal: props.compact ? 0 : 6, width: props.compact ? 15 : undefined, height: props.compact ? 15 : 18, alignItems: "center", justifyContent: "center", borderRadius: 5, backgroundColor: alpha(t.chart2, 0.15) }}>
          <T size={10.5} weight={props.compact ? "700" : "600"} color={t.chart2}>{props.compact ? "!" : "需要你"}</T>
        </View>
      );
    case "done":
      return <View style={{ width: size, height: size, borderRadius: size, backgroundColor: t.chart3 }} />;
    case "error":
      return <View style={{ width: size, height: size, borderRadius: size, backgroundColor: t.destructive }} />;
    default:
      return <View style={{ width: size, height: size, borderRadius: size, borderWidth: 1, borderColor: t.chart4 }} />;
  }
}

export const STATE_WORD: Record<SessionState | "done", string> = {
  idle: "空闲",
  working: "工作中",
  needs_you: "需要你",
  error: "出错",
  exited: "已结束",
  done: "完成",
};

export function stateColor(t: ReturnType<typeof useTheme>, s: SessionState | "done"): string {
  return { working: t.chart1, needs_you: t.chart2, done: t.chart3, error: t.destructive, idle: t.chart4, exited: t.sidebarForeground }[s];
}

export function Button(props: { label: string; onPress: () => void; kind?: "primary" | "plain" | "outline" | "danger"; disabled?: boolean; style?: StyleProp<ViewStyle> }) {
  const t = useTheme();
  const kind = props.kind ?? "primary";
  return (
    <Pressable
      onPress={props.onPress}
      disabled={props.disabled}
      style={({ pressed }) => [
        { borderRadius: RADIUS, paddingHorizontal: 16, paddingVertical: 12, alignItems: "center" },
        kind === "primary" && { backgroundColor: t.primary, opacity: pressed ? 0.8 : 1 },
        kind === "outline" && { borderWidth: 1, borderColor: t.input, backgroundColor: pressed ? t.accent : "transparent" },
        (kind === "plain" || kind === "danger") && { backgroundColor: pressed ? t.accent : "transparent" },
        props.disabled && { opacity: 0.5 },
        props.style,
      ]}
    >
      <T size={15} weight="500" color={kind === "primary" ? t.primaryForeground : kind === "danger" ? t.destructive : t.foreground}>{props.label}</T>
    </Pressable>
  );
}

export function IconButton(props: { name: IconName; label: string; onPress: () => void; active?: boolean; disabled?: boolean; primary?: boolean; size?: number }) {
  const t = useTheme();
  return (
    <Pressable
      accessibilityLabel={props.label}
      onPress={props.onPress}
      disabled={props.disabled}
      style={({ pressed }) => ({
        width: 40,
        height: props.primary ? 36 : 40,
        alignItems: "center",
        justifyContent: "center",
        borderRadius: RADIUS,
        backgroundColor: props.primary ? t.primary : pressed || props.active ? t.accent : "transparent",
        opacity: props.disabled ? 0.4 : props.primary && pressed ? 0.8 : 1,
      })}
    >
      <Icon name={props.name} size={props.size ?? 18} color={props.primary ? t.primaryForeground : props.active ? t.foreground : t.mutedForeground} />
    </Pressable>
  );
}

/** A pressable list row on the home canvas. */
export function Row(props: { children: ReactNode; onPress?: () => void; onLongPress?: () => void; style?: StyleProp<ViewStyle>; disabled?: boolean }) {
  const t = useTheme();
  return (
    <Pressable
      onPress={props.onPress}
      onLongPress={props.onLongPress}
      disabled={props.disabled}
      style={({ pressed }) => [
        { marginHorizontal: 8, paddingHorizontal: 8, minHeight: 44, paddingVertical: 8, borderRadius: RADIUS, justifyContent: "center", backgroundColor: pressed ? t.sidebarAccent : "transparent" },
        props.disabled && { opacity: 0.45 },
        props.style,
      ]}
    >
      {props.children}
    </Pressable>
  );
}

/** A sheet from the bottom over a dimmed screen. */
export function Sheet(props: { visible: boolean; onClose: () => void; title: string; children: ReactNode }) {
  const t = useTheme();
  return (
    <Modal visible={props.visible} transparent animationType="slide" onRequestClose={props.onClose} statusBarTranslucent>
      <Pressable style={{ flex: 1, backgroundColor: "rgba(0,0,0,0.35)" }} onPress={props.onClose} />
      <View style={{ backgroundColor: t.background, borderTopLeftRadius: 22, borderTopRightRadius: 22, maxHeight: "80%", paddingBottom: 24 }}>
        <View style={{ alignItems: "center", paddingTop: 8 }}>
          <View style={{ width: 36, height: 4, borderRadius: 2, backgroundColor: t.border }} />
        </View>
        <View style={{ flexDirection: "row", alignItems: "center", paddingLeft: 16, paddingRight: 4, height: 48 }}>
          <T size={16} weight="600" style={{ flex: 1 }}>{props.title}</T>
          <IconButton name="x" label="关闭" onPress={props.onClose} />
        </View>
        {props.children}
      </View>
    </Modal>
  );
}

/** "3 分钟" for a span in milliseconds. */
export function duration(ms: number): string {
  const s = Math.floor(ms / 1000);
  if (s < 60) return "刚刚";
  if (s < 3600) return `${Math.floor(s / 60)} 分钟`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时`;
  return `${Math.floor(s / 86400)} 天`;
}

/** The last two parts of a path, for rows. */
export function shortPath(path: string): string {
  const parts = path.replace(/^\\\\\?\\/, "").split(/[\\/]+/).filter(Boolean);
  return parts.length <= 2 ? path : `…/${parts.slice(-2).join("/")}`;
}

/** Remodex's round toolbar button: an icon on a soft grey disc (its glass, flattened). */
export function CircleButton(props: { name: IconName; label: string; onPress: () => void; disabled?: boolean; size?: number; filled?: boolean }) {
  const t = useTheme();
  const side = props.size ?? 40;
  return (
    <Pressable
      accessibilityLabel={props.label}
      onPress={props.onPress}
      disabled={props.disabled}
      hitSlop={4}
      style={({ pressed }) => ({
        width: side,
        height: side,
        borderRadius: side / 2,
        alignItems: "center",
        justifyContent: "center",
        backgroundColor: props.filled ? t.primary : pressed ? t.accent : t.muted,
        opacity: props.disabled ? 0.35 : pressed && props.filled ? 0.8 : 1,
      })}
    >
      <Icon name={props.name} size={Math.round(side * 0.45)} color={props.filled ? t.primaryForeground : t.foreground} />
    </Pressable>
  );
}

export interface MenuItem {
  label: string;
  icon: IconName;
  onPress: () => void;
  danger?: boolean;
  disabled?: boolean;
}

/** A pop-up menu under a toolbar button, as iOS shows one. */
export function Menu(props: { visible: boolean; onClose: () => void; items: MenuItem[]; top: number; side: "left" | "right" }) {
  const t = useTheme();
  return (
    <Modal visible={props.visible} transparent animationType="fade" onRequestClose={props.onClose} statusBarTranslucent>
      <Pressable style={{ flex: 1, backgroundColor: "rgba(0,0,0,0.08)" }} onPress={props.onClose}>
        <View
          style={{
            position: "absolute",
            top: props.top,
            [props.side]: 12,
            minWidth: 220,
            borderRadius: 14,
            backgroundColor: t.popover,
            paddingVertical: 6,
            borderWidth: 1,
            borderColor: t.border,
            shadowColor: "#000",
            shadowOpacity: 0.15,
            shadowRadius: 20,
            shadowOffset: { width: 0, height: 8 },
            elevation: 12,
          }}
        >
          {props.items.map(item => (
            <Pressable
              key={item.label}
              disabled={item.disabled}
              onPress={() => {
                props.onClose();
                item.onPress();
              }}
              style={({ pressed }) => ({
                flexDirection: "row",
                alignItems: "center",
                gap: 12,
                height: 46,
                paddingHorizontal: 16,
                backgroundColor: pressed ? t.accent : "transparent",
                opacity: item.disabled ? 0.4 : 1,
              })}
            >
              <T size={15.5} color={item.danger ? t.destructive : t.foreground} style={{ flex: 1 }}>{item.label}</T>
              <Icon name={item.icon} size={18} color={item.danger ? t.destructive : t.foreground} />
            </Pressable>
          ))}
        </View>
      </Pressable>
    </Modal>
  );
}

/** One line that something is wrong or on hold, tinted: an error, or a warning such as "重连中…". */
export function Notice(props: {
  text: string;
  tone?: "error" | "warn";
  busy?: boolean;
  lines?: number;
  action?: { label: string; onPress: () => void };
  onClose?: () => void;
  style?: StyleProp<ViewStyle>;
}) {
  const t = useTheme();
  const color = props.tone === "warn" ? t.chart2 : t.destructive;
  return (
    <View
      accessibilityRole="alert"
      style={[
        { flexDirection: "row", alignItems: "center", gap: 8, minHeight: 36, borderRadius: 12, backgroundColor: alpha(color, 0.08), paddingLeft: 12, paddingRight: props.onClose ? 4 : 12, paddingVertical: 6 },
        props.style,
      ]}
    >
      {props.busy ? <Spinner size={13} color={color} /> : null}
      <T size={13} color={color} lines={props.lines ?? 4} selectable style={{ flex: 1, lineHeight: 19 }}>{props.text}</T>
      {props.action ? (
        <Pressable onPress={props.action.onPress} hitSlop={8} style={({ pressed }) => ({ paddingHorizontal: 6, opacity: pressed ? 0.6 : 1 })}>
          <T size={13} weight="600">{props.action.label}</T>
        </Pressable>
      ) : null}
      {props.onClose ? (
        <Pressable accessibilityLabel="关闭提示" onPress={props.onClose} hitSlop={8}
          style={({ pressed }) => ({ width: 28, height: 28, borderRadius: 14, alignItems: "center", justifyContent: "center", backgroundColor: pressed ? alpha(color, 0.12) : "transparent" })}>
          <Icon name="x" size={13} color={color} />
        </Pressable>
      ) : null}
    </View>
  );
}

/** What a list says when it has nothing: still loading, or why it is empty. */
export function Empty(props: { text: string; busy?: boolean; style?: StyleProp<ViewStyle> }) {
  const t = useTheme();
  return (
    <View style={[{ alignItems: "center", justifyContent: "center", gap: 10, paddingHorizontal: 24, paddingVertical: 20 }, props.style]}>
      {props.busy ? <Spinner size={16} color={t.sidebarForeground} /> : null}
      <T size={13} color={t.sidebarForeground} style={{ textAlign: "center", lineHeight: 19 }}>{props.text}</T>
    </View>
  );
}

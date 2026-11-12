// Draws markdown.ts's blocks with React Native's Text, in the chat's look (Remodex:
// plain text on white, grey fills for code and tables). A link to a web page opens it;
// one to a file on the computer says where the file is.
import { useMemo, type ReactNode } from "react";
import { Alert, Linking, ScrollView, Text, View, type TextStyle } from "react-native";
import * as Clipboard from "expo-clipboard";
import { linkTarget, parse, runsText, type Block, type Run } from "./markdown";
import { MONO, useTheme } from "./theme";

const openLink = (href: string) => {
  const target = linkTarget(href);
  if (/^(https?|mailto):/i.test(target)) return void Linking.openURL(target).catch(() => {});
  Alert.alert("电脑上的文件", target.replace(/^file:\/+/i, ""), [
    { text: "复制路径", onPress: () => void Clipboard.setStringAsync(target.replace(/^file:\/+/i, "")) },
    { text: "好" },
  ]);
};

/** A column's width in a table: its longest cell, a CJK character twice a Latin one. */
function columnWidth(cells: Run[][], size: number): number {
  const width = (s: string) => [...s].reduce((w, c) => w + (/[⺀-鿿가-힯＀-￯]/.test(c) ? 1 : 0.6), 0);
  const longest = Math.max(...cells.map(c => width(runsText(c))), 2);
  return Math.round(Math.min(220, Math.max(56, longest * size + 20)));
}

export function Markdown(props: { text: string; size?: number; color?: string; cursor?: boolean }) {
  const t = useTheme();
  const size = props.size ?? 14.5;
  const color = props.color ?? t.foreground;
  const base: TextStyle = { fontFamily: MONO, fontSize: size, lineHeight: Math.round(size * 1.58), color };
  const blocks = useMemo(() => parse(props.text), [props.text]);

  const runs = (rs: Run[], key: string): ReactNode[] =>
    rs.map((r, i) => {
      if (r.t === "br") return "\n";
      const style: TextStyle = {
        ...(r.strong ? { fontWeight: "700" } : null),
        ...(r.em ? { fontStyle: "italic" } : null),
        ...(r.strike ? { textDecorationLine: "line-through" } : null),
        ...(r.code ? { fontSize: size - 1.5, backgroundColor: t.muted } : null),
        ...(r.href ? { color: t.chart3, textDecorationLine: "underline" } : null),
      };
      return (
        <Text key={`${key}.${i}`} style={style} onPress={r.href ? () => openLink(r.href!) : undefined}>
          {r.code ? ` ${r.text} ` : r.text}
        </Text>
      );
    });

  const block = (b: Block, key: string, last: boolean): ReactNode => {
    switch (b.t) {
      case "p":
        return (
          <Text key={key} selectable style={base}>
            {runs(b.runs, key)}
            {last && props.cursor ? " ▍" : null}
          </Text>
        );
      case "h": {
        const s = b.level <= 1 ? size + 4 : b.level === 2 ? size + 2.5 : size + 1;
        return (
          <Text key={key} selectable style={[base, { fontSize: s, lineHeight: Math.round(s * 1.45), fontWeight: "700", marginTop: 4 }]}>
            {runs(b.runs, key)}
          </Text>
        );
      }
      case "code":
        return (
          <View key={key} style={{ borderRadius: 12, backgroundColor: t.muted }}>
            {b.lang ? <Text style={{ fontFamily: MONO, fontSize: 11, color: t.sidebarForeground, paddingHorizontal: 12, paddingTop: 8 }}>{b.lang}</Text> : null}
            <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={{ padding: 12, paddingTop: b.lang ? 4 : 12 }}>
              <Text selectable style={{ fontFamily: MONO, fontSize: size - 2, lineHeight: Math.round((size - 2) * 1.5), color }}>{b.text.replace(/\n$/, "")}</Text>
            </ScrollView>
          </View>
        );
      case "quote":
        return (
          <View key={key} style={{ borderLeftWidth: 3, borderColor: t.border, paddingLeft: 12, gap: 8 }}>
            {b.blocks.map((x, i) => block(x, `${key}.${i}`, false))}
          </View>
        );
      case "list":
        return (
          <View key={key} style={{ gap: 6 }}>
            {b.items.map((item, i) => (
              <View key={`${key}.${i}`} style={{ flexDirection: "row" }}>
                <Text style={[base, { width: b.ordered ? 26 : 18, color: b.ordered ? color : t.mutedForeground }]}>{b.ordered ? `${b.start + i}.` : "•"}</Text>
                <View style={{ flex: 1, gap: 6 }}>{item.map((x, j) => block(x, `${key}.${i}.${j}`, last && i === b.items.length - 1 && j === item.length - 1))}</View>
              </View>
            ))}
          </View>
        );
      case "table": {
        const columns = b.head.map((h, c) => columnWidth([h, ...b.rows.map(r => r[c] ?? [])], size));
        const row = (cells: Run[][], k: string, head: boolean) => (
          <View key={k} style={{ flexDirection: "row", borderTopWidth: head ? 0 : 1, borderColor: t.border, backgroundColor: head ? t.muted : undefined }}>
            {columns.map((w, c) => (
              <View key={c} style={{ width: w, paddingHorizontal: 10, paddingVertical: 7 }}>
                <Text selectable style={[base, { fontSize: size - 1.5, lineHeight: Math.round((size - 1.5) * 1.5), textAlign: b.align[c] ?? "left" }, head ? { fontWeight: "700" } : null]}>
                  {runs(cells[c] ?? [], `${k}.${c}`)}
                </Text>
              </View>
            ))}
          </View>
        );
        return (
          <ScrollView key={key} horizontal showsHorizontalScrollIndicator={false}>
            <View style={{ borderWidth: 1, borderColor: t.border, borderRadius: 10, overflow: "hidden" }}>
              {row(b.head, `${key}.h`, true)}
              {b.rows.map((r, i) => row(r, `${key}.${i}`, false))}
            </View>
          </ScrollView>
        );
      }
      case "hr":
        return <View key={key} style={{ height: 1, backgroundColor: t.border, marginVertical: 4 }} />;
    }
  };

  if (!blocks.length) return props.cursor ? <Text style={base}>▍</Text> : null;
  return <View style={{ gap: 10 }}>{blocks.map((b, i) => block(b, String(i), i === blocks.length - 1))}</View>;
}

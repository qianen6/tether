// Pictures in a conversation: those sent with a message live on the computer
// and come over by `get_file`; a tap shows one full screen.
import { useEffect, useState } from "react";
import { Image, Modal, Pressable, StatusBar, View } from "react-native";
import { link } from "./link";
import { Icon, Spinner, T } from "./kit";
import { useTheme } from "./theme";

/** Pictures already here, by path on the computer: `data:` URIs. */
const loaded = new Map<string, Promise<string>>();

/** Keeps a picture just sent, so it shows without asking the computer. */
export function remember(path: string, mime: string, data: string) {
  loaded.set(path, Promise.resolve(`data:${mime};base64,${data}`));
}

function load(path: string): Promise<string> {
  let p = loaded.get(path);
  if (!p) {
    const client = link.client;
    if (!client) return Promise.reject(new Error("未连接电脑"));
    p = client.getFile(path).then(f => `data:${f.mime};base64,${f.data}`);
    // A failure is asked again next time.
    p.catch(() => loaded.delete(path));
    loaded.set(path, p);
  }
  return p;
}

function usePicture(path: string): { uri?: string; error?: string; retry: () => void } {
  const [uri, setUri] = useState<string>();
  const [error, setError] = useState<string>();
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let gone = false;
    setError(undefined);
    load(path).then(
      u => !gone && setUri(u),
      e => !gone && setError(e instanceof Error ? e.message : String(e)),
    );
    return () => {
      gone = true;
    };
  }, [path, attempt]);
  return { uri, error, retry: () => setAttempt(a => a + 1) };
}

const SIZE = 132;

/** A message's pictures, small; each opens full screen. */
export function Pictures({ paths, align = "flex-end" }: { paths: string[]; align?: "flex-start" | "flex-end" }) {
  const [shown, setShown] = useState<string>();
  return (
    <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6, justifyContent: align }}>
      {paths.map(p => <Thumb key={p} path={p} onOpen={setShown} />)}
      <Viewer uri={shown} onClose={() => setShown(undefined)} />
    </View>
  );
}

function Thumb({ path, onOpen }: { path: string; onOpen: (uri: string) => void }) {
  const t = useTheme();
  const { uri, error, retry } = usePicture(path);
  return (
    <Pressable
      accessibilityLabel="图片"
      onPress={() => (uri ? onOpen(uri) : error ? retry() : undefined)}
      style={{ width: SIZE, height: SIZE, borderRadius: 14, overflow: "hidden", backgroundColor: t.muted, alignItems: "center", justifyContent: "center" }}
    >
      {uri ? (
        <Image source={{ uri }} style={{ width: SIZE, height: SIZE }} resizeMode="cover" />
      ) : error ? (
        <View style={{ alignItems: "center", gap: 4, padding: 8 }}>
          <T size={12} color={t.sidebarForeground} style={{ textAlign: "center" }} lines={3}>{`图片打不开：${error}`}</T>
          <T size={11.5} color={t.mutedForeground}>点一下重试</T>
        </View>
      ) : (
        <Spinner size={16} color={t.sidebarForeground} />
      )}
    </Pressable>
  );
}

/** One picture over everything, fitted to the screen. */
export function Viewer({ uri, onClose }: { uri?: string; onClose: () => void }) {
  return (
    <Modal visible={!!uri} transparent animationType="fade" onRequestClose={onClose} statusBarTranslucent>
      <StatusBar barStyle="light-content" />
      <Pressable onPress={onClose} style={{ flex: 1, backgroundColor: "rgba(0,0,0,0.94)", alignItems: "center", justifyContent: "center" }}>
        {uri ? <Image source={{ uri }} style={{ width: "100%", height: "100%" }} resizeMode="contain" /> : null}
        <View style={{ position: "absolute", top: 44, right: 16, width: 36, height: 36, borderRadius: 18, backgroundColor: "rgba(255,255,255,0.14)", alignItems: "center", justifyContent: "center" }}>
          <Icon name="x" size={18} color="#fff" />
        </View>
      </Pressable>
    </Modal>
  );
}

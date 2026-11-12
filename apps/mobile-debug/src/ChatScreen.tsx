// One conversation, laid out as Remodex's TurnView: a slim toolbar (sidebar button, title
// and where it runs, in mono), replies as plain text and the user's messages in soft
// bubbles, and a floating composer card: the text, then the model and effort pills and
// the send button; under it where the work happens and what the agent (Codex, DSH) may do. The pending
// approval's choices sit over the composer. It opens a phone session, a conversation the
// computer's Codex keeps (its record first, from the phone's copy, then fresh; the first
// message continues it), or a new conversation (a chat, or work in a project or folder)
// that starts with the first message.
import { useEffect, useRef, useState } from "react";
import { Alert, FlatList, Image, Keyboard, KeyboardAvoidingView, Platform, Pressable, ScrollView, TextInput, View } from "react-native";
import * as ImagePicker from "expo-image-picker";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { TetherError, type Decision, type HistoryThread, type ProjectInfo, type SessionInfo } from "@tether/client";
import { history } from "./files";
import { chatTitle, modelParts, permissionOf, preludeItems, type Continued, type Pick } from "./history";
import { link } from "./link";
import { applyEvent, decisionLabel, emptyChat, foldSteps, pendingApprovals, stepsLabel, type Chat, type ChatItem, type Steps } from "./model";
import { Markdown } from "./MarkdownView";
import { plain } from "./markdown";
import { Pictures, remember, Viewer } from "./pictures";
import { PermissionMenu, RuntimeMenu, useModels } from "./RuntimeMenu";
import { useLink } from "./useLink";
import { folderName, projectOf } from "./entries";
import { CircleButton, Empty, Icon, Menu, Notice, shortPath, Spinner, StatusMark, STATE_WORD, stateColor, T, type IconName } from "./kit";
import { agentLabel, familyOf } from "./agents";
import { COLUMN, columnLeft, useAgentInfos, usePane } from "./context";
import { alpha, MONO, useTheme } from "./theme";

const COMPOSER_MIN = 24;
const COMPOSER_MAX = 140;

export type Opening =
  | { session: string }
  | { agent: string; thread: HistoryThread }
  | { agent: string; chat: true }
  | { agent: string; cwd: string; project?: ProjectInfo };

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** How far the keyboard covers the screen above the system bar, on Android. */
function useKeyboardPad(): number {
  const [pad, setPad] = useState(0);
  useEffect(() => {
    // Edge-to-edge Android neither resizes the window nor tells a KeyboardAvoidingView
    // where it really is; the keyboard's own height (above the system bar) is right.
    if (Platform.OS !== "android") return;
    const show = Keyboard.addListener("keyboardDidShow", e => setPad(Math.max(0, e.endCoordinates.height)));
    const hide = Keyboard.addListener("keyboardDidHide", () => setPad(0));
    return () => {
      show.remove();
      hide.remove();
    };
  }, []);
  return pad;
}

const TOOL_WORD: Record<string, string> = { shell: "运行", Bash: "运行", edit: "编辑", Write: "编辑", Edit: "编辑", webSearch: "搜索", Read: "读取" };

export function ChatScreen({ opening, onMenu, onNewChat, onSession }: {
  opening: Opening;
  /** Opens the sidebar; none when it is always there (a wide screen). */
  onMenu?: () => void;
  onNewChat: () => void;
  /** Which phone session this screen shows, once there is one. */
  onSession?: (id: string) => void;
}) {
  const t = useTheme();
  const insets = useSafeAreaInsets();
  const snap = useLink();
  const keyboard = useKeyboardPad();
  const [session, setSession] = useState<string | undefined>("session" in opening ? opening.session : undefined);
  const info: SessionInfo | undefined = snap.sessions.find(s => s.id === session);
  const agent = "agent" in opening ? opening.agent : info?.agent;
  const agents = useAgentInfos();
  const pane = usePane();
  /** How texts name the agent: "Codex", "DSH", … */
  const who = agentLabel(agent, agents);
  const family = agent ? familyOf(agent, agents) : undefined;
  const isCodex = family === "codex";
  const [continued, setContinued] = useState<Continued | undefined>(
    "thread" in opening ? { agent: opening.agent, thread: opening.thread } : undefined,
  );
  // Until we know whether this continues a conversation, nothing is subscribed.
  const [resolved, setResolved] = useState("chat" in opening || "cwd" in opening);
  const [menu, setMenu] = useState(false);
  const [prelude, setPrelude] = useState<ChatItem[]>([]);
  const [note, setNote] = useState<string>();
  const [reading, setReading] = useState(false);
  const [chat, setChat] = useState<Chat>(emptyChat);
  const [text, setText] = useState("");
  const [height, setHeight] = useState(COMPOSER_MIN);
  const [problem, setProblem] = useState<string>();
  const [sending, setSending] = useState(false);
  const [pick, setPick] = useState<Pick>({ model: null, effort: null, speed: null });
  const [picking, setPicking] = useState(false);
  const [permitting, setPermitting] = useState(false);
  const [composer, setComposer] = useState(120);
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const list = useRef<FlatList<ChatItem | Steps>>(null);
  // Whether the list is at its end: new output follows it there, and leaves it alone
  // while you read further up.
  const atEnd = useRef(true);
  const [away, setAway] = useState(false);
  const seq = useRef(0);
  seq.current = chat.seq;

  useEffect(() => {
    link.view(session);
    if (session) onSession?.(session);
    return () => link.view(undefined);
  }, [session]);

  // Which session continues which conversation. A continued session shows the
  // conversation as the computer keeps it, then only what happens from now on.
  useEffect(() => {
    let gone = false;
    const liveSeq = (id: string) => link.snapshot.sessions.find(s => s.id === id)?.last_seq ?? 0;
    void (async () => {
      if ("session" in opening) {
        const at = liveSeq(opening.session);
        const c = await history.continued(opening.session).catch(() => undefined);
        if (gone) return;
        if (c) {
          setContinued(c);
          setChat({ ...emptyChat, seq: at });
        } else {
          // A session the phone started (or forked) has only its own turns; the
          // conversation may have gone on in the Codex app, or begun before the fork.
          // The computer's record has all of it. When it cannot be read, or a turn is
          // running or stopped on an error (its events are not in the record yet, or
          // say what to do next), the session's own events show, as before.
          const own = link.snapshot.sessions.find(s => s.id === opening.session);
          const client = link.client;
          if (own?.thread && (own.state === "idle" || own.state === "exited") && client && link.snapshot.phase === "open") {
            const data = await client.readHistory(own.agent, own.thread).catch(() => undefined);
            if (gone) return;
            if (data) {
              fresh.current = true;
              setPrelude(preludeItems(data.events));
              setNote(data.truncated ? "更早的内容太长，只显示最近的部分" : undefined);
              setContinued({ agent: own.agent, thread: data.thread });
              setChat({ ...emptyChat, seq: at });
              void history.saveThread(own.agent, data);
            }
          }
        }
      } else if ("thread" in opening) {
        const sessions = link.snapshot.sessions.filter(s => s.state !== "exited");
        const on = sessions.find(s => s.agent === opening.agent && s.thread === opening.thread.id)?.id;
        const s = on ?? (await history.sessionOf(opening.agent, opening.thread.id, sessions.map(s => s.id)).catch(() => undefined));
        if (gone) return;
        if (s) {
          setSession(s);
          setChat({ ...emptyChat, seq: liveSeq(s) });
        }
      }
      setResolved(true);
    })();
    return () => {
      gone = true;
    };
  }, []);

  // The conversation's record: the phone's copy at once, the computer's when connected.
  const open = snap.phase === "open";
  const fresh = useRef(false);
  useEffect(() => {
    if (!continued) return;
    let gone = false;
    const { agent: a, thread } = continued;
    void history.thread(a, thread.id).then(saved => {
      if (!gone && saved && !fresh.current) setPrelude(preludeItems(saved.events));
    });
    const client = link.client;
    if (!open || !client || fresh.current) return;
    setNote("正在从电脑读取…");
    setReading(true);
    client
      .readHistory(a, thread.id)
      .then(data => {
        if (gone) return;
        fresh.current = true;
        setPrelude(preludeItems(data.events));
        setNote(data.truncated ? "更早的内容太长，只显示最近的部分" : undefined);
        void history.saveThread(a, data);
      })
      .catch(e => !gone && setNote(`读取电脑上的记录失败，显示的是手机上的副本：${message(e)}`))
      .finally(() => !gone && setReading(false));
    return () => {
      gone = true;
      setReading(false);
    };
  }, [continued, open]);

  useEffect(() => {
    if (agent) void history.projects(agent).then(p => p && setProjects(p));
  }, [agent]);

  // Asked each time a conversation opens: the computer keeps the answer while its
  // settings stay as they are, and asks Codex again after a switch (CC Switch).
  const { models, permissions, loading: modelsLoading, error: modelsError, retry: retryModels } = useModels(agent, open, false, picking || permitting);

  // One subscription per client: TetherClient resubscribes by itself after a
  // reconnect; only a new client (after a failed start) needs a new one.
  // The last client generation seen open: reconnecting keeps it, so the effect stays.
  const ready = useRef(-1);
  if (open) ready.current = snap.generation;
  useEffect(() => {
    const client = link.client;
    if (!session || !resolved || ready.current < 0 || !client) return;
    let stop: (() => void) | undefined;
    let gone = false;
    client
      .subscribe(session, e => setChat(c => applyEvent(c, e)), seq.current)
      .then(s => (gone ? s() : (stop = s), setProblem(undefined)))
      .catch(e => !gone && setProblem(`无法打开会话：${message(e)}`));
    return () => {
      gone = true;
      stop?.();
    };
  }, [session, resolved, ready.current]);

  const act = (f: () => Promise<void>) => () => void f().catch(e => setProblem(message(e)));

  /** The first message: a session that continues the conversation, or a new one. */
  const start = async (typed: string): Promise<SessionInfo> => {
    const client = link.client!;
    if (!agent) throw new Error("没有可用的 agent");
    // A new conversation is allowed what the computer's Codex app allows now, unless chosen here.
    const choice = { model: pick.model, effort: pick.effort, speed: pick.speed, permission: pick.permission ?? permissions.default };
    if (!continued) {
      const where =
        "cwd" in opening
          ? { cwd: opening.cwd, project: opening.project?.id ?? null, roots: opening.project?.roots ?? null }
          : { chat: true };
      return client.createSession(agent, { ...where, title: chatTitle(typed), ...choice });
    }
    const { thread } = continued;
    const ask = (cwd?: string) =>
      client.createSession(agent, { resume: thread.id, chat: thread.chat, title: thread.title, ...choice, ...(cwd ? { cwd } : {}) });
    let created: SessionInfo;
    try {
      created = await ask(thread.cwd || undefined);
    } catch (e) {
      // The folder it ran in is gone: continue from the home folder.
      if (!(e instanceof TetherError && e.code === "bad_request" && /cwd/.test(e.message))) throw e;
      created = await ask();
    }
    await history.link(created.id, continued);
    return created;
  };

  /** Pictures picked to go with the next message. */
  const [attached, setAttached] = useState<Attachment[]>([]);
  const [viewing, setViewing] = useState<string>();
  const attach = act(async () => {
    const picked = await ImagePicker.launchImageLibraryAsync({
      mediaTypes: ["images"],
      allowsMultipleSelection: true,
      selectionLimit: MAX_ATTACHED - attached.length,
      quality: 0.85,
      base64: true,
    });
    if (picked.canceled) return;
    const fits = picked.assets.filter(a => a.base64 && a.base64.length <= MAX_BASE64);
    if (fits.length < picked.assets.length) setProblem("有图片超过 5 MB，没有加上");
    setAttached(old => [
      ...old,
      ...fits.map((a, i) => {
        const mime = a.mimeType ?? "image/jpeg";
        return { uri: a.uri, mime, data: a.base64!, name: pictureName(a.fileName, mime, i) };
      }),
    ].slice(0, MAX_ATTACHED));
  });
  /** Puts the attached pictures on the computer: their paths there. */
  const upload = async (pictures: Attachment[]): Promise<string[]> => {
    const paths: string[] = [];
    for (const a of pictures) {
      const { path } = await link.client!.putFile(a.name, a.data);
      remember(path, a.mime, a.data);
      paths.push(path);
    }
    return paths;
  };

  // What was just sent shows at once, dimmed, until the computer's record of it arrives.
  const [pending, setPending] = useState<Pending>();
  const asked = chat.items.reduce((n, x) => n + (x.kind === "user" ? 1 : 0), 0);
  const askedNow = useRef(0);
  askedNow.current = asked;
  useEffect(() => {
    if (pending && asked > pending.base) setPending(undefined);
  }, [asked, pending]);

  const send = act(async () => {
    const typed = text.trim();
    const pictures = attached;
    // The computer takes no message while a turn runs: the button is "stop" then.
    if ((!typed && !pictures.length) || sending || working) return;
    setSending(true);
    setPending({ text: typed, uris: pictures.map(a => a.uri), base: askedNow.current });
    setText("");
    setAttached([]);
    setHeight(COMPOSER_MIN);
    setProblem(undefined);
    // What you send, and the answer to it, are at the end: the list goes there.
    atEnd.current = true;
    // Not sent after all: it goes back in the composer, unless something new is there.
    const giveBack = () => {
      setPending(undefined);
      setText(now => now || typed);
      setAttached(now => (now.length ? now : pictures));
    };
    try {
      let id = session;
      if (!id) {
        const created = await start(typed || "图片");
        id = created.id;
        // The computer may hand back a session that is still working on it.
        const busy = created.state === "working" || created.state === "needs_you";
        setChat({ ...emptyChat, seq: busy ? created.last_seq : 0 });
        setSession(id);
        if (busy) {
          giveBack();
          setProblem("电脑上这个对话还在进行，等它完成再发");
          return;
        }
      }
      const images = await upload(pictures);
      await link.client!.send(id, typed, images);
      setPending(p => p && { ...p, sent: true });
    } catch (e) {
      giveBack();
      throw e;
    } finally {
      setSending(false);
    }
  });
  const answer = (approval: string, d: Decision) => act(() => link.client!.answer(session!, approval, d))();
  const chosen: Pick = session
    ? { model: info?.model ?? null, effort: info?.effort ?? null, speed: info?.speed ?? null, permission: info?.permission ?? null }
    : pick;
  const choose = (p: Pick) => {
    if (!session) return setPick(p);
    // `null` leaves a choice as it is: send only what changed.
    const changed = (k: keyof Pick) => ((p[k] ?? null) !== (chosen[k] ?? null) ? (p[k] ?? null) : null);
    const [model, effort, speed, permission] = [changed("model"), changed("effort"), changed("speed"), changed("permission")];
    if (model === null && effort === null && speed === null && permission === null) return;
    act(() => link.client!.setModel(session, model, effort, speed, permission))();
  };
  /** Full access is asked about first: nothing is asked after it. */
  const choosePermission = (id: string) => {
    const apply = () => choose({ ...chosen, permission: id });
    if (id !== ":danger-full-access") return apply();
    Alert.alert(`给 ${who} 完全访问？`, `${who} 改电脑上任何文件、运行任何命令都不再先问你。`, [
      { text: "取消", style: "cancel" },
      { text: "完全访问", style: "destructive", onPress: apply },
    ]);
  };
  const thread = continued?.thread;
  /** The computer's own conversation this screen is on, once there is one. */
  const threadId = info?.thread ?? thread?.id;
  const reveal = () =>
    Alert.alert(
      "交给电脑上的 Codex？",
      "电脑上的 Codex 桌面版会打开这个对话，可以在电脑上接着聊。同一个对话同一时间只能在一边继续：之后在手机上再发，如果桌面版还开着它，需要分叉成新对话。",
      [
        { text: "取消", style: "cancel" },
        {
          text: "打开",
          onPress: act(async () => {
            if (!agent || !threadId) return;
            await link.client!.reveal(agent, threadId);
            Alert.alert("已在电脑上打开", "如果桌面版提示“已在另一个应用中打开”，点“重试”即可。");
          }),
        },
      ],
    );
  // Another program (the Codex app) has the conversation: offer a copy to go on in.
  const lastNotice = [...chat.items].reverse().find(x => x.kind === "notice");
  const taken = !!session && info?.state === "error" && lastNotice?.kind === "notice" && lastNotice.text.includes("分叉成新对话");
  const fork = act(async () => {
    if (!agent || !threadId || sending) return;
    setSending(true);
    try {
      const lastAsk = [...chat.items].reverse().find(x => x.kind === "user");
      const created = await link.client!.createSession(agent, {
        resume: threadId,
        fork: true,
        chat: isChat,
        title: title,
        model: chosen.model,
        effort: chosen.effort,
        speed: chosen.speed,
        permission: access.id,
        ...(cwd ? { cwd } : {}),
      });
      // What happened before stays on screen; the copy carries it on.
      setPrelude(p => [...p, ...chat.items.filter(x => x.kind !== "notice" && x.key !== lastAsk?.key).map(x => ({ ...x, key: `f:${x.key}` }))]);
      setChat(emptyChat);
      setSession(created.id);
      if (lastAsk?.kind === "user") await link.client!.send(created.id, lastAsk.text, lastAsk.images);
      setProblem(undefined);
    } finally {
      setSending(false);
    }
  });

  const state = info?.state ?? chat.state;
  const working = !!session && (state === "working" || state === "needs_you");
  const approvals = pendingApprovals(chat);
  const waiting = approvals[0];
  const isChat = info?.chat ?? (thread ? thread.chat : "chat" in opening);
  const cwd = info?.cwd || thread?.cwd || ("cwd" in opening ? opening.cwd : "");
  const project =
    ("cwd" in opening ? opening.project : undefined) ??
    (isChat || !cwd ? undefined : projectOf({ key: "", title: "", updated: 0, chat: false, cwd, thread }, projects));
  const title = info?.title || thread?.title || ("chat" in opening || "cwd" in opening ? "新对话" : agent ?? "会话");
  const access = permissionOf(permissions, chosen.permission, { chat: isChat, session: !!session });
  const where = isChat ? `对话 · ${access.name}` : cwd ? shortPath(cwd) : "";
  const all = prelude.length && chat.items.length ? [...prelude, ...chat.items] : prelude.length ? prelude : chat.items;
  // Reasoning with nothing to show says nothing.
  const items = all.filter(x => x.kind !== "thinking" || x.text.trim());
  // What Codex did on the way is one line under the question; the answer is what shows.
  const shown = foldSteps(items, working);

  const parts = modelParts(chosen, models);
  const canSend = (!!text.trim() || attached.length > 0) && open && !sending && resolved && !working;
  const thinking = working && !waiting && !pending && shown[shown.length - 1]?.kind !== "steps";

  const close = () =>
    Alert.alert("关闭会话？", "电脑上的 agent 会停止，对话记录还在电脑上。", [
      { text: "取消", style: "cancel" },
      { text: "关闭", style: "destructive", onPress: act(async () => { await link.client!.closeSession(session!); onNewChat(); }) },
    ]);
  const explainWhere = () =>
    Alert.alert(
      isChat ? "对话" : project?.name ?? folderName(cwd),
      isChat
        ? isCodex
          ? "没有项目的对话：Codex 在自己的文件夹里，和电脑上 Codex 的“对话”放在一起。"
          : `没有项目的对话：${who} 在 Tether 的对话文件夹里。`
        : project && project.roots.length > 1
          ? `项目“${project.name}”的文件夹：\n${project.roots.join("\n")}`
          : cwd || "电脑上的默认文件夹",
    );
  // An older computer offers no choice: say what applies.
  const explainAccess = () => Alert.alert(access.name, access.description || `电脑上的 ${who} 没有提供可选的权限。`);

  const content = (
    <>
      <View style={{ flexDirection: "row", alignItems: "center", gap: 10, paddingHorizontal: 12, paddingVertical: 8 }}>
        {onMenu ? <CircleButton name="menu" label="打开侧边栏" onPress={onMenu} /> : null}
        <View style={{ flex: 1, paddingLeft: onMenu ? 0 : 6 }}>
          <T size={15} weight="700" mono lines={1}>{title}</T>
          {where || working ? (
            <T size={11.5} mono color={t.sidebarForeground} lines={1}>
              {working ? <T size={11.5} mono color={stateColor(t, state)}>{`${STATE_WORD[state]}${where ? " · " : ""}`}</T> : null}
              {where}
            </T>
          ) : null}
        </View>
        {threadId && agent && isCodex ? <CircleButton name="laptop" label="在电脑上打开" onPress={reveal} disabled={!open} /> : null}
        <CircleButton name="more" label="更多" onPress={() => setMenu(true)} />
      </View>
      {snap.phase !== "open" ? (
        <Notice
          tone="warn"
          busy={snap.phase !== "failed"}
          lines={1}
          text={snap.phase === "failed" ? "连不上电脑，稍后自动重试" : snap.phase === "connecting" ? "正在连接电脑…" : "重连中…"}
          action={snap.phase === "failed" ? { label: "重试", onPress: () => void link.retry() } : undefined}
          style={{ marginHorizontal: 16, marginBottom: 4 }}
        />
      ) : null}
      <View style={{ flex: 1 }}>
        <FlatList
          ref={list}
          style={{ flex: 1 }}
          contentContainerStyle={{ flexGrow: 1, paddingHorizontal: 18, paddingTop: 8, paddingBottom: 12, gap: 16, width: "100%", maxWidth: COLUMN, alignSelf: "center" }}
          data={shown}
          keyExtractor={x => x.key}
          keyboardShouldPersistTaps="handled"
          onScroll={e => {
            const { contentOffset, contentSize, layoutMeasurement } = e.nativeEvent;
            const end = contentOffset.y + layoutMeasurement.height >= contentSize.height - 48;
            atEnd.current = end;
            if (end === away) setAway(!end);
          }}
          scrollEventThrottle={100}
          onContentSizeChange={() => atEnd.current && list.current?.scrollToEnd({ animated: false })}
          ListHeaderComponent={note && !(reading && !shown.length) ? <T size={12} color={t.sidebarForeground} style={{ textAlign: "center" }}>{note}</T> : undefined}
          ListFooterComponent={pending || thinking ? (
            <View style={{ gap: 16 }}>
              {pending ? <PendingBubble pending={pending} /> : null}
              {thinking ? (
                <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
                  <Spinner size={14} color={t.mutedForeground} />
                  <T size={13.5} mono color={t.mutedForeground} lines={1} style={{ flex: 1 }}>{chat.statusLine || "正在思考…"}</T>
                </View>
              ) : null}
            </View>
          ) : undefined}
          ListEmptyComponent={
            pending ? undefined : continued || !resolved ? (
              reading || !resolved ? <Empty busy text="正在从电脑读取对话…" style={{ flex: 1 }} /> : note ? undefined : <Empty text="这个对话还没有内容" style={{ flex: 1 }} />
            ) : (
              <View style={{ flex: 1, alignItems: "center", justifyContent: "center", gap: 12, paddingHorizontal: 28 }}>
                <View style={{ width: 56, height: 56, borderRadius: 18, backgroundColor: t.primary, alignItems: "center", justifyContent: "center" }}>
                  <Icon name="sparkles" size={26} color={t.primaryForeground} />
                </View>
                <T size={20} mono style={{ textAlign: "center" }}>
                  {!open ? "等待连接电脑…" : isChat ? "有什么可以帮你？" : `在 ${project?.name ?? (folderName(cwd) || "这个文件夹")} 里做点什么？`}
                </T>
                <T size={12.5} color={t.mutedForeground} style={{ textAlign: "center", lineHeight: 18 }}>
                  {`${isChat ? `${who} 在自己的对话文件夹里` : `${who} 会在 ${cwd} 里工作`}。权限：${access.name}${access.description ? `，${access.description}` : ""}。`}
                </T>
              </View>
            )
          }
          renderItem={({ item }) => (item.kind === "steps" ? <StepsLine steps={item} /> : <Item item={item} />)}
        />
        {away ? (
          <Pressable
            accessibilityLabel="回到最新"
            onPress={() => {
              atEnd.current = true;
              setAway(false);
              list.current?.scrollToEnd({ animated: true });
            }}
            style={({ pressed }) => ({
              position: "absolute", alignSelf: "center", bottom: 10, width: 36, height: 36, borderRadius: 18,
              alignItems: "center", justifyContent: "center", backgroundColor: pressed ? t.accent : t.background,
              borderWidth: 1, borderColor: t.border, shadowColor: "#000", shadowOpacity: 0.08, shadowRadius: 8, shadowOffset: { width: 0, height: 2 }, elevation: 3,
            })}
          >
            <Icon name="chevron-down" size={16} color={t.foreground} />
          </Pressable>
        ) : null}
      </View>
      <View onLayout={e => setComposer(e.nativeEvent.layout.height)} style={{ paddingHorizontal: 12, paddingTop: 4, paddingBottom: 6, gap: 8, width: "100%", maxWidth: COLUMN, alignSelf: "center" }}>
        {problem ? <Notice text={problem} onClose={() => setProblem(undefined)} /> : null}
        {/* The computer's agent cannot run yet (DSH with no model set up, or not starting): say so before anything is typed. */}
        {modelsError && !models.length && !modelsLoading && open && !working ? (
          <Notice tone="warn" text={modelsError} lines={6} action={{ label: "重试", onPress: retryModels }} />
        ) : null}
        {taken ? (
          <View style={{ borderRadius: 20, borderWidth: 1, borderColor: t.border, backgroundColor: t.muted, padding: 12, gap: 8 }}>
            <T size={13} color={t.mutedForeground} style={{ lineHeight: 19 }}>电脑上的 Codex 桌面版正开着这个对话。可以复制成新对话在手机上接着聊（上一条消息会重新发出）。</T>
            <View style={{ flexDirection: "row" }}>
              <Choice label="分叉成新对话继续" primary disabled={!open || sending} onPress={fork} />
            </View>
          </View>
        ) : null}
        {waiting ? (
          <View style={{ borderRadius: 20, borderWidth: 1, borderColor: alpha(t.chart2, 0.35), backgroundColor: alpha(t.chart2, 0.05), padding: 12, gap: 8 }}>
            <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
              <StatusMark state="needs_you" compact />
              <T size={14} weight="600" lines={2} style={{ flex: 1 }}>{waiting.title}</T>
              {approvals.length > 1 ? <T size={12} color={t.chart2}>{`还有 ${approvals.length - 1} 个`}</T> : null}
            </View>
            {waiting.detail ? (
              // What is being approved, in full: it scrolls when it is long.
              <ScrollView nestedScrollEnabled keyboardShouldPersistTaps="always" style={{ maxHeight: 118, borderRadius: 12, backgroundColor: t.background, borderWidth: 1, borderColor: t.border }} contentContainerStyle={{ paddingHorizontal: 10, paddingVertical: 8 }}>
                <T size={12.5} mono selectable>{waiting.detail}</T>
              </ScrollView>
            ) : null}
            {waiting.cwd ? <T size={12} color={t.sidebarForeground} lines={1}>{`目录：${shortPath(waiting.cwd)}`}</T> : null}
            <ScrollView horizontal showsHorizontalScrollIndicator={false} keyboardShouldPersistTaps="always" contentContainerStyle={{ gap: 8 }}>
              {waiting.choices.map((d, i) => (
                <Choice key={d} label={decisionLabel[d]} primary={i === 0} danger={d === "deny" || d === "abort"} disabled={!open} onPress={() => answer(waiting.id, d)} />
              ))}
            </ScrollView>
          </View>
        ) : null}
        <View
          style={{
            borderRadius: 24, backgroundColor: t.background, borderWidth: 1, borderColor: t.border, paddingTop: 12, paddingBottom: 6,
            shadowColor: "#000", shadowOpacity: 0.07, shadowRadius: 14, shadowOffset: { width: 0, height: 3 }, elevation: 3,
          }}
        >
          {attached.length ? (
            <ScrollView horizontal showsHorizontalScrollIndicator={false} keyboardShouldPersistTaps="always" contentContainerStyle={{ gap: 8, paddingHorizontal: 12, paddingBottom: 10 }}>
              {attached.map(a => (
                <Pressable key={a.uri} onPress={() => setViewing(a.uri)} style={{ width: 58, height: 58 }}>
                  <Image source={{ uri: a.uri }} style={{ width: 58, height: 58, borderRadius: 12, backgroundColor: t.muted }} />
                  <Pressable
                    accessibilityLabel="去掉这张图片"
                    hitSlop={8}
                    onPress={() => setAttached(old => old.filter(x => x !== a))}
                    style={{ position: "absolute", top: 3, right: 3, width: 20, height: 20, borderRadius: 10, backgroundColor: "rgba(0,0,0,0.6)", alignItems: "center", justifyContent: "center" }}
                  >
                    <Icon name="x" size={12} color="#fff" />
                  </Pressable>
                </Pressable>
              ))}
            </ScrollView>
          ) : null}
          <TextInput
            style={{
              minHeight: COMPOSER_MIN, height: Math.min(COMPOSER_MAX, Math.max(COMPOSER_MIN, height)), paddingHorizontal: 16, paddingVertical: 0,
              color: t.foreground, fontFamily: MONO, fontSize: 15, textAlignVertical: "top",
            }}
            multiline
            value={text}
            onChangeText={setText}
            onContentSizeChange={e => setHeight(e.nativeEvent.contentSize.height)}
            placeholder={working ? "可以先写着，这一轮结束后再发…" : items.length || continued ? "继续追问…" : isChat ? `问 ${who} 点什么…` : `告诉 ${who} 要做什么…`}
            placeholderTextColor={t.sidebarForeground}
          />
          <View style={{ flexDirection: "row", alignItems: "center", gap: 2, paddingHorizontal: 8, paddingTop: 8 }}>
            <Pressable accessibilityLabel="添加图片" onPress={attach} disabled={!open || attached.length >= MAX_ATTACHED} hitSlop={4}
              style={({ pressed }) => ({ width: 34, height: 34, borderRadius: 17, alignItems: "center", justifyContent: "center", backgroundColor: pressed ? t.accent : "transparent", opacity: !open || attached.length >= MAX_ATTACHED ? 0.4 : 1 })}>
              <Icon name="plus" size={18} color={t.mutedForeground} />
            </Pressable>
            <Pill icon={parts.fast ? "zap" : undefined} label={parts.model} onPress={() => setPicking(true)} disabled={!agent} max={170} />
            {parts.effort ? <Pill icon="brain" label={parts.effort} onPress={() => setPicking(true)} disabled={!agent} /> : null}
            <View style={{ flex: 1 }} />
            {working ? (
              <Pressable accessibilityLabel="停止" onPress={act(() => link.client!.interrupt(session!))} disabled={!open}
                style={({ pressed }) => ({ width: 34, height: 34, borderRadius: 17, alignItems: "center", justifyContent: "center", backgroundColor: t.primary, opacity: pressed ? 0.8 : 1 })}>
                <View style={{ width: 11, height: 11, borderRadius: 2, backgroundColor: t.primaryForeground }} />
              </Pressable>
            ) : (
              <Pressable accessibilityLabel="发送" onPress={send} disabled={!canSend}
                style={({ pressed }) => ({ width: 34, height: 34, borderRadius: 17, alignItems: "center", justifyContent: "center", backgroundColor: canSend ? t.primary : t.accent, opacity: pressed ? 0.8 : 1 })}>
                <Icon name="arrow-up" size={18} color={canSend ? t.primaryForeground : t.sidebarForeground} />
              </Pressable>
            )}
          </View>
        </View>
        <View style={{ flexDirection: "row", alignItems: "center", gap: 2, paddingHorizontal: 4 }}>
          <Pill icon={isChat ? "message" : "laptop"} label={isChat ? "对话" : project?.name ?? (folderName(cwd) || "本机")} onPress={explainWhere} max={180} />
          <Pill
            icon="shield"
            label={access.name}
            onPress={permissions.list.length ? () => setPermitting(true) : explainAccess}
            disabled={!agent}
            tint={access.id === ":danger-full-access" ? t.destructive : access.id === ":read-only" ? undefined : t.chart1}
          />
          <View style={{ flex: 1 }} />
          {working ? <Spinner size={14} color={t.sidebarForeground} /> : null}
        </View>
      </View>
      <Viewer uri={viewing} onClose={() => setViewing(undefined)} />
      <PermissionMenu
        visible={permitting}
        onClose={() => setPermitting(false)}
        left={columnLeft(pane, COLUMN, 14)}
        who={who}
        codex={isCodex}
        bottom={insets.bottom + keyboard + 8}
        list={permissions.list}
        current={access.id}
        onPick={choosePermission}
      />
      <RuntimeMenu
        visible={picking}
        onClose={() => setPicking(false)}
        left={columnLeft(pane, COLUMN, 14)}
        family={family}
        bottom={insets.bottom + keyboard + composer + 4}
        models={models}
        loading={modelsLoading}
        error={modelsError}
        current={chosen}
        onPick={choose}
      />
      <Menu
        visible={menu}
        onClose={() => setMenu(false)}
        top={insets.top + 58}
        side="right"
        items={[
          { label: "新对话", icon: "square-pen", onPress: onNewChat },
          { label: isCodex ? "模型、思考与速度" : "模型与思考", icon: "sparkles", onPress: () => setPicking(true), disabled: !agent },
          ...(threadId && agent && isCodex ? [{ label: "在电脑上打开", icon: "laptop" as const, onPress: reveal, disabled: !open }] : []),
          ...(session ? [{ label: "关闭会话", icon: "x" as const, onPress: close, danger: true }] : []),
        ]}
      />
    </>
  );

  return Platform.OS === "ios" ? (
    <KeyboardAvoidingView style={{ flex: 1, backgroundColor: t.background }} behavior="padding">{content}</KeyboardAvoidingView>
  ) : (
    <View style={{ flex: 1, backgroundColor: t.background, paddingBottom: keyboard }}>{content}</View>
  );
}

type Attachment = { uri: string; mime: string; data: string; name: string };
/** A message on its way: `base` is how many of yours the conversation had before it. */
type Pending = { text: string; uris: string[]; base: number; sent?: boolean };

/** The message just sent, as it will look, dimmed until the computer has it. */
function PendingBubble({ pending }: { pending: Pending }) {
  const t = useTheme();
  return (
    <View style={{ alignSelf: "flex-end", maxWidth: "85%", gap: 6, alignItems: "flex-end" }}>
      {pending.uris.length ? (
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6, justifyContent: "flex-end", opacity: pending.sent ? 1 : 0.6 }}>
          {pending.uris.map(uri => <Image key={uri} source={{ uri }} style={{ width: 132, height: 132, borderRadius: 14, backgroundColor: t.muted }} />)}
        </View>
      ) : null}
      {pending.text ? (
        <View style={{ backgroundColor: t.fill, borderRadius: 20, paddingHorizontal: 15, paddingVertical: 10, opacity: pending.sent ? 1 : 0.6 }}>
          <T size={14.5} mono style={{ lineHeight: 22 }}>{pending.text}</T>
        </View>
      ) : null}
      {pending.sent ? null : (
        <View style={{ flexDirection: "row", alignItems: "center", gap: 5, paddingRight: 4 }}>
          <Spinner size={11} color={t.sidebarForeground} />
          <T size={11.5} color={t.sidebarForeground}>发送中…</T>
        </View>
      )}
    </View>
  );
}
const MAX_ATTACHED = 4;
/** `MAX_FILE` (5 MiB) as base64. */
const MAX_BASE64 = Math.ceil((5 << 20) / 3) * 4;
const EXT: Record<string, string> = { "image/png": "png", "image/jpeg": "jpg", "image/gif": "gif", "image/webp": "webp", "image/heic": "heic", "image/bmp": "bmp" };

/** A name the computer takes: it goes by the extension. */
function pictureName(name: string | null | undefined, mime: string, i: number): string {
  if (name && /\.(png|jpe?g|gif|webp|heic|bmp)$/i.test(name)) return name;
  return `image-${Date.now()}-${i}.${EXT[mime] ?? "jpg"}`;
}

/** A small control in the composer: an icon, a word, a chevron, as Remodex's runtime pill. */
function Pill(props: { icon?: IconName; label: string; onPress: () => void; disabled?: boolean; max?: number; tint?: string }) {
  const t = useTheme();
  return (
    <Pressable onPress={props.onPress} disabled={props.disabled} hitSlop={4}
      style={({ pressed }) => ({
        flexDirection: "row", alignItems: "center", gap: 4, maxWidth: props.max ?? 140, height: 34, paddingHorizontal: 8, borderRadius: 17,
        backgroundColor: pressed ? t.accent : "transparent", opacity: props.disabled ? 0.4 : 1,
      })}>
      {props.icon ? <Icon name={props.icon} size={14} color={props.tint ?? t.mutedForeground} /> : null}
      <T size={13.5} mono color={props.tint ?? t.mutedForeground} lines={1} style={{ flexShrink: 1 }}>{props.label}</T>
      <Icon name="chevron-down" size={11} color={t.sidebarForeground} />
    </Pressable>
  );
}

function Choice(props: { label: string; primary?: boolean; danger?: boolean; disabled?: boolean; onPress: () => void }) {
  const t = useTheme();
  return (
    <Pressable
      onPress={props.onPress}
      disabled={props.disabled}
      style={({ pressed }) => ({
        height: 38, maxWidth: 240, paddingHorizontal: 16, justifyContent: "center", borderRadius: 19,
        backgroundColor: props.primary ? t.primary : pressed ? t.accent : t.muted,
        opacity: props.disabled ? 0.5 : props.primary && pressed ? 0.8 : 1,
      })}
    >
      <T size={14.5} weight="500" lines={1} color={props.primary ? t.primaryForeground : props.danger ? t.destructive : t.foreground}>{props.label}</T>
    </Pressable>
  );
}

/** The steps before an answer, folded: what they were, and a tap shows each. */
function StepsLine({ steps }: { steps: Steps }) {
  const t = useTheme();
  const [open, setOpen] = useState(false);
  const last = steps.steps[steps.steps.length - 1];
  const now = last?.kind === "tool" ? `${TOOL_WORD[last.tool] ?? last.tool} ${last.summary}` : last ? plain(last.text).split("\n")[0] : "";
  return (
    <View style={{ gap: 10 }}>
      <Pressable onPress={() => setOpen(!open)} hitSlop={6} style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
        {steps.active ? <Spinner size={12} color={t.sidebarForeground} /> : null}
        <T size={12.5} mono color={t.sidebarForeground} lines={1} style={{ flexShrink: 1 }}>
          {steps.active && !open ? now || "正在思考…" : stepsLabel(steps.steps)}
        </T>
        <View style={{ transform: [{ rotate: open ? "90deg" : "0deg" }] }}><Icon name="chevron-right" size={11} color={t.sidebarForeground} /></View>
      </Pressable>
      {open ? (
        <View style={{ borderLeftWidth: 2, borderColor: t.border, paddingLeft: 12, gap: 12 }}>
          {steps.steps.map(x => <Item key={x.key} item={x} />)}
        </View>
      ) : null}
    </View>
  );
}

function Item({ item }: { item: ChatItem }) {
  const t = useTheme();
  const [open, setOpen] = useState(false);
  switch (item.kind) {
    case "user":
      return (
        <View style={{ alignSelf: "flex-end", maxWidth: "85%", gap: 6 }}>
          {item.images?.length ? <Pictures paths={item.images} /> : null}
          {item.text ? (
            <View style={{ alignSelf: "flex-end", backgroundColor: t.fill, borderRadius: 20, paddingHorizontal: 15, paddingVertical: 10 }}>
              <T size={14.5} mono selectable style={{ lineHeight: 22 }}>{item.text}</T>
            </View>
          ) : null}
        </View>
      );
    case "assistant":
      return <Markdown text={item.text} cursor={!item.done} />;
    case "thinking":
      return (
        <Pressable onPress={() => setOpen(!open)} style={{ gap: 2 }}>
          <View style={{ flexDirection: "row", alignItems: "center", gap: 4 }}>
            <T size={12.5} mono color={t.sidebarForeground}>思考</T>
            <View style={{ transform: [{ rotate: open ? "0deg" : "-90deg" }] }}><Icon name="chevron-down" size={11} color={t.sidebarForeground} /></View>
          </View>
          {open ? (
            <Markdown text={item.text} size={13} color={t.sidebarForeground} />
          ) : (
            <T size={13} mono color={t.sidebarForeground} lines={2} style={{ lineHeight: 19 }}>{plain(item.text)}</T>
          )}
        </Pressable>
      );
    case "tool": {
      const status = item.ok === undefined ? undefined : item.ok ? "完成" : "失败";
      return (
        <Pressable onPress={() => setOpen(!open)} style={{ gap: 6 }}>
          <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
            <T size={12.5} mono color={t.mutedForeground}>{TOOL_WORD[item.tool] ?? item.tool}</T>
            <T size={12.5} mono color={t.sidebarForeground} lines={1} style={{ flex: 1 }}>{item.summary}</T>
            {status ? (
              <T size={11.5} mono color={item.ok ? t.sidebarForeground : t.destructive}>{status}</T>
            ) : (
              <Spinner size={12} color={t.sidebarForeground} />
            )}
            {item.output ? <View style={{ transform: [{ rotate: open ? "90deg" : "0deg" }] }}><Icon name="chevron-right" size={11} color={t.sidebarForeground} /></View> : null}
          </View>
          {open && item.output ? (
            <View style={{ borderRadius: 12, backgroundColor: t.muted, padding: 12 }}>
              <T size={12} mono selectable color={t.mutedForeground}>{item.output}</T>
            </View>
          ) : null}
        </Pressable>
      );
    }
    case "approval":
      return (
        <View style={{ borderRadius: 18, borderWidth: 1, borderColor: item.decision ? t.border : alpha(t.chart2, 0.4), backgroundColor: item.decision ? t.muted : alpha(t.chart2, 0.05), padding: 12, gap: 6 }}>
          <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
            {item.decision ? null : <StatusMark state="needs_you" compact />}
            <T size={14} weight="600" style={{ flex: 1 }}>{item.approval.title}</T>
          </View>
          {item.approval.detail ? <T size={12.5} mono selectable color={t.foreground}>{item.approval.detail}</T> : null}
          {item.approval.cwd ? <T size={12} color={t.sidebarForeground}>目录：{item.approval.cwd}</T> : null}
          <T size={12} color={item.decision ? t.sidebarForeground : t.chart2}>{item.decision ? `已处理：${decisionLabel[item.decision]}` : "在下方选择"}</T>
        </View>
      );
    case "notice": {
      const color = item.level === "error" ? t.destructive : item.level === "warn" ? t.chart1 : t.sidebarForeground;
      return item.level === "info" ? (
        <T size={12.5} mono color={color} selectable style={{ lineHeight: 19 }}>{item.text}</T>
      ) : (
        <View style={{ borderRadius: 12, backgroundColor: alpha(color, 0.08), paddingHorizontal: 12, paddingVertical: 9 }}>
          <T size={13} color={color} selectable style={{ lineHeight: 19 }}>{item.text}</T>
        </View>
      );
    }
    case "turn":
      return item.summary ? <T size={12} mono color={t.sidebarForeground}>{`完成 · ${item.summary}`}</T> : null;
  }
}

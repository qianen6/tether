// The phone's one connection to its computer: TailPocket tunnel + TetherClient.
// Screens read `link.snapshot` through useLink() and call the methods here.
import { AppState, Platform } from "react-native";
import * as Notifications from "expo-notifications";
import * as SecureStore from "expo-secure-store";
import { tailPocket, type TailPocketConnection } from "react-native-tailpocket";
import {
  TetherClient,
  TetherError,
  type ClientState,
  type KeyValueStorage,
  type NotifyMessage,
  type SessionInfo,
} from "@tether/client";
import { startKeepalive, stopKeepalive } from "../modules/tether-keepalive";
import { finished, mergeSession, sortSessions } from "./model";

export const QR_PREFIX = "tether1:";
const SLOT = "tether";
/** Rejections after which the saved pairing is useless. */
const DEAD = new Set(["invalid_token", "revoked", "invalid_secret"]);
/** How long a reconnect may go on before coming back to the app starts the connection over. */
const STALLED = 5000;

export type Phase = "loading" | "unpaired" | "pairing" | "connecting" | ClientState | "failed";

export interface Snapshot {
  phase: Phase;
  hostName?: string;
  error?: string;
  sessions: SessionInfo[];
  /** Milliseconds the last pairing took, scan excluded. */
  pairMs?: number;
  /** Sessions whose turn ended while nobody looked: Herdr's blue "done". */
  done: string[];
  /** Bumped for each new TetherClient, so screens know to subscribe again. */
  generation: number;
  /** When a failed start is tried again, ms since epoch. */
  retryAt?: number;
}

// SecureStore keys allow [A-Za-z0-9._-] only.
const storage: KeyValueStorage = {
  get: k => SecureStore.getItemAsync(`tether_${k}`),
  set: (k, v) => SecureStore.setItemAsync(`tether_${k}`, v),
};

const deviceName = `${Platform.OS === "android" ? "Android" : "iPhone"} 手机`;

const PAIR_ERRORS: Record<string, string> = {
  "invalid-invite": "这不是 Tether 的配对二维码。",
  expired: "二维码已过期，请在电脑上重新生成。",
  unreachable: "连不上电脑：确认电脑上 Tether 正在运行，再重新生成二维码。",
  denied: "电脑拒绝了这次配对。",
  invalid_secret: "配对密钥无效或已用过，请在电脑上重新生成二维码。",
  locked: "失败次数太多，请一分钟后再试。",
};

function pairError(code: string, e: unknown): string {
  const text = e instanceof Error ? e.message : String(e);
  return PAIR_ERRORS[code] ? `${PAIR_ERRORS[code]}（${code}）` : `配对失败：${text}`;
}

class Link {
  snapshot: Snapshot = { phase: "loading", sessions: [], done: [], generation: 0 };
  client?: TetherClient;
  private tunnel?: TailPocketConnection;
  private listeners = new Set<() => void>();
  private unsubscribers: (() => void)[] = [];
  /** The session on screen: no system notification for it while open. */
  private viewing?: string;
  private retryTimer?: ReturnType<typeof setTimeout>;
  private failures = 0;
  /** Since when the client has been reconnecting. */
  private lostAt?: number;
  onOpenSession?: (session: string) => void;

  /** The screen shows `session` (or nothing): its notifications and done mark go. */
  view(session: string | undefined) {
    this.viewing = session;
    if (session && this.snapshot.done.includes(session)) this.set({ done: this.snapshot.done.filter(x => x !== session) });
  }

  subscribe = (fn: () => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  private set(patch: Partial<Snapshot>) {
    this.snapshot = { ...this.snapshot, ...patch };
    for (const fn of this.listeners) fn();
  }

  /** At app start: connect to the saved computer, if any. */
  async start(): Promise<void> {
    clearTimeout(this.retryTimer);
    this.retryTimer = undefined;
    try {
      const saved = await tailPocket.store.link(SLOT);
      const token = await storage.get("token");
      if (!saved || !token) return this.set({ phase: "unpaired" });
      await this.open(saved.name);
      this.failures = 0;
    } catch (e) {
      this.fail(e);
    }
  }

  /** Scan the computer's QR code, join its tunnel, then pair with the gateway. */
  async pair(): Promise<void> {
    try {
      const qr = await tailPocket.scan(QR_PREFIX);
      this.set({ phase: "pairing", error: undefined });
      const started = Date.now();
      // `pair` decodes a text invite with TailPocket's own prefix, not ours:
      // decode it with ours first (as .ref/herdr-anywhere/app/src/tunnel.ts does).
      const invite = await tailPocket.decodeInvite(qr, QR_PREFIX);
      const host = await tailPocket.pair(invite, { device: deviceName, save: true, slot: SLOT });
      if (!host.payload) throw new Error("二维码里没有配对密钥，请在电脑上重新生成");
      await this.open(host.name, host.payload);
      this.set({ pairMs: Date.now() - started });
    } catch (e) {
      this.close();
      await tailPocket.store.deleteLink(SLOT).catch(() => {});
      const code = e instanceof Error && "code" in e ? String(e.code) : "";
      if (code === "cancelled") return this.set({ phase: "unpaired" });
      this.set({ phase: "unpaired", error: pairError(code, e) });
    }
  }

  private async open(hostName: string, pairSecret?: string): Promise<void> {
    this.close();
    this.set({ phase: "connecting", hostName, error: undefined });
    this.tunnel = await tailPocket.connect({ slot: SLOT });
    const url = `${this.tunnel.baseUrl.replace(/^http/, "ws")}/v1/ws`;
    const client = new TetherClient({ url, deviceName, storage, ...(pairSecret ? { pairSecret } : {}) });
    this.client = client;
    this.set({ generation: this.snapshot.generation + 1 });
    this.unsubscribers.push(
      client.on("state", s => this.onState(s)),
      client.on("session_update", s => this.onSession(s)),
      client.on("session_closed", id => this.set({ sessions: this.snapshot.sessions.filter(s => s.id !== id) })),
      client.on("notify", n => void this.notify(n)),
    );
    const welcome = await client.connect();
    this.set({ hostName: welcome.host_name });
    await this.refresh();
  }

  async refresh(): Promise<void> {
    if (!this.client) return;
    this.set({ sessions: sortSessions(await this.client.listSessions()) });
  }

  private onSession(s: SessionInfo) {
    const prev = this.snapshot.sessions.find(x => x.id === s.id)?.state;
    const done = finished(prev, s.state) && s.id !== this.viewing && !this.snapshot.done.includes(s.id)
      ? [...this.snapshot.done, s.id]
      : s.state === "idle" || s.state === "exited" ? this.snapshot.done : this.snapshot.done.filter(x => x !== s.id);
    this.set({ sessions: sortSessions(mergeSession(this.snapshot.sessions, s)), done });
  }

  private onState(s: ClientState) {
    // We unsubscribe before closing ourselves, so "closed" here means the
    // gateway rejected our token: this phone was removed on the computer.
    if (s === "closed") {
      void this.forget("这台手机已被电脑移除，或配对已失效，请重新扫码。");
      return;
    }
    this.set({ phase: s });
    if (s === "reconnecting") this.lostAt ??= Date.now();
    if (s === "open") {
      this.lostAt = undefined;
      startKeepalive("Tether 已连接", this.snapshot.hostName ?? "");
      void this.refresh().catch(() => {});
    }
  }

  private fail(e: unknown) {
    const code = e instanceof TetherError ? e.code : undefined;
    if (code && DEAD.has(code)) {
      void this.forget("这台手机已被电脑移除，或配对已失效，请重新扫码。");
      return;
    }
    this.close();
    // The computer may be asleep or the network gone: keep trying, slower each time.
    const delay = Math.min(60_000, 3000 * 2 ** Math.min(this.failures++, 5));
    this.retryTimer = setTimeout(() => void this.start(), delay);
    this.set({ phase: "failed", error: e instanceof Error ? e.message : String(e), retryAt: Date.now() + delay });
  }

  /** Retry now after a failure. */
  async retry(): Promise<void> {
    if (this.snapshot.phase !== "failed") return;
    this.set({ phase: "connecting", error: undefined });
    await this.start();
  }

  /**
   * The app is in front again. A failed start need not wait for its timer; a reconnect
   * that has gone on for a while (the client waits up to 30 s between tries, and the
   * tunnel may have died in the background) starts over, tunnel included.
   */
  async wake(): Promise<void> {
    if (this.snapshot.phase === "failed") return this.retry();
    if (this.snapshot.phase !== "reconnecting" || !this.lostAt || Date.now() - this.lostAt < STALLED) return;
    this.lostAt = undefined;
    await this.start();
  }

  private close() {
    this.lostAt = undefined;
    for (const u of this.unsubscribers.splice(0)) u();
    this.client?.close();
    this.client = undefined;
    void this.tunnel?.close().catch(() => {});
    this.tunnel = undefined;
    stopKeepalive();
  }

  /** Drops the pairing on this phone (the computer keeps its record until revoked there). */
  async forget(reason?: string): Promise<void> {
    clearTimeout(this.retryTimer);
    this.retryTimer = undefined;
    this.close();
    await tailPocket.store.deleteLink(SLOT).catch(() => {});
    await SecureStore.deleteItemAsync("tether_token").catch(() => {});
    this.set({ phase: "unpaired", sessions: [], done: [], hostName: undefined, error: reason });
  }

  private async notify(n: NotifyMessage) {
    const foreground = AppState.currentState === "active";
    if (foreground && n.session && n.session === this.viewing) return;
    await Notifications.scheduleNotificationAsync({
      content: { title: n.title, body: n.body, data: { session: n.session ?? null, term: n.term ?? null } },
      trigger: null,
    });
  }
}

export const link = new Link();

Notifications.setNotificationHandler({
  handleNotification: async () => ({
    shouldShowBanner: true,
    shouldShowList: true,
    shouldPlaySound: true,
    shouldSetBadge: false,
  }),
});

function opened(r: Notifications.NotificationResponse | null) {
  const session = r?.notification.request.content.data?.session;
  if (typeof session === "string") link.onOpenSession?.(session);
}

Notifications.addNotificationResponseReceivedListener(opened);

/** The notification that launched the app, if one did: call once onOpenSession is set. */
export async function openLaunchNotification(): Promise<void> {
  opened(await Notifications.getLastNotificationResponseAsync().catch(() => null));
  await Notifications.clearLastNotificationResponseAsync().catch(() => {});
}

AppState.addEventListener("change", s => {
  if (s === "active") void link.wake();
});

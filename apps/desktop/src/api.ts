import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import type { AgentInfo } from "../../../packages/client/src/generated/AgentInfo";

export type { AgentInfo };

// These are desktop IPC DTOs, not additional phone protocol definitions.
export interface PairInfo {
  qr: string;
  expires_at: number;
}

export interface DeviceInfo {
  id: string;
  name: string;
  paired_at: number;
  last_seen: number | null;
}

export interface HostStatus {
  tunnel_address: string | null;
  gateway: string;
  connected_devices: number;
  sessions_working: number;
  sessions_need_you: number;
  fake_host: boolean;
}

/** An agent whose program can be chosen, and the chosen one (null: Tether finds it). */
export interface AgentLocation {
  agent: string;
  program: string | null;
}

/** A chosen program that passed its check. */
export interface ProgramCheck {
  program: string;
  /** What `--version` printed; shells are not asked. */
  version: string | null;
}

export interface DesktopApi {
  pair(): Promise<PairInfo>;
  currentPair(): Promise<PairInfo | null>;
  devices(): Promise<DeviceInfo[]>;
  revoke(id: string): Promise<void>;
  agents(): Promise<AgentInfo[]>;
  status(): Promise<HostStatus>;
  keepAwake(): Promise<boolean>;
  setKeepAwake(enabled: boolean): Promise<boolean>;
  autostart(): Promise<boolean>;
  setAutostart(enabled: boolean): Promise<boolean>;
  /** Agents whose program can be chosen, with the program chosen for each. */
  locations(): Promise<AgentLocation[]>;
  /** Checks the program at `path` (a file or its folder) for `agent` and keeps it; rejects with why not. */
  setLocation(agent: string, path: string): Promise<ProgramCheck>;
  /** Forgets `agent`'s chosen program: Tether finds it by itself again. */
  clearLocation(agent: string): Promise<void>;
  onPairing(handler: (pair: PairInfo) => void): Promise<UnlistenFn>;
  onError(handler: (message: string) => void): Promise<UnlistenFn>;
}

export const desktopApi: DesktopApi = {
  pair: () => invoke("pair"),
  currentPair: () => invoke("current_pair"),
  devices: () => invoke("devices"),
  revoke: (id) => invoke("revoke", { id }),
  agents: () => invoke("agents"),
  status: () => invoke("status"),
  keepAwake: () => invoke("get_keep_awake"),
  setKeepAwake: (enabled) => invoke("set_keep_awake", { enabled }),
  autostart: isEnabled,
  setAutostart: async (enabled) => {
    await (enabled ? enable() : disable());
    return isEnabled();
  },
  locations: () => invoke("locations"),
  setLocation: (agent, path) => invoke("set_location", { agent, path }),
  clearLocation: (agent) => invoke("clear_location", { agent }),
  onPairing: (handler) => listen<PairInfo>("desktop:pairing", (event) => handler(event.payload)),
  onError: (handler) => listen<string>("desktop:error", (event) => handler(event.payload)),
};

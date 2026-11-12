import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import { desktopApi } from "./api";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-autostart", () => ({ enable: vi.fn(), disable: vi.fn(), isEnabled: vi.fn() }));
beforeEach(() => vi.resetAllMocks());

describe("desktop IPC", () => {
  it("uses the registered command names and argument keys", async () => {
    await desktopApi.pair();
    await desktopApi.currentPair();
    await desktopApi.devices();
    await desktopApi.revoke("device-id");
    await desktopApi.agents();
    await desktopApi.status();
    await desktopApi.keepAwake();
    await desktopApi.setKeepAwake(true);
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ["pair"], ["current_pair"], ["devices"], ["revoke", { id: "device-id" }],
      ["agents"], ["status"], ["get_keep_awake"], ["set_keep_awake", { enabled: true }],
    ]);
  });

  it("uses the autostart plugin and reads back the OS state", async () => {
    vi.mocked(isEnabled).mockResolvedValueOnce(true).mockResolvedValueOnce(true).mockResolvedValueOnce(false);
    expect(await desktopApi.autostart()).toBe(true);
    expect(await desktopApi.setAutostart(true)).toBe(true);
    expect(enable).toHaveBeenCalledOnce();
    expect(await desktopApi.setAutostart(false)).toBe(false);
    expect(disable).toHaveBeenCalledOnce();
  });

  it("unpacks tray event payloads and returns the unlisten function", async () => {
    const off = vi.fn();
    vi.mocked(listen).mockResolvedValue(off);
    const onPair = vi.fn();
    expect(await desktopApi.onPairing(onPair)).toBe(off);
    const [name, callback] = vi.mocked(listen).mock.calls[0];
    expect(name).toBe("desktop:pairing");
    const pair = { qr: "tether1:tray", expires_at: 123 };
    callback({ event: name, id: 1, payload: pair });
    expect(onPair).toHaveBeenCalledWith(pair);
    const onError = vi.fn();
    await desktopApi.onError(onError);
    const [errorName, errorCallback] = vi.mocked(listen).mock.calls[1];
    expect(errorName).toBe("desktop:error");
    errorCallback({ event: errorName, id: 2, payload: "host error" });
    expect(onError).toHaveBeenCalledWith("host error");
  });
});

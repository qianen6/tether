import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import QRCode from "qrcode";
import { App } from "./App";
import { Pairing } from "./Pairing";
import type { DesktopApi, DeviceInfo, PairInfo } from "./api";

const qrDataUrl = vi.hoisted(() => vi.fn<(text: string, options: unknown) => Promise<string>>());
vi.mock("qrcode", () => ({ default: { toDataURL: qrDataUrl } }));

const imageUrl = "data:image/png;base64,ZmFrZQ==";
const pairing = (): PairInfo => ({ qr: "tether1:test-payload", expires_at: Date.now() + 600_000 });

function fakeApi() {
  let devices: DeviceInfo[] = [
    { id: "pixel", name: "Pixel 9", paired_at: 1_730_000_000_000, last_seen: 1_730_000_060_000 },
    { id: "iphone", name: "iPhone", paired_at: 1_730_000_000_000, last_seen: null },
  ];
  const chosen = new Map<string, string>();
  let onPair: (pair: PairInfo) => void = () => {};
  let onError: (message: string) => void = () => {};
  const offPair = vi.fn();
  const offError = vi.fn();
  const api = {
    pair: vi.fn(async () => pairing()),
    currentPair: vi.fn(async (): Promise<PairInfo | null> => null),
    devices: vi.fn(async () => [...devices]),
    revoke: vi.fn(async (id: string) => { devices = devices.filter((device) => device.id !== id); }),
    agents: vi.fn(async () => [
      { id: "codex", name: "Codex", kind: "codex" as const, available: true, chat: true, terminal_command: ["codex"] },
      { id: "claude", name: "Claude Agent", kind: "claude" as const, available: chosen.has("claude"), detail: chosen.has("claude") ? null : "未找到 claude 命令", chat: true, terminal_command: [chosen.get("claude") ?? "claude"] },
      { id: "dsh", name: "DeepSeek Harness", kind: "dsh" as const, available: chosen.has("dsh"), detail: chosen.has("dsh") ? null : "未找到 DSH", chat: true, terminal_command: ["dsh"] },
    ]),
    status: vi.fn(async () => ({ tunnel_address: "fake.local", gateway: "127.0.0.1:43123", connected_devices: devices.length, sessions_working: 1, sessions_need_you: 2, fake_host: true })),
    keepAwake: vi.fn(async () => false),
    setKeepAwake: vi.fn(async (enabled: boolean) => enabled),
    autostart: vi.fn(async () => true),
    setAutostart: vi.fn(async (enabled: boolean) => enabled),
    locations: vi.fn(async () => ["codex", "claude", "dsh"].map((agent) => ({ agent, program: chosen.get(agent) ?? null }))),
    setLocation: vi.fn(async (agent: string, path: string) => {
      const file = path.split("\\").pop() ?? "";
      if (!file.startsWith(agent)) throw `${file} 看起来不是 ${agent}`;
      chosen.set(agent, path);
      return { program: path, version: agent === "dsh" ? "0.1.1-rc.2" : "2.1.0 (Claude Code)" };
    }),
    clearLocation: vi.fn(async (agent: string) => { chosen.delete(agent); }),
    onPairing: vi.fn(async (handler: (pair: PairInfo) => void) => { onPair = handler; return offPair; }),
    onError: vi.fn(async (handler: (message: string) => void) => { onError = handler; return offError; }),
  } satisfies DesktopApi;
  return { api, offPair, offError, emitPair: (pair: PairInfo) => onPair(pair), emitError: (error: string) => onError(error) };
}

beforeEach(() => qrDataUrl.mockResolvedValue(imageUrl));
afterEach(() => { cleanup(); vi.useRealTimers(); vi.clearAllMocks(); });

describe("desktop shell", () => {
  it("checks a program location before keeping it, for any agent", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    // Every agent the Host lists gets a chooser; missing ones say 选择, found ones 更改.
    expect(await screen.findByRole("button", { name: "更改 Codex 的位置" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "选择 DeepSeek Harness 的位置" }));
    const dsh = screen.getByLabelText(/^DeepSeek Harness 程序的完整路径，或它所在的文件夹；也可以填已经在运行的 dsh web/);
    fireEvent.change(dsh, { target: { value: "D:\\DSH\\node.exe" } });
    fireEvent.click(screen.getByRole("button", { name: "检查并保存" }));
    expect((await screen.findByRole("alert")).textContent).toContain("看起来不是 dsh");
    fireEvent.change(dsh, { target: { value: "D:\\DSH\\dsh.cmd" } });
    fireEvent.click(screen.getByRole("button", { name: "检查并保存" }));
    expect((await screen.findByText(/已找到 0\.1\.1-rc\.2/)).textContent).toContain("之后都用这个位置");
    expect(api.setLocation).toHaveBeenLastCalledWith("dsh", "D:\\DSH\\dsh.cmd");
    expect(await screen.findByText("使用选择的位置：D:\\DSH\\dsh.cmd")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "选择 Claude Agent 的位置" }));
    fireEvent.change(screen.getByLabelText("Claude Agent 程序的完整路径，或它所在的文件夹"), { target: { value: "E:\\tools\\claude.exe" } });
    fireEvent.click(screen.getByRole("button", { name: "检查并保存" }));
    expect(await screen.findByText("使用选择的位置：E:\\tools\\claude.exe")).toBeTruthy();
    expect(api.setLocation).toHaveBeenLastCalledWith("claude", "E:\\tools\\claude.exe");

    fireEvent.click(screen.getByRole("button", { name: "DeepSeek Harness 改回自动查找" }));
    await waitFor(() => expect(api.clearLocation).toHaveBeenCalledWith("dsh"));
    expect(await screen.findByRole("button", { name: "选择 DeepSeek Harness 的位置" })).toBeTruthy();
    expect(screen.getByText("使用选择的位置：E:\\tools\\claude.exe")).toBeTruthy();
  });

  it("shows host status, device timestamps and agent availability", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    expect(await screen.findByText("假 Host（开发数据）")).toBeTruthy();
    expect(screen.getByText("隧道：fake.local")).toBeTruthy();
    expect(screen.getByLabelText("已连接设备：2")).toBeTruthy();
    expect(screen.getByLabelText("工作中：1")).toBeTruthy();
    expect(screen.getByLabelText("等待你处理：2")).toBeTruthy();
    expect(screen.getByRole("status").textContent).toBe("隧道已连接");
    expect(screen.getByText("最后在线：从未上线")).toBeTruthy();
    expect(screen.getAllByText(/^配对时间：/)).toHaveLength(2);
    expect(screen.getByText("未找到 claude 命令")).toBeTruthy();
    expect(screen.getByRole("checkbox", { name: "开机自启" }).getAttribute("disabled")).toBeNull();
  });

  it("opens pairing and renders precisely the Host QR payload", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    fireEvent.click(screen.getByRole("button", { name: "配对新手机" }));
    const image = await screen.findByRole("img", { name: "配对二维码" });
    expect(image.getAttribute("src")).toBe(imageUrl);
    expect(api.pair).toHaveBeenCalledOnce();
    expect(QRCode.toDataURL).toHaveBeenCalledWith("tether1:test-payload", { width: 256, margin: 2, errorCorrectionLevel: "M" });
    expect(screen.getByRole("timer").textContent).toMatch(/剩余 (9 分 59|10 分 0) 秒/);
  });

  it("counts down from the absolute deadline and removes an expired QR", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1_730_000_000_000);
    await act(async () => { render(<Pairing pair={{ qr: "tether1:short", expires_at: Date.now() + 2000 }} />); });
    expect(screen.getByRole("timer").textContent).toContain("0 分 2 秒");
    await act(async () => { vi.advanceTimersByTime(1000); });
    expect(screen.getByRole("timer").textContent).toContain("0 分 1 秒");
    await act(async () => { vi.advanceTimersByTime(1000); });
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.getByText("二维码已过期，请重新配对。")).toBeTruthy();
    expect(screen.queryByRole("timer")).toBeNull();
  });

  it("does not render an already expired pairing payload", async () => {
    render(<Pairing pair={{ qr: "expired", expires_at: Date.now() - 1 }} />);
    expect(screen.getByText("二维码已过期，请重新配对。")).toBeTruthy();
    expect(QRCode.toDataURL).not.toHaveBeenCalled();
  });

  it("shows QR encoding failures rather than an empty image", async () => {
    vi.mocked(QRCode.toDataURL).mockRejectedValueOnce(new Error("encoder failed"));
    render(<Pairing pair={pairing()} />);
    expect((await screen.findByRole("alert")).textContent).toContain("二维码生成失败：Error: encoder failed");
    expect(screen.queryByRole("img")).toBeNull();
  });

  it("puts the QR away on request and shows it again on a new pairing", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    expect(screen.queryByRole("button", { name: "收起二维码" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "配对新手机" }));
    await screen.findByRole("img", { name: "配对二维码" });
    fireEvent.click(screen.getByRole("button", { name: "收起二维码" }));
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.queryByRole("button", { name: "收起二维码" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "配对新手机" }));
    expect(await screen.findByRole("img", { name: "配对二维码" })).toBeTruthy();
    expect(api.pair).toHaveBeenCalledTimes(2);
  });

  it("reports pairing errors and allows retry", async () => {
    const { api } = fakeApi();
    api.pair.mockRejectedValueOnce(new Error("pair unavailable"));
    render(<App api={api} />);
    fireEvent.click(screen.getByRole("button", { name: "配对新手机" }));
    expect((await screen.findByRole("alert")).textContent).toContain("pair unavailable");
    fireEvent.click(screen.getByRole("button", { name: "配对新手机" }));
    expect(await screen.findByRole("img", { name: "配对二维码" })).toBeTruthy();
  });

  it("loads a pending tray pairing and accepts new tray pairing events", async () => {
    const context = fakeApi();
    context.api.currentPair.mockResolvedValueOnce({ ...pairing(), qr: "tether1:early-tray" });
    render(<App api={context.api} />);
    await screen.findByRole("img", { name: "配对二维码" });
    expect(QRCode.toDataURL).toHaveBeenCalledWith("tether1:early-tray", expect.anything());
    act(() => context.emitPair({ ...pairing(), qr: "tether1:later-tray" }));
    await waitFor(() => expect(QRCode.toDataURL).toHaveBeenCalledWith("tether1:later-tray", expect.anything()));
    expect(context.api.pair).not.toHaveBeenCalled();
  });

  it("requires confirmation and cancelling never calls revoke", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    const remove = await screen.findByRole("button", { name: "移除 Pixel 9" });
    fireEvent.click(remove);
    const dialog = screen.getByRole("alertdialog", { name: "移除设备？" });
    expect(within(dialog).getByText(/确认移除 Pixel 9/)).toBeTruthy();
    expect(api.revoke).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(screen.getByRole("button", { name: "移除 Pixel 9" })).toBeTruthy();
    expect(document.activeElement).toBe(remove);
    expect(api.revoke).not.toHaveBeenCalled();
  });

  it("confirms a removal, calls the exact id and refreshes devices and status", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "移除 Pixel 9" }));
    fireEvent.click(screen.getByRole("button", { name: "确认移除" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "移除 Pixel 9" })).toBeNull());
    expect(api.revoke).toHaveBeenCalledExactlyOnceWith("pixel");
    expect(screen.getByRole("button", { name: "移除 iPhone" })).toBeTruthy();
    await waitFor(() => expect(screen.getByLabelText("已连接设备：1")).toBeTruthy());
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("keeps a failed removal visible and permits a retry", async () => {
    const { api } = fakeApi();
    api.revoke.mockRejectedValueOnce(new Error("revoke failed"));
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "移除 Pixel 9" }));
    fireEvent.click(screen.getByRole("button", { name: "确认移除" }));
    expect((await screen.findByRole("alert")).textContent).toContain("revoke failed");
    expect(screen.getByRole("button", { name: "移除 Pixel 9" })).toBeTruthy();
    expect(screen.getByRole("alertdialog")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "确认移除" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(api.revoke).toHaveBeenCalledTimes(2);
  });

  it("contains keyboard focus in confirmation and Escape cancels", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    fireEvent.click(await screen.findByRole("button", { name: "移除 Pixel 9" }));
    const dialog = screen.getByRole("alertdialog");
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "取消" }));
    fireEvent.keyDown(dialog, { key: "Tab" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "确认移除" }));
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(api.revoke).not.toHaveBeenCalled();
  });

  it("updates both settings through their real API boundaries", async () => {
    const { api } = fakeApi();
    render(<App api={api} />);
    const autostart = screen.getByRole<HTMLInputElement>("checkbox", { name: "开机自启" });
    const awake = screen.getByRole<HTMLInputElement>("checkbox", { name: "防止电脑休眠" });
    await waitFor(() => expect(autostart.disabled).toBe(false));
    expect(autostart.checked).toBe(true);
    fireEvent.click(autostart);
    await waitFor(() => expect(autostart.checked).toBe(false));
    expect(api.setAutostart).toHaveBeenCalledWith(false);
    fireEvent.click(awake);
    await waitFor(() => expect(awake.checked).toBe(true));
    expect(api.setKeepAwake).toHaveBeenCalledWith(true);
    fireEvent.click(awake);
    await waitFor(() => expect(awake.checked).toBe(false));
    expect(api.setKeepAwake).toHaveBeenLastCalledWith(false);
  });

  it("does not claim a setting succeeded when its API fails", async () => {
    const { api } = fakeApi();
    api.setKeepAwake.mockRejectedValueOnce(new Error("power failed"));
    api.setAutostart.mockRejectedValueOnce(new Error("registry failed"));
    render(<App api={api} />);
    const awake = screen.getByRole<HTMLInputElement>("checkbox", { name: "防止电脑休眠" });
    const autostart = screen.getByRole<HTMLInputElement>("checkbox", { name: "开机自启" });
    await waitFor(() => expect(awake.disabled).toBe(false));
    fireEvent.click(awake);
    expect((await screen.findByRole("alert")).textContent).toContain("power failed");
    expect(awake.checked).toBe(false);
    fireEvent.click(autostart);
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("registry failed"));
    expect(autostart.checked).toBe(true);
  });

  it("reports read errors, refreshes and shows empty states", async () => {
    const { api } = fakeApi();
    api.status.mockRejectedValueOnce(new Error("host failed"));
    api.devices.mockResolvedValue([]);
    api.agents.mockResolvedValue([]);
    render(<App api={api} />);
    expect((await screen.findByRole("alert")).textContent).toContain("host failed");
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    expect(await screen.findByText("还没有配对设备。")).toBeTruthy();
    expect(screen.getByText("未发现 agent。")).toBeTruthy();
  });

  it("unsubscribes from both tray events on unmount", async () => {
    const context = fakeApi();
    const view = render(<App api={context.api} />);
    await waitFor(() => expect(context.api.currentPair).toHaveBeenCalledOnce());
    act(() => context.emitError("托盘配对失败"));
    expect(screen.getByRole("alert").textContent).toBe("托盘配对失败");
    view.unmount();
    expect(context.offPair).toHaveBeenCalledOnce();
    expect(context.offError).toHaveBeenCalledOnce();
  });

  it("does not overwrite a newer tray QR with a slow initial snapshot", async () => {
    const context = fakeApi();
    let finish!: (pair: PairInfo) => void;
    context.api.currentPair.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    render(<App api={context.api} />);
    await waitFor(() => expect(context.api.currentPair).toHaveBeenCalledOnce());
    act(() => context.emitPair({ ...pairing(), qr: "tether1:new-event" }));
    await screen.findByRole("img", { name: "配对二维码" });
    await act(async () => finish({ ...pairing(), qr: "tether1:old-snapshot" }));
    expect(QRCode.toDataURL).not.toHaveBeenCalledWith("tether1:old-snapshot", expect.anything());
  });

  it("drops stale QR encoder results when a pairing is replaced", async () => {
    let finish!: (image: string) => void;
    qrDataUrl.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const view = render(<Pairing pair={{ ...pairing(), qr: "old" }} />);
    view.rerender(<Pairing pair={{ ...pairing(), qr: "new" }} />);
    await screen.findByRole("img", { name: "配对二维码" });
    await act(async () => finish("data:image/png;base64,old"));
    expect(screen.getByRole("img").getAttribute("src")).toBe(imageUrl);
  });

  it("cleans up a tray subscription that resolves after unmount", async () => {
    const context = fakeApi();
    let finish!: (off: typeof context.offPair) => void;
    context.api.onPairing.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const view = render(<App api={context.api} />);
    view.unmount();
    await act(async () => finish(context.offPair));
    expect(context.offPair).toHaveBeenCalledOnce();
    expect(context.api.onError).not.toHaveBeenCalled();
  });
});

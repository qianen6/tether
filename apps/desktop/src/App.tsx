import { useEffect, useRef, useState } from "react";
import { desktopApi, type AgentInfo, type AgentLocation, type DesktopApi, type DeviceInfo, type HostStatus, type PairInfo } from "./api";
import { Pairing } from "./Pairing";
import "./style.css";

/** An example of what to type for `agent`, as it is usually installed on Windows. */
function example(agent: AgentInfo): string {
  if (agent.kind === "dsh") return "C:\\Tools\\DeepSeekHarness\\node-v24.19.0-win-x64\\dsh.cmd";
  if (agent.kind === "terminal") return "C:\\Program Files\\PowerShell\\7\\pwsh.exe";
  const program = agent.terminal_command[0] ?? agent.id;
  const name = program.split(/[\\/]/).pop()?.replace(/\.(cmd|exe|bat)$/i, "") || agent.id;
  return `C:\\Users\\你\\AppData\\Roaming\\npm\\${name}.cmd`;
}

/**
 * Where an agent's program is, for when Tether cannot find it (installed outside PATH and the
 * usual folders) or should use another one: the program or its folder is typed in, checked on
 * the computer (right name, runs `--version`), and kept only when the check passes.
 * "改回自动查找" forgets it.
 */
function ProgramLocation({ api, agent, saved, onChanged }: { api: DesktopApi; agent: AgentInfo; saved: string | null; onChanged: () => void }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [found, setFound] = useState<string | null>(null);
  const field = `location-${agent.id}`;
  const change = Boolean(saved) || agent.available;

  async function check() {
    setBusy(true); setError(null); setFound(null);
    try {
      const result = await api.setLocation(agent.id, text);
      setEditing(false);
      setFound(result.version ? `已找到 ${result.version}，之后都用这个位置。` : "检查通过，之后都用这个位置。");
      onChanged();
    } catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }

  async function clear() {
    setBusy(true); setError(null); setFound(null);
    try { await api.clearLocation(agent.id); onChanged(); }
    catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }

  return <div className="locate">
    {saved && !editing && <p className="meta">使用选择的位置：{saved}</p>}
    {found && <p role="status" className="found">{found}</p>}
    {editing ? <form onSubmit={(event) => { event.preventDefault(); void check(); }}>
      <label htmlFor={field} className="meta">{agent.name} 程序的完整路径，或它所在的文件夹{agent.kind === "dsh" ? "；也可以填已经在运行的 dsh web 打印的地址" : ""}</label>
      <input id={field} value={text} spellCheck={false} autoFocus disabled={busy}
        placeholder={example(agent)}
        onChange={(event) => setText(event.currentTarget.value)} />
      <p className="meta">在资源管理器里按住 Shift 右键程序文件，选“复制为路径”，粘贴到这里。</p>
      {agent.kind === "dsh" && <p className="meta">要连接已经开着的 DSH：把 dsh web 启动时打印的那一行（dsh web: http://127.0.0.1:端口/?token=…）整行粘贴进来。DSH 重启后 token 会变，到时 Tether 会自己启动 DSH，需要的话再重新填写。</p>}
      {error && <p role="alert" className="alert">{error}</p>}
      <div className="actions">
        <button type="submit" className="primary" disabled={busy || !text.trim()}>{busy ? "正在检查…" : "检查并保存"}</button>
        <button type="button" disabled={busy} onClick={() => { setEditing(false); setError(null); }}>取消</button>
      </div>
    </form> : <div className="actions">
      <button type="button" className={change ? "ghost" : undefined} disabled={busy}
        aria-label={`${change ? "更改" : "选择"} ${agent.name} 的位置`}
        onClick={() => { setEditing(true); setFound(null); setText(saved ?? ""); }}>
        {change ? "更改位置" : "选择位置"}
      </button>
      {saved && <button type="button" className="ghost" disabled={busy} aria-label={`${agent.name} 改回自动查找`} onClick={() => void clear()}>改回自动查找</button>}
      {error && <p role="alert" className="alert">{error}</p>}
    </div>}
  </div>;
}

function formatTime(value: number | null): string {
  return value === null ? "从未上线" : new Date(value).toLocaleString();
}

export function App({ api = desktopApi }: { api?: DesktopApi }) {
  const [status, setStatus] = useState<HostStatus | null>(null);
  const [devices, setDevices] = useState<DeviceInfo[] | null>(null);
  const [agents, setAgents] = useState<AgentInfo[] | null>(null);
  const [locations, setLocations] = useState<AgentLocation[]>([]);
  const [pair, setPair] = useState<PairInfo | null>(null);
  const [pairing, setPairing] = useState(false);
  const [overviewError, setOverviewError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [confirmDevice, setConfirmDevice] = useState<DeviceInfo | null>(null);
  const [removing, setRemoving] = useState(false);
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [keepAwake, setKeepAwake] = useState<boolean | null>(null);
  const [saving, setSaving] = useState<"autostart" | "awake" | null>(null);
  const cancelButton = useRef<HTMLButtonElement>(null);
  const confirmButton = useRef<HTMLButtonElement>(null);
  const opener = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    let cancelled = false;
    let busy = false;
    const update = async () => {
      if (busy) return;
      busy = true;
      try {
        const [nextStatus, nextDevices, nextAgents, nextLocations] = await Promise.all([api.status(), api.devices(), api.agents(), api.locations()]);
        if (!cancelled) {
          setStatus(nextStatus); setDevices(nextDevices); setAgents(nextAgents); setLocations(nextLocations); setOverviewError(null);
        }
      } catch (reason) {
        if (!cancelled) setOverviewError(`读取 Host 失败：${String(reason)}`);
      } finally { busy = false; }
    };
    void update();
    const timer = setInterval(() => void update(), 2000);
    return () => { cancelled = true; clearInterval(timer); };
  }, [api, refresh]);

  useEffect(() => {
    let cancelled = false;
    void Promise.all([api.autostart(), api.keepAwake()]).then(([start, awake]) => {
      if (!cancelled) { setAutostart(start); setKeepAwake(awake); setSettingsError(null); }
    }).catch((reason: unknown) => { if (!cancelled) setSettingsError(`读取设置失败：${String(reason)}`); });
    return () => { cancelled = true; };
  }, [api, refresh]);

  useEffect(() => {
    let cancelled = false;
    let receivedPairing = false;
    const cleanup: (() => void)[] = [];
    const subscribe = async () => {
      // Subscribe before loading a pending tray QR, so early tray clicks are not lost.
      const offPair = await api.onPairing((value) => {
        receivedPairing = true;
        if (!cancelled) { setPair(value); setActionError(null); }
      });
      if (cancelled) { offPair(); return; }
      cleanup.push(offPair);
      const offError = await api.onError((message) => { if (!cancelled) setActionError(message); });
      if (cancelled) { offError(); return; }
      cleanup.push(offError);
      const current = await api.currentPair();
      if (!cancelled && !receivedPairing && current) setPair(current);
    };
    void subscribe().catch((reason: unknown) => { if (!cancelled) setActionError(`读取托盘配对状态失败：${String(reason)}`); });
    return () => { cancelled = true; cleanup.forEach((off) => off()); };
  }, [api]);

  useEffect(() => {
    if (confirmDevice) cancelButton.current?.focus();
    else opener.current?.focus();
  }, [confirmDevice]);

  async function startPairing() {
    setPairing(true); setActionError(null);
    try { setPair(await api.pair()); }
    catch (reason) { setActionError(`配对失败：${String(reason)}`); }
    finally { setPairing(false); }
  }

  async function removeDevice() {
    if (!confirmDevice) return;
    setRemoving(true); setActionError(null);
    try {
      await api.revoke(confirmDevice.id);
      setDevices((current) => current?.filter((device) => device.id !== confirmDevice.id) ?? null);
      setConfirmDevice(null); setRefresh((value) => value + 1);
    } catch (reason) { setActionError(`移除失败：${String(reason)}`); }
    finally { setRemoving(false); }
  }

  async function changeSetting(kind: "autostart" | "awake", enabled: boolean) {
    setSaving(kind); setSettingsError(null);
    try {
      if (kind === "autostart") setAutostart(await api.setAutostart(enabled));
      else setKeepAwake(await api.setKeepAwake(enabled));
    } catch (reason) { setSettingsError(`保存设置失败：${String(reason)}`); }
    finally { setSaving(null); }
  }

  const connected = !!status?.tunnel_address;
  const blocked = !!confirmDevice;

  return (
    <main>
      <header className="top">
        <div className="brand">
          <h1>Tether</h1>
          <span className={`pill ${status ? (connected ? "ok" : "warn") : ""}`} role="status">
            <i aria-hidden="true" />
            {status ? (connected ? "隧道已连接" : "隧道尚未连接") : "正在读取状态…"}
          </span>
        </div>
        <button className="ghost" disabled={blocked || saving !== null} onClick={() => setRefresh((value) => value + 1)}>刷新</button>
      </header>

      {overviewError && <p role="alert" className="alert">{overviewError}</p>}

      <section aria-labelledby="status-title" className="card">
        <h2 id="status-title">状态</h2>
        {status ? <>
          <ul className="stats">
            <li aria-label={`已连接设备：${status.connected_devices}`}><b>{status.connected_devices}</b><span>已连接设备</span></li>
            <li aria-label={`工作中：${status.sessions_working}`} className={status.sessions_working ? "busy" : ""}><b>{status.sessions_working}</b><span>工作中</span></li>
            <li aria-label={`等待你处理：${status.sessions_need_you}`} className={status.sessions_need_you ? "attention" : ""}><b>{status.sessions_need_you}</b><span>等待你处理</span></li>
          </ul>
          <p className="meta">隧道：{status.tunnel_address ?? "尚未连接"}</p>
          <p className="meta">网关：{status.gateway}</p>
          {status.fake_host && <p className="badge">假 Host（开发数据）</p>}
        </> : <p className="muted">正在读取状态…</p>}
      </section>

      <section aria-labelledby="connect-title" className="card">
        <h2 id="connect-title">连接手机</h2>
        <p className="muted">在手机上打开 Tether，点“扫码配对”，扫描这里的二维码。</p>
        {actionError && !confirmDevice && <p role="alert" className="alert">{actionError}</p>}
        {pair && <Pairing pair={pair} />}
        <div className="actions">
          <button className="primary" disabled={pairing || blocked} onClick={() => void startPairing()}>{pairing ? "正在开启配对…" : "配对新手机"}</button>
          {pair && <button disabled={blocked} onClick={() => setPair(null)}>收起二维码</button>}
        </div>
      </section>

      <section aria-labelledby="devices-title" className="card">
        <h2 id="devices-title">设备</h2>
        {devices === null ? <p className="muted">正在读取设备…</p> : devices.length === 0 ? <p className="muted">还没有配对设备。</p> : <ul className="rows">
          {devices.map((device) => <li key={device.id}>
            <div className="grow">
              <strong>{device.name}</strong>
              <p className="meta">配对时间：{formatTime(device.paired_at)}</p>
              <p className="meta">最后在线：{formatTime(device.last_seen)}</p>
            </div>
            <button className="danger" disabled={blocked} aria-label={`移除 ${device.name}`} onClick={(event) => {
              opener.current = event.currentTarget; setActionError(null); setConfirmDevice(device);
            }}>移除</button>
          </li>)}
        </ul>}
      </section>

      {confirmDevice && <div className="backdrop"><section role="alertdialog" aria-modal="true" aria-labelledby="remove-title" aria-describedby="remove-description" onKeyDown={(event) => {
        if (event.key === "Escape" && !removing) { event.preventDefault(); setConfirmDevice(null); }
        if (event.key === "Tab") {
          event.preventDefault();
          if (document.activeElement === cancelButton.current) confirmButton.current?.focus();
          else cancelButton.current?.focus();
        }
      }}>
        <h2 id="remove-title">移除设备？</h2>
        <p id="remove-description">确认移除 {confirmDevice.name}？该设备需要重新配对才能连接。</p>
        {actionError && <p role="alert" className="alert">{actionError}</p>}
        <div className="actions end">
          <button ref={cancelButton} disabled={removing} onClick={() => setConfirmDevice(null)}>取消</button>
          <button ref={confirmButton} className="danger solid" disabled={removing} onClick={() => void removeDevice()}>{removing ? "正在移除…" : "确认移除"}</button>
        </div>
      </section></div>}

      <section aria-labelledby="agents-title" className="card">
        <h2 id="agents-title">Agent</h2>
        {agents === null ? <p className="muted">正在读取 agent…</p> : agents.length === 0 ? <p className="muted">未发现 agent。</p> : <ul className="rows">
          {agents.map((agent) => <li key={agent.id}>
            <div className="grow">
              <strong>{agent.name}</strong>
              {!agent.available && <p className="meta">{agent.detail || "未提供原因"}</p>}
              {locations.filter((l) => l.agent === agent.id).map((l) => <ProgramLocation key={l.agent} api={api} agent={agent} saved={l.program} onChanged={() => setRefresh((value) => value + 1)} />)}
            </div>
            <span className={`tag ${agent.available ? "ok" : "off"}`}>{agent.available ? "可用" : "不可用"}</span>
          </li>)}
        </ul>}
      </section>

      <section aria-labelledby="settings-title" className="card">
        <h2 id="settings-title">设置</h2>
        {settingsError && <p role="alert" className="alert">{settingsError}</p>}
        <label className="switch"><span>开机自启</span><input type="checkbox" checked={autostart ?? false} disabled={autostart === null || saving !== null || blocked} onChange={(event) => void changeSetting("autostart", event.currentTarget.checked)} /></label>
        <p className="hint">登录系统后自动在托盘启动，手机随时能连上。</p>
        <label className="switch"><span>防止电脑休眠</span><input type="checkbox" checked={keepAwake ?? false} disabled={keepAwake === null || saving !== null || blocked} onChange={(event) => void changeSetting("awake", event.currentTarget.checked)} /></label>
        <p className="hint">电脑休眠后手机连不上；打开后 Tether 运行期间不休眠。</p>
        {autostart === null && keepAwake === null && <p className="muted">正在读取设置；失败时可点击刷新重试。</p>}
        {saving && <p role="status" className="muted">正在保存设置…</p>}
      </section>

      <p className="foot">关闭窗口后，Tether 会继续在系统托盘运行。</p>
    </main>
  );
}

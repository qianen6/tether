# 模块接口契约（冻结）

T00 会把这里的签名写成可编译的骨架（函数体为 `todo!()` 或最小实现），之后各任务在骨架上实现。
**签名不得修改**；需要改动写到 `docs/requests/`。下面省略了 `use` 与派生宏，所有公开类型都实现 `Debug`，跨线程使用的都是 `Send + Sync`。

## 依赖关系

```
proto ◀── store
proto ◀── agent-api ◀── adapter-codex / adapter-claude / adapter-acp
terminals（只依赖 proto）
tunnel（不依赖其他内部 crate）
server ◀── proto, store, agent-api, terminals
host   ◀── 以上全部
apps/cli, apps/desktop ◀── host
packages/client ◀── proto 生成的 TS 类型
apps/mobile-debug ◀── packages/client, react-native-tailpocket
```

## crates/proto

按 `PROTOCOL.md` 第 7 节定义类型，另有：

已由 T00 完整实现（`crates/proto/src/lib.rs`），下面是摘要。

```rust
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME: usize = 8 << 20;
pub const MAX_TOOL_OUTPUT: usize = 8 << 10;
pub mod close { HANDSHAKE = 4400, UNAUTHORIZED = 4401, VERSION_MISMATCH = 4426 }

pub enum ClientMsg { Pair{..}, Hello{..}, ListAgents{req}, ... , Ping{n} }   // serde tag = "t"
impl ClientMsg { pub fn req(&self) -> Option<u64>; }
pub enum ServerMsg { Paired{..}, Welcome{..}, Rejected{reason}, Ok{req,data}, Error{req,code,message},
                     Event{..}, SessionUpdate{..}, SessionClosed{..}, TermExit{..}, Notify{..}, Pong{n} }
impl ServerMsg { pub fn ok<T: Serialize>(req, &T) -> ServerMsg; pub fn error(req, code, message) -> ServerMsg; }
pub enum RejectReason { InvalidSecret, InvalidToken, Revoked, VersionMismatch, Locked }
pub enum ErrorCode { BadRequest, Unauthorized, NotFound, AgentUnavailable, Busy, Internal }
pub enum NotifyKind { NeedsYou, Done, Failed }

// `ok` 的 data（服务端用 ServerMsg::ok 序列化，客户端按请求类型解析）
pub struct Empty {}                           // 无返回内容的请求
pub struct AgentsData   { agents: Vec<AgentInfo> }
pub struct SessionsData { sessions: Vec<SessionInfo> }
pub struct SessionData  { session: SessionInfo }   // create_session、subscribe
pub struct TermData     { term: TermInfo }         // term_open、term_attach
pub struct TermsData    { terms: Vec<TermInfo> }

pub fn truncate_tail(s: &str, max: usize) -> String;   // 保留末尾，不切断 UTF-8 字符

pub mod binary {
    pub const TERM_OUTPUT: u8 = 0x01;
    pub const TERM_INPUT: u8 = 0x02;
    pub fn encode(kind: u8, term: &str, data: &[u8]) -> Vec<u8>;
    pub fn decode(frame: &[u8]) -> anyhow::Result<(u8, &str, &[u8])>;
}
pub fn now_ms() -> u64;
```

## crates/store

同步 API，内部 `Mutex<rusqlite::Connection>`；调用方在异步代码里直接调用（操作都很快）。

```rust
pub struct Store;
impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Store>;          // 自动建表/迁移
    pub fn open_in_memory() -> anyhow::Result<Store>;

    // 设备
    pub fn issue_device(&self, name: &str) -> anyhow::Result<(Device, String /*明文令牌*/)>;
    pub fn device_by_token(&self, token: &str) -> anyhow::Result<Option<Device>>;   // 常量时间比较哈希
    pub fn set_phone_key(&self, device_id: &str, phone_key: &str) -> anyhow::Result<()>;
    pub fn touch_device(&self, device_id: &str) -> anyhow::Result<()>;              // 更新 last_seen
    pub fn list_devices(&self) -> anyhow::Result<Vec<Device>>;
    pub fn revoke_device(&self, device_id: &str) -> anyhow::Result<Option<Device>>; // 返回被删设备（含 phone_key）

    // 会话
    pub fn upsert_session(&self, s: &SessionRecord) -> anyhow::Result<()>;
    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionRecord>>;
    pub fn delete_session(&self, id: &str) -> anyhow::Result<()>;

    // 事件（只存非瞬时事件）
    pub fn append_event(&self, session: &str, e: &proto::Event) -> anyhow::Result<()>;
    pub fn events_after(&self, session: &str, after: u64, limit: usize) -> anyhow::Result<Vec<proto::Event>>;
}
pub struct Device { pub id: String, pub name: String, pub phone_key: Option<String>,
                    pub paired_at: u64, pub last_seen: Option<u64> }
pub struct SessionRecord { pub info: proto::SessionInfo, pub native_id: Option<String> }
```

## crates/agent-api

```rust
pub struct SpawnOptions {
    pub cwd: PathBuf,
    pub resume: Option<String>,            // 上次的 native_id
    pub program: Option<PathBuf>,          // 覆盖可执行文件（测试、自定义 agent 用）
    pub args: Vec<String>,                 // 额外参数（自定义 agent 用）
    pub env: Vec<(String, String)>,
    pub chat: bool,                        // 无项目的对话：只读，其余操作要审批
    pub model: Option<String>,             // None：agent 自己的默认
    pub effort: Option<String>,
    pub speed: Option<String>,             // SpeedInfo.id 或 "default"
    pub permission: Option<String>,        // PermissionInfo.id；None：按旧规则（chat 只读）
    pub project: Option<String>,           // agent 自己的项目 id（Codex projectId）
    pub roots: Vec<PathBuf>,               // 该项目的全部文件夹
    pub reveal: bool,                      // 第一条消息后调用 reveal；Codex 自动运行入口忽略，见 T11b
    pub fork: bool,                        // 和 resume 一起：复制成新对话，新 id 由 NativeId 报回
}

pub enum AgentCommand {
    Send { text: String, images: Vec<String> },  // images：电脑上的图片路径；只收文字的 agent 用 with_paths 附在文字后
    Answer { approval: String, decision: proto::Decision },
    Interrupt,
    SetModel { model: Option<String>, effort: Option<String>, speed: Option<String>,
               permission: Option<String> },  // 下一轮起；None 表示不改
    Shutdown,
}

pub enum AgentOutput {
    Event(proto::EventKind),               // 不含 seq，由 server 编号
    NativeId(String),                      // agent 自己的会话 id，用于恢复
    Exited { code: Option<i32>, error: Option<String> },
}

pub trait AgentAdapter: Send + Sync + 'static {
    fn info(&self) -> proto::AgentInfo;
    /// 必须立即返回（在 tokio 运行时内调用，内部自行 spawn 任务）。
    /// 返回的发送端关闭（被 drop）等同于 Shutdown。
    fn spawn(&self, opts: SpawnOptions, out: mpsc::UnboundedSender<AgentOutput>)
        -> anyhow::Result<mpsc::UnboundedSender<AgentCommand>>;

    // 以下都有默认实现（没有历史 → NoHistory 错误，模型列表为空，不建对话文件夹）。
    fn list_history(&self, query: HistoryQuery) -> BoxFuture<proto::HistoryData>;
    fn read_history(&self, read: HistoryRead) -> BoxFuture<proto::HistoryReadData>;
    fn list_models(&self, query: ModelQuery) -> BoxFuture<Vec<proto::ModelInfo>>;
    /// 可选的权限档位和 agent 自己的应用当前用的档位；默认为空（不提供选择）。
    fn list_permissions(&self, query: ModelQuery) -> BoxFuture<Permissions>;  // Permissions { list, default }
    /// chat 会话的新文件夹（以标题命名）；None 时 server 用默认目录。
    fn chat_dir(&self, hint: &str) -> anyhow::Result<Option<PathBuf>>;
    /// 在电脑上 agent 自己的应用里打开这个对话；默认报错（没有这种应用）。
    fn reveal(&self, thread: &str) -> anyhow::Result<()>;
    /// 一轮结束、空闲这么久后 server 停掉 agent 进程，让别的程序能打开这个对话；
    /// 下一条消息按 resume 重新 spawn。默认 None（一直开着）。Codex 为 60 秒：
    /// Codex 一个对话同时只允许一个 app-server 写（thread-writer-locks）。
    fn release_after(&self) -> Option<Duration>;
}

/// 测试用回显 agent（T00 已实现）。普通消息：state(working) → assistant_delta×3 → assistant_text
/// "echo: <text>" → turn_done → state(idle)。
/// "approve:<cmd>"：先发 approval 并等待 Answer（deny → "已拒绝"，abort → 停止本轮）。
/// "/exit"：进程正常退出（Exited code 0）；"/crash"：异常退出（code 1）。用于测试重新 spawn。
pub struct EchoAdapter;

pub mod testkit {   // T00 已实现
    /// 驱动一个适配器：依次发送命令，收集输出直到满足 until 或超时。
    pub async fn run(adapter: &dyn AgentAdapter, opts: SpawnOptions, script: Vec<AgentCommand>,
                     until: impl Fn(&[AgentOutput]) -> bool, timeout: Duration) -> anyhow::Result<Vec<AgentOutput>>;
    /// 逐步驱动：先等到某个输出（例如拿到 approval id），再发下一条命令。
    pub struct Driver { pub cmd, pub rx, pub outputs: Vec<AgentOutput> }
    impl Driver { spawn, send, wait_until, wait_for, events }
    /// 按行读写的内存双工流，用于在不启动进程的情况下测试协议逻辑。
    pub fn line_duplex() -> (LineEnd, LineEnd);
    impl LineEnd { into_parts, send_line, send_json, next_line, next_json, send_fixture }
}
```

## 各适配器 crate

```rust
// adapter-codex（T04）
pub const ID: &str = "codex"; pub const COMMAND: &str = "codex";
pub struct CodexAdapter { /* private: via_desktop: fn() -> bool */ }
pub fn adapter() -> Arc<dyn AgentAdapter>; // 和 CodexAdapter::default() 一样，始终直连
pub fn adapter_with(via_desktop: fn() -> bool) -> Arc<dyn AgentAdapter>;
// adapter-claude（T05）：界面名称用 "Claude Agent"（Anthropic 品牌规定不能叫 "Claude Code"）
pub const ID: &str = "claude"; pub const COMMAND: &str = "claude";
pub struct ClaudeAdapter; pub fn adapter() -> Arc<dyn AgentAdapter>;
// adapter-acp（T06）
pub struct AcpAgent { pub id: String, pub name: String, pub command: Vec<String> }
pub struct AcpAdapter; pub fn adapter(agent: AcpAgent) -> Arc<dyn AgentAdapter>;
```

`info()` 已在骨架中实现（用 `which` 检测命令是否存在），各任务只需实现 `spawn`。

**Codex 桌面跟随（T11b）**

- host 的 `codex` 与 `codex-desktop` 都用 `adapter_with(via_desktop)`，回调在每次启动会话时求值：`std::env::var_os("TETHER_CODEX_DIRECT").is_none()`。因此 `TETHER_CODEX_DIRECT=1`（以及该变量存在的其他值）强制直连。`adapter()` / `Default` 仍为纯直连，真实模型测试不依赖桌面版。
- 回调允许时，每次启动会话先连接桌面 IPC：连不上即以「桌面版没有打开」错误结束，不走直连。`resume` 存在且不是 `fork` 时再查 owner：有 owner 则跟随；`no-client-found` 走直连；其他错误回答为「桌面版已更新」（`bus::Updated`）；3 秒内无回答为「桌面版没有回应」。新对话和分叉连上后即走直连。
- IPC 的帧和连接复用 `bus`，快照和 patch 翻译复用 `mirror`；`follow::owned(reader, writer, thread)` 和 `follow::run(link, thread, opts, commands, out)` 可在 `tokio::io::duplex` 上运行。初始化与 owner 查询各有 3 秒超时，首个快照 15 秒，discovery 即时 decline；revision 失配时重新请求快照，并暂停队列发送直到恢复。
- 跟随时，Send 先报 Working，再 FIFO 排队；仅在没有正在启动或进行中的回合时发送 `thread-follower-start-turn`，不 steer。中断清空队列，结束本地待审批项，并在获知 turn id 后发中断。模型、effort、speed 作为后续 start-turn 的 `model`、`effort`、`serviceTier` 传入；桌面版是否采用这些设置仍待实测。
- 普通 Shutdown、命令通道关闭、输出通道关闭时发送 `following:false` 并正常结束。管道关闭或 owner disconnected 时报告 Exited.error：「电脑上的 Codex 桌面版关闭了。请在电脑上重新打开 Codex，再发送。」状态版本号不是 11 时报告「桌面版已更新……分叉成新对话……」。不在失败的跟随会话里重试直连，避免重复执行；下一条消息由 server 重新 spawn 并选路。
- `release_after()` 仍为 60 秒，`reveal()` 显式操作不变；自动运行入口忽略旧手机传来的 `opts.reveal`，绝不为跟随、直连、新建或分叉打开深链接、切换桌面窗口。
- 手机和协议类型不变。`UserText` 除 server 记录的手机输入外，也可来自桌面版输入；手机消息的回声不重复上报。首次快照不重放历史，只汇报正在进行的回合与审批。
- 未确认：桌面版对模型设置的实际采用情况、真实审批 `requests` 形状、macOS socket 路径。审批自动测试使用模拟请求而非真实审批录制；Windows 真实回合录制用于离线回放。

**适配器行为约定（所有适配器必须遵守）**

1. 收到 `Send` 时立即输出 `state(working)`；**不要**重复输出该消息的 `user_text`（由 server 负责）。Codex 桌面跟随可为在电脑上直接输入的新消息输出 `user_text`。
2. 发出 `approval` 后输出 `state(needs_you)`；收到对应 `Answer` 后输出 `approval_done`，再回到 `state(working)`。
3. 一轮结束输出 `turn_done`，然后 `state(idle)`。
4. 进程退出输出 `Exited`。之后 server 若再收到消息，会用 `resume = native_id` 重新 `spawn`。
5. agent 不支持的 `Decision` 不放进 `choices`。映射规则见各适配器任务。
6. `tool_end.output` 截断到 8 KiB（保留末尾）。

## crates/terminals

```rust
pub struct TermManager;
impl TermManager {
    pub fn new(scrollback_bytes: usize) -> TermManager;                 // 默认 512 KiB
    pub fn open(&self, command: Vec<String>, cwd: PathBuf, cols: u16, rows: u16) -> anyhow::Result<proto::TermInfo>;
    pub fn attach(&self, term: &str) -> anyhow::Result<(Vec<u8> /*滚动缓冲区*/, broadcast::Receiver<TermChunk>)>;
    pub fn input(&self, term: &str, data: &[u8]) -> anyhow::Result<()>;
    pub fn resize(&self, term: &str, cols: u16, rows: u16) -> anyhow::Result<()>;
    pub fn kill(&self, term: &str) -> anyhow::Result<()>;
    pub fn list(&self) -> Vec<proto::TermInfo>;
}
pub enum TermChunk {
    Data(bytes::Bytes),
    Attention,                 // 响铃字符，或有输出后静默超过 attention_after（默认 8 秒）
    Exit(Option<i32>),
}
```

## crates/tunnel

```rust
pub struct TunnelConfig {
    pub helper: PathBuf,                   // tailpocket-host 可执行文件
    pub forward: SocketAddr,               // 网关地址 127.0.0.1:<port>
    pub state_path: PathBuf,               // 节点身份与已配对手机（0600）
    pub name: String,                      // 手机上显示的电脑名
    pub derp_map_url: Option<String>,      // 自建 DERP 时使用
    pub qr_prefix: String,                 // "tether1:"
}
pub struct Tunnel;
impl Tunnel {
    pub async fn start(cfg: TunnelConfig) -> anyhow::Result<Tunnel>;
    /// 开配对窗口；payload 原样在隧道内交给手机。
    pub async fn pair(&self, ttl: Duration, payload: String) -> anyhow::Result<PairWindow>;
    pub async fn revoke(&self, phone_key: &str) -> anyhow::Result<()>;
    pub async fn set_allowed(&self, phone_keys: Vec<String>) -> anyhow::Result<()>;
    pub fn address(&self) -> Option<String>;
    pub async fn close(self) -> anyhow::Result<()>;
}
pub struct PairWindow {
    pub qr: String,
    pub expires_at: u64,
    pub joined: oneshot::Receiver<PhoneJoined>,   // 窗口过期则发送端被 drop
}
pub struct PhoneJoined { pub phone_key: String, pub device_name: String }
```

## crates/server

```rust
pub struct ServerConfig { pub bind: SocketAddr /*127.0.0.1:0*/, pub host_name: String }
pub struct Server;
impl Server {
    pub async fn start(cfg: ServerConfig, store: Arc<Store>, terms: Arc<TermManager>,
                       agents: Vec<Arc<dyn AgentAdapter>>) -> anyhow::Result<Server>;
    pub fn local_addr(&self) -> SocketAddr;
    /// 生成配对密钥（一次性，ttl 后失效）。手机用它 pair 成功后回调 on_paired(device)。
    pub fn new_pair_secret(&self, ttl: Duration) -> String;
    pub fn on_paired(&self) -> broadcast::Receiver<store::Device>;
    /// 吊销：删除设备并立即断开其所有连接；返回设备（调用方再去隧道 revoke）。
    pub async fn revoke(&self, device_id: &str) -> anyhow::Result<Option<store::Device>>;
    pub fn connected_devices(&self) -> Vec<String>;
    pub async fn shutdown(self);
}
```

`server` 内部包含：会话管理（状态机、`seq`、持久化、补发、重启后懒恢复）、网关（PROTOCOL.md 全部消息）、提醒规则（`notify`）。

## crates/host

```rust
pub struct HostConfig {
    pub data_dir: PathBuf,                 // 默认按系统：Windows %APPDATA%\Tether，macOS ~/Library/Application Support/Tether，Linux ~/.local/share/tether
    pub name: String,                      // 默认主机名
    pub tailpocket_helper: Option<PathBuf>,// 默认在可执行文件旁或 vendor/ 下查找
    pub derp_map_url: Option<String>,
    pub custom_agents: Vec<CustomAgent>,   // 也会读取 data_dir/agents.toml
}
pub struct CustomAgent { pub id: String, pub name: String, pub command: Vec<String>, pub mode: CustomMode }
pub enum CustomMode { Acp, Terminal }

pub fn default_data_dir() -> PathBuf;

pub struct Host;
impl Host {
    pub async fn start(cfg: HostConfig) -> anyhow::Result<Host>;
    pub async fn pair(&self, ttl: Duration) -> anyhow::Result<PairInfo>;   // 组合 server 密钥 + tunnel 窗口
    pub fn devices(&self) -> anyhow::Result<Vec<store::Device>>;
    pub async fn revoke(&self, device_id: &str) -> anyhow::Result<()>;
    pub fn agents(&self) -> Vec<proto::AgentInfo>;
    pub fn status(&self) -> HostStatus;
    pub async fn shutdown(self) -> anyhow::Result<()>;
}
pub struct PairInfo { pub qr: String, pub expires_at: u64 }
pub struct HostStatus { pub tunnel_address: Option<String>, pub gateway: SocketAddr,
                        pub connected_devices: usize, pub sessions_working: usize, pub sessions_need_you: usize }
```

T10 实现说明（不改上面的签名）：

- 数据目录内容：`config.toml`（`name`、`derp_map_url`）、`agents.toml`（`[[agent]] id / name / command / mode = "acp" | "terminal" | "dsh"`，首次启动生成带注释的模板）、`tether.db`、`tunnel-state.json`、`tether.log`（CLI）、`control.json`（运行中的 `tether serve` 的本地控制端口与令牌）。
- agent 列表：内置 codex、claude；ACP 预设 gemini、opencode、kimi、qwen、cursor、copilot（命令未经验证）；`shell`（仅终端）；`agents.toml` 中同 id 的条目覆盖内置项。环境变量 `TETHER_ENABLE_ECHO=1` 额外提供 echo（端到端测试用）。
- 手机加入隧道（`PhoneJoined`）与网关配对（`on_paired`）按先进先出关联，再 `set_phone_key`；超时未配对的隧道手机会被移出隧道。
- 找不到 `tailpocket-host` 时 `Host::start` 返回错误。

## packages/client（TypeScript）

```ts
export interface Transport {                       // 默认用全局 WebSocket；测试可注入
  connect(url: string): WebSocketLike;
}
export interface ClientOptions {
  url: string;                                     // ws://127.0.0.1:<p>/v1/ws（来自 react-native-tailpocket）
  deviceName: string;
  token?: string;                                  // 已配对时
  pairSecret?: string;                             // 首次配对时
  storage: { get(k: string): Promise<string|null>; set(k: string, v: string): Promise<void> };
  transport?: Transport;
}
export class TetherClient {
  constructor(opts: ClientOptions);
  connect(): Promise<Welcome>;                     // 首次配对时自动保存 token
  close(): void;
  readonly state: "connecting" | "open" | "reconnecting" | "closed";
  on(ev: "state" | "session_update" | "session_closed" | "notify", fn: (x: any) => void): () => void;

  listAgents(): Promise<AgentInfo[]>;
  listSessions(): Promise<SessionInfo[]>;
  createSession(agent: string, opts?: { cwd?: string; title?: string; resume?: string;
    model?: string | null; effort?: string | null; chat?: boolean; permission?: string | null }): Promise<SessionInfo>;
  listHistory(agent: string, opts?: { search?: string; cursor?: string }): Promise<HistoryData>;
  readHistory(agent: string, thread: string): Promise<HistoryReadData>;
  listModels(agent: string): Promise<ModelInfo[]>;
  listModelChoices(agent: string): Promise<ModelsData>;  // 模型 + 权限档位
  setModel(session: string, model?: string | null, effort?: string | null, speed?: string | null,
    permission?: string | null): Promise<void>;
  /** 订阅：自动补发、断线重连后自动续订；返回取消函数。 */
  subscribe(session: string, onEvent: (e: Event) => void, after?: number): Promise<() => void>;
  send(session: string, text: string): Promise<void>;
  answer(session: string, approval: string, decision: Decision): Promise<void>;
  interrupt(session: string): Promise<void>;
  closeSession(session: string): Promise<void>;

  openTerminal(o: { command?: string[]; agent?: string; cwd?: string; cols: number; rows: number }): Promise<TerminalHandle>;
  attachTerminal(term: string): Promise<TerminalHandle>;
  listTerminals(): Promise<TermInfo[]>;
}
export interface TerminalHandle {
  info: TermInfo;
  onData(fn: (bytes: Uint8Array) => void): () => void;
  onExit(fn: (code: number | null) => void): () => void;
  write(data: string | Uint8Array): void;
  resize(cols: number, rows: number): Promise<void>;
  detach(): Promise<void>;
  kill(): Promise<void>;
}
```

# T11b：桌面版已打开的 Codex 对话交给桌面版运行（接手说明）

工作分支是 `task/T11`，在现有未提交的改动上继续做。

本任务的修改范围比 AGENTS.md 规定的宽，可以改下面"剩余工作"里提到的所有文件，包括：
- `crates/agent-api`、`crates/host`、`crates/server`
- `docs/PROTOCOL.md`、`docs/CONTRACTS.md`
- 手机端

## 为什么要做

Codex 的一个对话，同一时间只能由一个 app-server 写入：每个对话有一把系统文件锁，在 `~/.codex/thread-writer-locks/<id>.lock`。

桌面版一旦加载了某个对话，要到桌面版退出才会放开这把锁。这时 Tether 再自己起 app-server 去 `thread/resume`，就会报：

```
thread X already has an active writer
```

## 方案（用户已确认，与 Remodex 的做法相同）

会话启动时（`AgentAdapter::spawn`）自动决定走哪条路，**不提供任何开关，用户无需选择**。

### 1. 对话已被电脑上的 Codex 桌面版打开

条件：桌面版有这个对话的 owner。

做法：
- 经桌面版内部的 IPC 总线跟随这个对话。
- 手机发出的消息、中断、审批，都通过总线交给桌面版执行。
- 桌面版上的全部进展实时同步到手机，包括在电脑上直接打的字。
- 不会冲突，也不会让桌面版窗口跳转。

### 2. 其他情况：沿用现有的直连模式

适用于以下任一情况：
- 桌面版没开；
- 对话没被桌面版加载；
- 新对话；
- 分叉。

做法：Tether 自己在后台起 app-server 来跑，桌面版完全不动。
- **绝不**打开 `codex://threads/...` 去让桌面版接管，**不许让桌面版窗口跳转**。
- 已有的 60 秒空闲释放（`release_after`）保持不变。这样手机停用后，用户可以在桌面版里点开同一个对话。

### 3. 跟随过程中桌面版退出了

条件：管道关闭，或收到 owner 的 `client-status-changed`，其 `status` 为 `"disconnected"`。

做法：会话以错误结束，错误文字为：

> 电脑上的 Codex 桌面版关闭了。再发一条消息即可由 Tether 在后台继续。

用户下一条消息到来时，服务器会重开会话，这时按第 2 条走直连。

### 4. 环境变量 `TETHER_CODEX_DIRECT=1`

设置后永远走直连，用于没有桌面版的机器和排错。

## 已完成的部分（未提交）

`cargo test -p tether-adapter-codex --lib -- mirror bus` 已通过。

### `crates/adapter-codex/src/bus.rs`

- 分帧：u32 小端长度，后跟 UTF-8 JSON。
- `frames()`：用一个独立任务读帧。
- `request`、`broadcast`、`decline`：构造对应的消息。
- `connect()`：
  - Windows：连 `\\.\pipe\codex-ipc`；
  - unix：依次尝试几个 socket 路径（**未验证**）。
  - 连不上时返回 `Closed`。
- `version` 常量。
- `CLOSED_TEXT` 现在用不上了，可以删掉，也可以改成上面第 3 条的文字。

### `mirror.rs` 与 `mirror_tests.rs`

- 把快照和 immer patch 应用到本地状态，再把状态变化翻译成 `EventKind`。
- 第一个快照只用来建立场景，不重放历史；只有进行中的回合要报。
- 手机自己发的用户消息（经 `sent()` 登记）不会重复显示；在电脑上打的字显示为 `UserText`。
- 审批：
  - `state.requests` 里出现新请求 → 发 `Approval` + `NeedsYou`；
  - 在电脑上回答掉的 → 发 `ApprovalDone`，允许还是拒绝由 item 状态推断。
- patch 对不上时返回 `Applied::Stale`。
- 对外方法：`busy()`、`active_turn()`、`answer()`、`waiting()`、`set_working()`。

### 其他文件

- `adapter.rs`：把构造 thread 参数的代码抽成了 `thread_request(opts)`，直连模式照常使用。
- `desktop.rs`：`codex_home()` 改为 `pub(crate)`，供 `bus.rs` 使用。
- `lib.rs`：已加 `mod bus; mod mirror;`。
- `fixtures/codex/desktop-turn.jsonl`：一次真实回合的录制，已脱敏。内容依次是：一个快照、一串 patches、中途的两个快照。

## 总线协议（Windows 上实测）

### 初始化

1. 发 `initialize` 请求：
   - `version` 为 0
   - `params` 为 `{clientType:"tether"}`
   - `sourceClientId` 为 `"initializing-client"`
2. 回复格式：
   ```
   {type:"response", requestId, resultType:"success", result:{clientId}, handledByClientId}
   ```
3. 之后所有消息的 `sourceClientId` 都填这个 `clientId`。
4. 收到 `{type:"client-discovery-request"}` 时回 `bus::decline(frame)`，表示我们不处理任何请求。

### 查对话的 owner

- 请求：`thread-owner-discovery`，`{hostId:"local", conversationId}`。这是桌面版自己用的方法，版本号用 `bus::version` 里同名的值；若不确定，可改用下面那个已实测的方法。
- 或者：`thread-follower-read-model-settings`，`v1 {conversationId}`（已实测）。
- 结果判断：
  - success：`handledByClientId` 就是 owner。
  - error `"no-client-found"`：没有 owner。

### 跟随

- 广播 `thread-stream-following-changed`，`v1 {conversationId, hostId:"local", following:true}`。
- owner 会先回一个快照，之后持续发 patches。两者都是 `thread-stream-state-changed` v11，`params` 为 `{conversationId, hostId, change}`，其中 `change` 有两种：
  - `{type:"snapshot", revision, conversationState}`
  - `{type:"patches", baseRevision, revision, patches:[{op,path,value}]}`
- 已在跟随时再发一次 `following:true`，owner 会重发快照（已从源码确认）。`Applied::Stale` 就靠这个恢复。
- 离开时发 `following:false`。
- 桌面版的其他窗口也会触发中途快照，收到时按 `first=false` 处理。

### 发消息

请求 `thread-follower-start-turn`，`v2`：

```
{conversationId,
 turnStart:{request:{threadId,
                     input:[{type:"text", text, text_elements:[]}],
                     model?, effort?, serviceTier?},
            context:{}}}
```

回复中 `result.result.turn.id` 就是 turn id。

### 中断

请求 `thread-follower-interrupt-turn`，`v4`：

```
{conversationId, mode:"user-stop", expectedTurnId}
```

### 审批

- 方法按 `Asked.method` 选：
  - 等于 `mirror::FILE_APPROVAL` → `thread-follower-file-approval-decision`；
  - 其他 → `thread-follower-command-approval-decision`。
- `v1`，参数：
  ```
  {conversationId, requestId: Asked.id 原样, decision}
  ```
- `decision` 用 `translate::decision()` 的结果（accept / acceptForSession / decline / cancel）。

### 断线

- 有客户端断开时，router 对它还没回复的请求回 `resultType:"error", error:"client-disconnected"`。
- 同时广播 `client-status-changed {clientId, clientType, status}`，`status` 为 `"connected"` 或 `"disconnected"`。

## 剩余工作

### 1. `crates/adapter-codex/src/follow.rs`：跟随会话循环

必须能在 duplex 上测试，接口如下：

```rust
/// Connected and the app owns `thread`: ready to follow. None: not running, or not its thread.
pub(crate) async fn owned<R, W>(reader: R, writer: W, thread: &str) -> anyhow::Result<Option<Link<W>>>;
pub(crate) async fn run<W>(link: Link<W>, thread: &str, opts: &SpawnOptions,
    commands: UnboundedReceiver<AgentCommand>, out: &UnboundedSender<AgentOutput>) -> anyhow::Result<()>;
```

#### Link

- 字段：帧接收端、writer、clientId，以及 `held: VecDeque<Value>`。
- `call()` 用于初始化阶段：
  - 等回复，超时 15s；
  - 等待期间收到的其他帧存进 `held`；
  - 收到 discovery 当场 decline。
- 主循环里不阻塞等回复，改用 `pending: HashMap<requestId, Pending>` 记账，`Pending` 有三种：`Start(text)`、`Interrupt`、`Answer`。
- requestId 用 `uuid::Uuid::new_v4()`。

#### 开始阶段

1. 发 `following:true`。
2. 等第一个快照，最多 15s；按 `first=true` 交给 mirror。
3. 收到第一个快照之前的 patches 一律忽略。

#### 主循环（`select!`）

**命令：**
- **Send**
  1. 先发 Working，并调 `mirror.set_working(true)`。
  2. 把消息放进 queue。
  3. 调 `drain()`：当 `starting` 为空且 `!mirror.busy()` 时，取出队首，先调 `mirror.sent(text)`，再发 start-turn。**只排队，不 steer。**
- **Interrupt**
  1. 清空 queue。
  2. 对仍在等待的审批逐个发 `ApprovalDone{Abort}`，并调 `mirror.answer`。
  3. 有 `active_turn` 就发中断请求；还没有 turn id 的，先记一个标志，等 turn id 出现再发。
- **Answer**
  - `mirror.answer()` 找到了：发审批请求，发 `ApprovalDone`，再按情况发 Working 或 NeedsYou。
  - 找不到：发 Warn notice "审批已结束或不存在"。
- **SetModel**：保存设置，发和直连模式一样的 notice（见 `adapter.rs` 的 SetModel 分支）。
- **Shutdown**、命令通道关闭、`out.closed()`：发 `following:false`，返回 `Ok`。

**帧：**
- response 为 Start 失败：
  1. 用 `translate::error_text` 生成 Error notice；
  2. 如果 `!busy`，再发 `TurnDone` + Idle。
- state-changed：只处理自己的 `conversationId`。
  - 快照：`first=false`；
  - Stale：重发 `following:true`；
  - 每处理完一帧都调一次 `drain()`。
- 管道关闭，或 owner 的状态变成 `disconnected`：返回 `Err`，文字用上面第 3 条。

#### 测试

用 duplex 写一个假桌面：
- initialize 时回 clientId；
- owner 查询按测试需要回成功或 `no-client-found`；
- 把 `desktop-turn.jsonl` 回放成广播。

至少覆盖以下情况：
- 有 owner：发一条消息，事件序列与 mirror 测试一致；
- 没有 owner：`owned()` 返回 None；
- 管道关闭时报错；
- 审批经总线回答；
- 回合进行中再 Send，消息排队；
- 中断。

### 2. 接入 adapter 和 lib

- `CodexAdapter` 加一个字段 `via_desktop: fn() -> bool`：
  - `Default` 与 `adapter()` 保持纯直连，现有测试和 `tests/live.rs` 不受影响；
  - 新增 `pub fn adapter_with(via_desktop: fn() -> bool) -> Arc<dyn AgentAdapter>`。
- `adapter::process()`，满足 `via_desktop()`、`resume` 有值、且不是 fork 时：
  1. 调 `bus::connect()`；
  2. 再调 `follow::owned()`；
  3. 得到 `Some` 就走 `follow::run`，结束时返回 `(None, None)`，错误原样用 `to_string()`；
  4. 其他情况（连不上、None、出错）记一条 `tracing::debug`，然后照常走直连。
- `release_after()`、`reveal()` 保持不变。
- `lib.rs` 加 `mod follow;`。

### 3. host

- 涉及两处：
  - `crates/host/src/agents.rs` 里 `codex` 的 `Named`；
  - `crates/host/src/codex_desktop.rs` 的 inner。
- 两处都改用 `tether_adapter_codex::adapter_with(via_desktop)`，其中：
  ```rust
  fn via_desktop() -> bool { std::env::var_os("TETHER_CODEX_DIRECT").is_none() }
  ```

### 4. 文档

- `docs/PROTOCOL.md` / `docs/CONTRACTS.md`：
  - 写明 Codex 会话的两条路和怎么选；
  - 写明 `UserText` 也可能来自在电脑上直接打的字。
- 手机端不需要改。

## 尚未确认的点（请在报告里写清）

- start-turn 里的 model、effort、serviceTier 桌面版是否会采用。实测时看 `latestThreadSettings` 的 patch。
- 审批在 `requests` 里的实际形状：按代码推断与 app-server 一致，但没录到过。
- macOS 的 socket 路径。

## 验收

```bash
. scripts/env.sh
cargo fmt --all
cargo clippy -p tether-adapter-codex -p tether-host -- -D warnings
cargo test --workspace
pnpm -r test
```

全部通过后提交，信息为：`T11: 桌面版已打开的 Codex 对话经桌面版运行`。

### 实测

需要用户同意，并且电脑上的 Codex 桌面版已打开。

1. 在桌面版已打开的对话里，从手机发一条消息：
   - 回复同步到手机；
   - 桌面版窗口不跳转。
2. 手机上新建一个对话：
   - 走直连；
   - 桌面版不跳转。

### 禁止事项

- 不截屏。
- 不杀用户的 Codex 进程。
- 不改 `.codex-global-state.json`。
- 不自行重启 Tether，先问用户。

> 后续调整（用户确认）：桌面版没开时不再走直连，直接提示用户先打开 Codex；owner 查询超时 3 秒；桌面版回答异常时提示“已更新…分叉成新对话”。以 PROTOCOL.md §4.1 为准。

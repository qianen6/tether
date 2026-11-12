# T03 服务：会话管理 + WebSocket 网关 + 提醒

**阶段 1。归属路径：`crates/server`。** 这是最大的任务。

## 目标

实现 `CONTRACTS.md` 中 `crates/server` 的 API，以及 `PROTOCOL.md` 的**全部**消息。用 `agent-api` 的 `EchoAdapter` 测试，不依赖任何真实 agent。

## 依赖的骨架

`tether-store`、`tether-terminals` 可能还是 `todo!()`（T02、T07 并行开发中）。做法：

- 在 `server` 内部定义两个**私有** trait：`StoreBackend`（会话、事件、设备所需的方法）和 `TermBackend`（终端所需的方法），并分别为 `Store`、`TermManager` 实现。
- 公开的 `Server::start`（契约签名不变）把具体类型包装成 trait 对象，再调用内部的 `pub(crate) fn start_with(...)`。
- 测试通过 `start_with` 注入内存假实现，所以不需要等 T02、T07 合并。
- 另外写一组用真实 `Store::open_in_memory()` 和 `TermManager` 的测试，标记 `#[ignore = "需要 T02/T07 合并"]`，由 T10 去掉标记。

## 模块

1. **`sessions`**：每个会话一个任务（actor）。
   - 状态机：`idle` / `working` / `needs_you` / `error` / `exited`，根据适配器输出的 `state` 事件更新。
   - 给事件编号 `seq`（从 1 递增），非瞬时事件写入存储，然后广播。
   - 收到 `send_message`：先生成 `user_text` 事件，再发给适配器；适配器进程不存在时，用 `resume = native_id` 重新 `spawn`。
   - 电脑重启后：从存储加载会话列表，状态统一设为 `idle`（之前是 `working` 的加一条 `notice`："电脑重启，任务已中断"），**收到第一条消息时才启动适配器**（懒恢复）。
   - 状态行 `status_line`：取最近一次 `tool_start.summary` 或 `approval.title`。
2. **`gateway`**：axum，路由 `/v1/ws`，只监听 `127.0.0.1`。
   - 握手：第一帧必须是 `pair` 或 `hello`，5 秒超时；认证失败按 PROTOCOL.md 关闭。
   - 配对：`new_pair_secret` 生成的密钥一次性、有过期时间；连续失败 5 次锁 60 秒（参考 `.ref/boop/src/pairing.ts`）；成功后签发设备并通过 `on_paired` 广播。
   - 每个连接：一个读循环 + 一个写队列；订阅表；终端附着表。
   - 订阅补发：先从存储读 `seq > after` 的历史，再接实时广播，**保证不丢不重**（先订阅广播、再读历史、按 seq 去重）。
   - 广播接收落后（lagged）时：对该连接的已订阅会话，从存储按最后发送的 seq 补发。
   - 心跳：60 秒没有任何帧就关闭。
   - `revoke`：删除设备，关闭该设备的所有连接（close 4401）。
3. **`notify`**：提醒规则。
   - 状态变为 `needs_you` → `notify(needs_you)`，所有连接都发。
   - 状态变为 `error` 或出现 `notice(error)` → `notify(failed)`。
   - `turn_done` 时，如果**没有任何连接订阅着该会话** → `notify(done)`。
   - 终端 `TermChunk::Attention` → 对该终端发 `notify(needs_you)`，同一终端 60 秒内最多一次。

## 测试（全部用真实 WebSocket 客户端 `tokio-tungstenite` 连 `Server`）

- 配对：正确密钥成功拿到令牌；密钥只能用一次；过期；连续错 5 次被锁。
- 认证：错误令牌被拒；吊销后已建立的连接立即被关闭（4401）。
- 会话：新建 Echo 会话 → 发消息 → 收到 `user_text`、`state`、`assistant_delta`、`assistant_text`、`turn_done`。
- 审批：发 `approve:xx` → 收到 `approval` 和 `notify(needs_you)` → `answer` → 回合结束。
- 补发：收到一半时断开，用 `after = 最后的 seq` 重新订阅，最终事件序列完整无重复。
- 重启：关闭 Server，用同一个存储重新启动，会话仍在，发消息能继续。
- 心跳超时、非法帧、超大帧、未认证就发请求。
- 终端：`term_open` → 二进制输入输出往返 → `term_exit`（用假终端实现）。

## 验收

```sh
cargo test -p tether-server --offline
cargo clippy -p tether-server -- -D warnings
```

# tether-server（T03）

公开 API 保持 `docs/CONTRACTS.md` 不变；网关只接受 `127.0.0.1`，路由为 `/v1/ws`。

## 实现分工

- `src/lib.rs`：Server 生命周期、启动配置、设备吊销与关机清理。
- `src/backends.rs`：crate 内部的 `StoreBackend` / `TermBackend`，真实类型仅作薄委托。
- `src/pairing.rs`：32 字节随机 base64url 密钥、一次消费、15 分钟 TTL 上限、连续失败 5 次后锁 60 秒。
- `src/sessions.rs`：每会话一个 actor，串行处理命令和输出；先持久化非瞬时事件再广播；所有事件共用递增 seq（瞬时 delta 对应的 seq 空洞正常）；保存 native_id，退出后以及电脑重启后懒启动/恢复。
- `src/gateway.rs`：握手、JSON 请求、二进制终端帧、写队列、心跳、订阅和附着。
- `src/notify.rs`：会话提醒，以及独立于附着连接的终端 Attention 监听与每终端 60 秒冷却。

## 协议覆盖

| 手机消息 | 实现 |
|---|---|
| `pair`、`hello` | 5 秒首帧限制；版本检查；paired 后立即 welcome；设备认证及 touch |
| `list_agents`、`list_sessions`、`create_session` | 类型化 ok；可用性、chat 能力、cwd 校验；session_update 广播 |
| `subscribe`、`unsubscribe` | 先订阅广播，再分页读历史；ok 在补发之前；以最后发送 seq 去重；lagged 时从存储补齐 |
| `send_message` | 先写 user_text；busy 检查；按 native_id 懒恢复；事件封装后的 8 MiB 上限检查 |
| `answer`、`interrupt` | 校验待审批 id / choices；审批答案防重复；空闲 interrupt 幂等 |
| `close_session` | actor 自己删除存储和注册项，再广播 session_closed；请求连接被取消也完成清理 |
| `term_open`、`term_attach`、`term_detach` | command / agent 二选一；自动附着；原子滚动缓冲快照先于实时字节 |
| `term_resize`、`term_kill`、`list_terms` | 尺寸与存在性检查；终端退出只发给附着者 |
| `ping` | 只回复 pong，不回复 ok；60 秒无任意帧则断开 |
| 二进制 `0x02` | 只允许向已附着终端写原始字节；`0x01` 输出按 8 MiB 拆帧 |

电脑推送覆盖 `event`、`session_update`、`session_closed`、`term_exit`、`notify`。畸形请求、错误方向二进制帧和超大应用消息返回 bad_request；配对/令牌/版本错误使用约定 rejected 和关闭码。

## 一致性与边界

- actor 维护序号及元数据；store 的 append/upsert 是两个冻结调用，启动时会向后扫描持久化尾部以修复两次调用之间退出造成的 last_seq 落后。
- `assistant_delta` 永不进入历史；重连以持久化 `assistant_text` 得到最终完整文本。不能把瞬时 delta 的 seq 空洞当成丢失历史。
- 会话事件有独立广播通道，通知/状态有全局广播通道。全局通道落后会重新发送会话快照并补发已知的 session_closed；会话通道落后会按 seq 读历史。
- 订阅计数由连接持有的 RAII guard 管理；done 只在全局订阅数为零时推送。
- 吊销与令牌查找/连接注册共用短同步锁，避免认证竞态；关闭信号独立于拥堵的输出队列。
- server 只留设备令牌 SHA-256 摘要。当前进程观察过的已撤销令牌返回 revoked；重启后及本进程未观察过的已删除令牌返回 invalid_token，均拒绝认证并关闭 4401。持久化区别的接口建议见 `docs/requests/T03.md`。
- axum/tungstenite 聚合后，应用层检查并丢弃超过 8 MiB 的消息，以保留连接处理下一帧；这不等同于传输层流式限内存解码。
- 冻结终端接口没有输出游标。终端接收端 lagged 时发送 busy，客户端显式 detach/attach 重置滚动视图；自动重放全部滚动缓冲会重复字节，因此没有采用。

## 离线验收

PowerShell（不调用真实模型、账号或 Claude Code）：

```powershell
$env:CARGO_NET_OFFLINE = 'true'
cargo test -p tether-server --offline
cargo clippy -p tether-server -- -D warnings
cargo clippy -p tether-server --all-targets --offline -- -D warnings
cargo fmt -p tether-server -- --check
```

测试始终使用真实 `tokio-tungstenite` 连接。`src/tests/fakes.rs` 提供内存存储与假终端；agent 执行全部委托 `EchoAdapter`。少量测试经内部注入事件检验 Echo 不输出的 tool/thinking/error 分支和确定性的广播溢出。

T02 / T07 合并后再人工运行（当前两个测试均有 `#[ignore = "需要 T02/T07 合并"]`）：

```powershell
cargo test -p tether-server --offline real_backends_ -- --ignored
```

依赖只使用工作区预置版本。新增的 SHA-256/常量时间比较直接依赖需要 T10 同步根 Cargo.lock 的 tether-server 条目，详见请求文件；本任务不提交根 lockfile。

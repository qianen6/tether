# ACP 通用适配器（T06）

## 实现

- 冻结的 `AcpAgent`、`AcpAdapter`、`adapter()` 与 `AgentAdapter::spawn()` 接口保持不变。
- `spawn()` 立即返回，后台启动进程；`session.rs` 使用同一套可注入内存流的状态机。
- 标准输入/输出按行传输 JSON-RPC 2.0；读任务独立、单写入方、消息上限 8 MiB，标准错误持续排空并保留末尾。
- 初始化 → 新建/加载会话 → 串行 prompt；`NativeId` 用于恢复。初始化期间也立即报告 `Send → working`。
- 流式消息、思考、工具、计划、审批和回合结束事件遵循 agent-api 的六项行为约定；从不输出 `user_text`。
- 支持多个待处理审批、真实 optionId、取消、打断后继续对话；关闭命令发送端等同于 Shutdown。
- 子进程退出先排空输出，再发唯一的 `Exited`；关闭/错误会终止并回收子进程。Windows 解析 `.cmd`/`.exe`，设置 `CREATE_NO_WINDOW`。
- 初始化/会话建立超时 30 秒，取消确认和写入超时各 5 秒；普通 prompt 不设置模型运行时限。

## 配置

`AcpAgent.command[0]` 是默认程序，后续元素是预设参数。`SpawnOptions.program` 只覆盖程序；参数为预设参数加 `SpawnOptions.args`。`cwd` 为空时使用当前目录，否则转为绝对路径；环境变量以 `SpawnOptions.env` 覆盖，命令查找尊重覆盖后的 PATH。`info().terminal_command` 保留完整预设命令。

## 规范对齐与任务要点补充

依据本地 `.ref/agent-client-protocol/` 的 `schema/v1/schema.json` 和 `docs/protocol/v1/`，参考仓库提交 `7441d106a853682cce61d2dd7db54123e4b9a477`。

1. 本任务的 `clientCapabilities`、`loadSession`、`stopReason` 是 **ACP v1**。明确协商 `protocolVersion: 1`；同一参考仓库的 v2 是不同协议，不混用其字段。agent 返回其他版本时报告错误并退出。
2. `session/load` 的返回值不要求 `sessionId`，沿用请求的 ID；加载响应前处理历史通知，但不重复产生 user_text 或伪造 turn_done。
3. 取消时必须给**所有**待处理权限请求回复 `cancelled`，随后发 `session/cancel`，直到 prompt 的取消响应到达才结束回合；期间继续接受最后的工具/文本更新。也处理 agent 的 `$/cancel_request`。
4. 工具初始状态也可能已经是 completed/failed。工具更新是补丁：省略/null 保留旧值，content 数组替换而不累加，空数组清空；最终输出支持 content、diff、rawOutput，避免重复 tool_end。
5. 有 `messageId` 时同 ID 的文本累加，不同 ID 分别产生最终 assistant_text；无 ID 时同回合累加，加载历史遇到用户消息时划分边界。未知更新和不支持的非文本内容忽略。
6. `allow_always → allow_session` 按任务映射到真实 optionId；**实际记忆范围/期限由 agent 定义**，完整 options/name 保留在 approval.detail，客户端不自行扩大授权。`reject_always` 没有忠实对应的统一决策，不伪装成 deny；abort 始终通过取消协议实现。同 kind 多个选项按原顺序取第一个。
7. 认证以标准错误码 `-32000` 判定，不根据 authMethods 非空自行登录；prompt 和 session/new/load 的认证错误均提示“请先在电脑上登录该 agent”。`-32800` 作为取消而不是普通失败处理。
8. proto 的 `truncate_tail` 在预算外添加省略号；本 crate 预留省略号字节，保证 tool_end.output **总计不超过 8 KiB**，不改冻结实现。

这些是规范补充/边界适配，没有修改冻结契约，也没有修改归属路径之外的文件。

## 离线测试

```powershell
$env:CARGO_NET_OFFLINE = 'true'
cargo test -p tether-adapter-acp --offline
cargo clippy -p tether-adapter-acp -- -D warnings
cargo clippy -p tether-adapter-acp --offline --all-targets -- -D warnings
cargo fmt -p tether-adapter-acp --check
rustfmt --edition 2024 --check crates/adapter-acp/tests/mock_acp/mock_program.rs
python fixtures/acp/validate.py
```

全部测试使用 Rust 内存模拟 agent；进程封装测试额外用本地 rustc 编译 std-only 的 `tests/mock_acp/mock_program.rs`，不启动任何真实 agent、账号登录或模型。tokio 的 test-util 仅为开发依赖，用虚拟时钟验证超时。schema 校验使用环境已有的 Python jsonschema，只读本地 schema，不下载依赖或访问外部引用。

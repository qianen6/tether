# T06 ACP 通用适配器

**阶段 1。归属路径：`crates/adapter-acp`、`fixtures/acp/`。**

## 目标

实现一个 ACP（Agent Client Protocol）客户端形式的 `AgentAdapter`：一个适配器接上所有支持 ACP 的 agent（Gemini CLI、OpenCode、Kimi CLI、Qwen Code、Cursor CLI、Copilot CLI 等，以及经适配器的 Claude、Codex）。

## 参考

规范和 schema 在 `.ref/agent-client-protocol/`（以其中的 schema 和文档为准，下面只是要点）。

- JSON-RPC 2.0，stdin/stdout，每行一条消息（**带** `"jsonrpc":"2.0"`）。
- `initialize`：`protocolVersion`、`clientCapabilities`（本阶段 `fs.readTextFile`、`fs.writeTextFile`、`terminal` 都设为 `false`）。从响应里读 `agentCapabilities`（是否支持 `loadSession`）和 `authMethods`。
- 新会话：`session/new`（`cwd`、`mcpServers: []`）→ `sessionId` 作为 `NativeId`。
- 恢复：agent 支持 `loadSession` 时用 `session/load`；不支持就新建会话，并发 `notice(info)`："该 agent 不支持恢复，已开始新会话"。
- 发消息：`session/prompt`（`sessionId`，`prompt: [{type:"text", text}]`）；响应里的 `stopReason` 表示回合结束。
- 打断：通知 `session/cancel`。
- agent 发来的通知 `session/update`，按 `update.sessionUpdate` 映射：

| sessionUpdate | 统一事件 |
|---|---|
| `agent_message_chunk` | `assistant_delta`（同一回合内累加，回合结束时补发一个 `assistant_text`） |
| `agent_thought_chunk` | `thinking` |
| `tool_call` | `tool_start{item: toolCallId, tool: kind 或 title, summary: title}` |
| `tool_call_update`（status 为 completed / failed） | `tool_end` |
| `plan` | `notice(info)`，列出计划条目 |
| 其他 | 忽略 |

- agent 发来的请求 `session/request_permission`（带 `options`，每项有 `optionId` 和 `kind`）：生成 `approval`，`choices` 按 options 里实际存在的 kind 映射：`allow_once → allow_once`、`allow_always → allow_session`、`reject_once → deny`。回复 `{"outcome":{"outcome":"selected","optionId":"<所选 option 的 id>"}}`；`abort` 时回复 `{"outcome":{"outcome":"cancelled"}}` 并发 `session/cancel`。
- agent 请求 `fs/*` 或 `terminal/*`：回复 JSON-RPC 错误（method not found）。
- 需要认证（`authMethods` 非空且 prompt 返回认证错误）：发 `notice(error)`："请先在电脑上登录该 agent"。

## 配置

`SpawnOptions.program` + `args` 指定具体的 agent 命令（例如 `gemini --experimental-acp`、`opencode acp`）。`host` 会为每个已知 ACP agent 创建一个带预设命令的实例，见 T10。`info()` 返回的 `id` 和 `name` 由构造参数传入：

```rust
pub struct AcpAgent { pub id: String, pub name: String, pub command: Vec<String> }
pub fn adapter(agent: AcpAgent) -> Arc<dyn AgentAdapter>;
```

## 测试

- 在 `tests/` 里用 Rust 写一个**模拟 ACP agent**（编译成测试用的小程序，或直接通过 `line_duplex` 在内存里跑），覆盖：初始化、新会话、流式回复、工具调用、权限请求（允许 / 拒绝 / 取消）、打断、不支持 `loadSession` 的恢复、认证错误。
- `fixtures/acp/` 放这些对话的 JSONL，供回放测试使用。

## 验收

```sh
cargo test -p tether-adapter-acp --offline
cargo clippy -p tether-adapter-acp -- -D warnings
```

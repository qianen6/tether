# T04 Codex 适配器

**阶段 1。归属路径：`crates/adapter-codex`、`fixtures/codex/`。** 这是唯一要用真实 agent 测试的适配器。

## 目标

实现 `AgentAdapter`：启动 `codex app-server`，用 JSON-RPC 驱动它，把它的输出翻译成统一事件。

## 协议要点（已核对本机 codex-cli 0.153 生成的 schema）

完整定义在 `.ref/codex-schema/`（`codex_app_server_protocol.v2.schemas.json` 以及 `ClientRequest.json`、`ServerRequest.json`、`ServerNotification.json`）。

- 传输：stdin/stdout，每行一条 JSON-RPC 消息，**没有** `"jsonrpc"` 字段。请求 `{id, method, params}`，响应 `{id, result}` 或 `{id, error}`，通知 `{method, params}`。
- 启动：`initialize`（`clientInfo: {name:"tether", title:"Tether", version}`）→ 等响应 → 发通知 `initialized`。
- 新会话：`thread/start`（`cwd`，`approvalPolicy: "on-request"`，`sandbox: "workspace-write"`）→ `result.thread.id` 作为 `NativeId`。
- 恢复：`thread/resume`（`threadId = resume`）。
- 发消息：没有进行中的回合时用 `turn/start`（`threadId`，`input: [{type:"text", text, text_elements: []}]`）；有进行中的回合时用 `turn/steer`（参数看 schema）。
- 打断：`turn/interrupt`（`threadId`、`turnId`）。`turnId` 从 `turn/started` 通知或 `turn/start` 的响应里拿。
- 通知到事件的映射：

| Codex 通知 | 统一事件 |
|---|---|
| `turn/started` | `state(working)` |
| `item/agentMessage/delta` | `assistant_delta{item: itemId, text: delta}` |
| `item/completed`，`agentMessage` | `assistant_text{item, text}` |
| `item/completed`，`reasoning` | `thinking{item, text: summary 拼接}` |
| `item/started`，`commandExecution` | `tool_start{item, tool:"shell", summary: command}` |
| `item/completed`，`commandExecution` | `tool_end{item, ok: exitCode==0, output: aggregatedOutput}` |
| `item/started` / `completed`，`fileChange` | `tool_start` / `tool_end`，`tool:"edit"`，summary 列出文件路径 |
| `item/started` / `completed`，`mcpToolCall`、`webSearch` | `tool_start` / `tool_end` |
| `turn/completed` | 如有 `turn.error` 先发 `notice(error)`；然后 `turn_done`、`state(idle)` |
| `error` | `notice(error)` |

  只处理 `threadId` 等于本会话的通知（子 agent 的线程忽略）。

- 服务器请求（带 `id`，必须回复）：

| Codex 请求 | 处理 |
|---|---|
| `item/commandExecution/requestApproval` | `approval{kind:"command", title:"运行：<command>", detail: 命令 + cwd + reason}`，`choices` 四个全给 |
| `item/fileChange/requestApproval` | `approval{kind:"file_change"}`，detail 用之前 `item/started` 里记下的 `changes`（路径 + diff，截断 8 KiB） |
| 其他（`item/tool/requestUserInput`、`mcpServer/elicitation/request`、`item/permissions/requestApproval` 等） | 回复拒绝或 JSON-RPC 错误，并发 `notice(warn)`："手机端暂不支持此类请求" |

  决定映射：`allow_once → "accept"`、`allow_session → "acceptForSession"`、`deny → "decline"`、`abort → "cancel"`。回复格式 `{id, result: {decision}}`。

- Windows：`codex` 通常是 npm 生成的 `codex.cmd`，用 `which` 解析后启动。

## 结构

- `protocol.rs`：JSON-RPC 编解码、请求编号与等待响应。
- `translate.rs`：通知和请求 → 统一事件（纯函数，好测试）。
- `adapter.rs`：进程管理，以及命令（Send / Answer / Interrupt / Shutdown）的处理。

## 测试

1. 单元测试：`translate` 对每种通知和请求的映射。
2. 录制数据测试：在 `fixtures/codex/` 放几段真实或手写的 JSONL 会话（普通问答、命令审批、文件修改审批、打断、出错），用 `testkit::line_duplex` 回放，检查输出事件序列。
3. **真实测试**（`tests/live.rs`，`#[ignore]`）：在临时目录启动真实 `codex app-server`：
   - 问答："只回复 OK" → 收到 `assistant_text` 和 `turn_done`；
   - 审批：让它运行一个命令（例如 `echo hi > a.txt`）→ 收到 `approval` → 回 `allow_once` → 文件被创建；
   - 恢复：拿到 `NativeId` → Shutdown → 用 `resume` 重新 spawn → 能继续对话。

   沙箱里跑不了（要联网调用模型），由人工执行。也可以用它录制 `fixtures/codex/` 的数据（把 stdin/stdout 写到文件的开关，通过环境变量 `TETHER_RECORD_DIR` 打开）。

## 验收

```sh
cargo test -p tether-adapter-codex --offline
cargo clippy -p tether-adapter-codex -- -D warnings
# 人工执行：
cargo test -p tether-adapter-codex -- --ignored --nocapture
```

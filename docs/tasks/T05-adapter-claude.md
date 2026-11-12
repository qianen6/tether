# T05 Claude Code 适配器

**阶段 1。归属路径：`crates/adapter-claude`、`fixtures/claude/`。**
**不做真实运行测试**，只用录制数据（按用户要求）。

## 目标

实现 `AgentAdapter`：以 stream-json 模式启动 `claude` 命令行，通过它的控制协议驱动会话、处理工具审批。这和官方 Claude Agent SDK 使用的是同一套协议。

## 协议要点（来自官方 claude-agent-sdk-python 0.2.179 源码）

- 启动参数：
  `claude --output-format stream-json --verbose --input-format stream-json --include-partial-messages --permission-prompt-tool stdio`
  恢复时再加 `--resume=<session_id>`。工作目录设为 `cwd`。
- 启动后先发初始化控制请求，并等待它的 `control_response`：
  `{"type":"control_request","request_id":"req_1","request":{"subtype":"initialize","hooks":null}}`
- 发消息：`{"type":"user","message":{"role":"user","content":"<text>"},"parent_tool_use_id":null,"session_id":""}`
- 打断：`{"type":"control_request","request_id":"req_N","request":{"subtype":"interrupt"}}`
- 标准输出的每行 JSON：

| `type` | 处理 |
|---|---|
| `system`，`subtype:"init"` | 取 `session_id` → `NativeId` |
| `stream_event` | `event.type == "message_start"` 时记下 `message.id`；`content_block_delta` 且 `delta.type == "text_delta"` → `assistant_delta{item: "<message.id>:<index>", text}` |
| `assistant` | 遍历 `message.content`：`text` → `assistant_text{item: "<id>:<index>"}`；`thinking` → `thinking`；`tool_use` → `tool_start{item: tool_use.id, tool: name, summary}` |
| `user` | 内容里的 `tool_result` → `tool_end{item: tool_use_id, ok: !is_error, output}` |
| `result` | `turn_done{summary: 耗时与费用}`，`state(idle)`；`is_error` 时先发 `notice(error)` |
| `control_request`，`subtype:"can_use_tool"` | 生成 `approval`（见下），记下 `request_id`、原始 `input` 和 `permission_suggestions` |
| `control_cancel_request` | 对应的审批作废：发 `notice(info)`，从待处理表删除 |
| `control_response` | 匹配自己发出的请求（initialize、interrupt） |

- 审批的回复：
  - `allow_once`：`{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"allow","updatedInput":<原 input>}}}`
  - `allow_session`：同上，再加 `"updatedPermissions": <permission_suggestions>`。**只有 suggestions 非空时**，才把 `allow_session` 放进 `choices`。
  - `deny`：`{"behavior":"deny","message":"用户在手机上拒绝"}`
  - `abort`：`{"behavior":"deny","message":"用户在手机上拒绝并停止","interrupt":true}`
- 审批卡片的生成：

| 工具 | title | detail |
|---|---|---|
| `Bash` | `运行：<command 前 80 字符>` | 完整 command + description |
| `Edit` / `MultiEdit` | `修改 <file_path>` | old_string / new_string 片段 |
| `Write` | `写入 <file_path>` | content 前 4 KiB |
| 其他 | `使用工具 <name>` | input 格式化后的 JSON（截断 8 KiB） |

  `tool_start.summary` 同理（Bash 用 command，文件类工具用路径）。
- Windows：用 `which` 找 `claude`（可能是 `.exe` 或 `.cmd`）。

## 结构与测试

结构同 T04（`protocol.rs` / `translate.rs` / `adapter.rs`）。

- 单元测试：`translate` 的每种消息；审批回复的 JSON 格式与上文完全一致。
- 录制数据：在 `fixtures/claude/` 手写 JSONL 会话：普通问答（含流式片段）、Bash 审批允许、Edit 审批拒绝、带 suggestions 的本会话允许、打断、`control_cancel_request`、`result` 出错、`--resume` 恢复。用 `testkit::line_duplex` 回放，同时检查适配器写给 claude 的内容。

## 验收

```sh
cargo test -p tether-adapter-claude --offline
cargo clippy -p tether-adapter-claude -- -D warnings
```

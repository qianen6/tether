# DeepSeek Harness（DSH）录制数据

`crates/adapter-dsh` 的测试数据。2026-11-12 用本机的 DSH 0.1.1-rc.2（`dsh web`）录制：
DSH 跑在一次性的 `DSH_HOME` 里，模型换成本地脚本（OpenAI 兼容接口，按消息里的关键词回固定内容或工具调用），
所以没有用到真实账号，也没有调用真实模型。录完后把本机路径换成了 `C:\work\demo`、`C:\Users\me`、`C:\dsh`，
并去掉了 DSH 发给模型的系统提示（`request/header`）和运行时上下文消息。

每行 `{"dir", "v"}`：

| dir | 内容 |
|---|---|
| `req` | 发给 `POST /api/<method>` 的 `client-request`，或发给 `/api/respond` 的 `client-response` |
| `res` | 对应的 `server-response`（`/api/respond` 的回执是 `{"accepted":…}`） |
| `events.mux` | `ws://…/api/events.mux` 上的 `server-request`，`payload` 是会话帧 |
| `events.host` | `ws://…/api/events.host` 上的 `server-request`，`payload` 是主机帧 |

| 文件 | 内容 |
|---|---|
| `reply-tool-cancel.jsonl` | 新会话；三轮：普通回复、PowerShell 命令（失败）、再一轮回复；第四轮执行 `Start-Sleep` 时 `session.cancel`；然后 `session.list`、`session.history`、`session.models`、`agentPreset.list` |
| `approval-question.jsonl` | 模型要求越出沙箱（`sandbox_permissions`）→ `approval/requested`：第一次拒绝、之后允许；`ask_user_question` → `question/requested`（这次用了错误的回答格式，DSH 回 `bad-response`）；`session.selectModel` 成功与失败；不存在的会话 |
| `resume-commands.jsonl` | 重启 `dsh web` 后接上旧会话；`commands/list`、`commands/execute`（`/permission read-only`、`/permission`、未知命令）；`llm.models`、`llm.providers`；正确格式回答问题 |

## 协议要点（0.1.x）

- 没有令牌和 cookie：`/api` 只认回环地址的 `Host`。0.2 起改成令牌换 cookie、方法名 `session/prompt`，这里没有实现。
- 一元调用：`POST /api/<method>`，`{type:"client-request", rpcId, method, payload}` → `{type:"server-response", rpcId, result:{ok, value|error}}`。
  插件提供的方法（`commands/execute`、`commands/list`）同样走这里，`payload` 为 `{args:{…}}`。
- 两条只下行的 WebSocket：`events.mux` 推所有已接上会话的帧（`session/event`、`approval/*`、`question/*`、`session/queue`、`session/projection`），
  `events.host` 推会话增删和 `host/session-status`。`host/session-status` 的 `running` 会在一轮中途变成 `false`，
  不能用来判断一轮结束；以 `session/event` 的 `turn/end` 为准。
- 读一个会话（`session.history`、`commands/list` 等）会在 `dsh web` 里把它接上（冷恢复），之后它的帧才出现在 `events.mux`。
- 审批：用 `approval/requested` 帧外层的 `rpcId` 回 `/api/respond`，`value = {sessionId, approvalId, outcome: "allowed-once"|"rejected"}`。
- 问题：同样用帧的 `rpcId`，`value = {sessionId, answer:{answers:[{id, selected:[选项文字], custom?}]}}`；取消用 `result:{ok:false, error:{code:"cancelled"}}`。
- 权限只能通过命令切换：`commands/execute` 执行 `/permission <read-only|workspace-write|danger-full-access>`。
  直接把 `/permission …` 当消息发给 `session.prompt` 不会执行命令，只会成为一条普通消息。

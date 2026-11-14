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

- 没有令牌和 cookie：`/api` 只认回环地址的 `Host`。0.2 的协议见下面一节。
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

## 0.2（`v2/`）

`v2/conversation.jsonl`：2026-11-12 用 npm 上的 `@deepseek-ai/dsh` 0.2 录制，同样是一次性的 `DSH_HOME` 加本地脚本模型。
内容：新会话里的普通回复、PowerShell 命令、越出沙箱的审批（先拒绝、再允许一次）、`ask_user_question` 和回答、执行中取消，
以及 `session/selectModel`、`session/list`、`session/events`、`commands/list`、`commands/execute` 等调用。
路径同样换成了 `C:\work\demo`、`C:\Users\me`，去掉了系统提示和运行时上下文消息，不含 token 与 cookie。

每行 `{"dir", "v"}`：`req`/`res` 是一元调用的请求和回复；`open`/`close` 是发上 `/api/remote.mux` 的 `open`/`cancel`；
`mux` 是 `/api/remote.mux` 下行的 `item`/`end`/`error`。

协议要点：

- 鉴权：`dsh web` 打印 `http://127.0.0.1:端口/?token=…`。token 每次启动随机生成，只在这个进程里有效，DSH 也不把它写进任何文件。
  `GET /connect?token=…&interactions=answer` 回 302 和 `dsh-auth-…` cookie，之后的请求和 WebSocket 都要带这个 cookie，没有就是 401。
- 一元调用：`POST /api/<命名空间>/<方法>`，`payload` 是 `{args:{<参数名>: …}}`，例如 `session/create` 是 `{request:{cwd}}`，
  `session/list` 是 `{_request:{}}`。错误码改为 `session/not-found` 这种写法。
- 下行全在一条 WebSocket `/api/remote.mux` 上，按 `streamId` 分流：
  - `$events`（参数 `{}`）：先来 `ready{clientId}`；`waterfall` 是要人回答的事（`approval/request`、`user-questions/request`），
    用 `POST /api/$events/result` 回答，参数 `{clientId, eventId, outcome}`，`outcome` 为 `{kind:"result", value}`、`{kind:"next"}`
    或 `{kind:"rejected", error}`；`emit` 是主机通知（`api-session/added|removed|status|error`）；`cancel` 表示别处已经答了。
  - `session/follow`（参数 `{request:{address:{kind:"session", sessionId}, activate:true, assistantStream:true}}`）：先来 `snapshot`，
    然后是 `event`（和 0.1 相同的会话事件，多了 `approval/asked`、`approval/decided`）和 `assistant-stream`（流式文字）。
- 审批回答的值是 `"allowed-once"`、`"rejected"` 等字符串；问题是 `{answers:[{id, selected, custom?}]}`。
- 工具结果的 `isError` 和文字直接在 `message` 上（0.1 在 `message.result` 里）。

适配器的做法：`src/v2.rs` 把 0.1 的调用名翻译成 0.2 的端点和参数、把结果整理回 0.1 的形状，再把 `remote.mux` 的帧转成 0.1 的帧；
会话逻辑（`session.rs`、`translate.rs`）两个版本共用。

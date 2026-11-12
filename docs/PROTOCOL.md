# 通信协议 v1（冻结）

修改本文件须经整合任务（T11）统一处理。各任务如需改动，写到 `docs/requests/`。

## 1. 传输层

```
手机 App ── ws://127.0.0.1:<p>/v1/ws ──▶ react-native-tailpocket（手机上的本机转发）
         ══ TailPocket 隧道（Tailcat / WireGuard，端到端加密）══
         ──▶ tailpocket-host（电脑，只放行已配对手机）──▶ 网关 127.0.0.1:<gateway_port>/v1/ws
```

- 网关只监听 `127.0.0.1`。隧道负责加密与"只放行已配对手机"；网关负责应用层认证（设备令牌）。
- 局域网直连模式（`--lan`）不在 v1 范围内。

## 2. 帧

- **文本帧**：一个 JSON 对象，字段 `t` 为消息类型（snake_case）。
- **二进制帧**：终端数据。格式：

| 偏移 | 长度 | 内容 |
|---|---|---|
| 0 | 1 | 类型：`0x01` 终端输出（电脑→手机）、`0x02` 终端输入（手机→电脑） |
| 1 | 1 | 终端 id 的字节长度 `n`（1–255） |
| 2 | n | 终端 id（UTF-8） |
| 2+n | 其余 | 原始字节 |

- 单帧上限 8 MiB。超限：发送 `error`（code `bad_request`）并丢弃。

## 3. 连接与认证

连接建立后，手机发送的**第一帧**必须是 `pair` 或 `hello`；5 秒内未收到则关闭（close code 4400）。

### 3.1 首次配对

电脑开配对窗口时生成一次性 **配对密钥**（32 字节随机，base64url），作为 TailPocket 配对的 `payload` 在隧道内交给手机。手机连上后：

```json
→ {"t":"pair","version":1,"secret":"<配对密钥>","device_name":"Pixel 9"}
← {"t":"paired","device_id":"<uuid>","token":"<设备令牌，32 字节 base64url>"}
```

随后同一连接直接进入已认证状态（等同于发送过 `hello`），电脑紧接着发送 `welcome`。

- 密钥一次性，窗口最长 15 分钟（默认 10 分钟）。
- 失败返回 `{"t":"rejected","reason":"invalid_secret"}` 并关闭（4401）。连续失败 5 次，60 秒内拒绝一切配对，原因为 `"locked"`。
- 电脑只保存令牌的 SHA-256。

### 3.2 之后每次连接

```json
→ {"t":"hello","version":1,"token":"<设备令牌>","device_name":"Pixel 9"}
← {"t":"welcome","version":1,"host_name":"家里的电脑","device_id":"<uuid>","server_time":1730000000000}
```

失败：`{"t":"rejected","reason":"invalid_token" | "revoked" | "version_mismatch"}`，然后关闭（4401；版本不符 4426）。
`revoked` 只在本次运行中见过该令牌时返回；电脑端重启后，已吊销设备的令牌返回 `invalid_token`。两者对客户端的含义相同：令牌失效，需要重新配对。

设备被吊销时，该设备的所有连接立即关闭（4401），同时从隧道移除其手机公钥。

## 4. 请求与响应

手机发出的请求都带 `req`（u64，连接内递增）。电脑回复：

```json
{"t":"ok","req":7,"data":{...}}
{"t":"error","req":7,"code":"not_found","message":"..."}
```

错误码：`bad_request`、`unauthorized`、`not_found`、`agent_unavailable`、`busy`、`internal`。没有对应请求的错误 `req` 为 `null`。

| 请求 `t` | 字段 | 成功时 `data` |
|---|---|---|
| `list_agents` | — | `{"agents":[AgentInfo]}` |
| `list_sessions` | — | `{"sessions":[SessionInfo]}` |
| `create_session` | `agent`, `cwd?`, `title?`, `resume?`（`HistoryThread.id`，接着这个对话聊）, `model?`, `effort?`, `speed?`（`SpeedInfo.id`，`"default"` 为标准速度）, `permission?`（`PermissionInfo.id`：agent 不问就能做什么；省略时按旧规则：对话只读、其余可改工作文件夹）, `chat?`（无项目的对话：agent 自建文件夹，忽略 `cwd`；不带 `permission` 时只读）, `project?`（`ProjectInfo.id`）, `roots?`（该项目全部文件夹，`cwd` 取其一）, `reveal?`（第一条消息后在电脑上的 agent 应用里打开这个对话）, `fork?`（和 `resume` 一起用：复制那个对话成新对话再接着聊，原对话不动；用于它正被别的程序占着时） | `{"session":SessionInfo}`。`resume` 已有空闲会话时先关掉它再新建；它正在工作则直接返回它。`fork` 不带 `resume` 回 `bad_request` |
| `set_model` | `session`, `model?`, `effort?`, `speed?`, `permission?`（`null` 表示不改这一项） | `{}`，下一轮起生效，并广播 `session_update` |
| `list_models` | `agent` | `{"models":[ModelInfo],"permissions":[PermissionInfo],"default_permission":string\|null}`。`models` 可能为空（手机可手填模型名）；`permissions` 是电脑上这个 agent 提供的权限档位（不同版本可能不同），为空表示不提供选择；`default_permission` 是它自己的应用当前用的档位 |
| `list_history` | `agent`, `search?`, `cursor?` | `{"threads":[HistoryThread],"next_cursor"?,"projects":[ProjectInfo]}`，新的在前；`projects` 只在第一页（无 `cursor`）给出；没有历史的 agent 回 `bad_request` |
| `reveal` | `agent`, `thread` | `{}`，在电脑上 agent 自己的应用里打开这个对话（如 Codex 桌面版），让它在那边也列出来并能接着用：手机上有会话在这个对话上时，先让它放开（停掉它的 agent 进程，下一条消息再续接），再打开；那个会话正在工作则回 `busy`。没有这种应用的 agent 回 `bad_request` |
| `read_history` | `agent`, `thread` | `{"thread":HistoryThread,"events":[Event],"truncated":bool}`，`seq` 从 1 起，太长时只留最新的几轮 |
| `subscribe` | `session`, `after`（u64，0 表示全部） | `{"session":SessionInfo}`，随后按序推送 `seq > after` 的历史 `event`，再推实时事件 |
| `unsubscribe` | `session` | `{}` |
| `send_message` | `session`, `text`, `images?`（电脑上的图片路径数组，通常是 `put_file` 返回的） | `{}`。Codex 把图片作为 `localImage` 一起发出；只收文字的 agent 把路径附在文字后 |
| `put_file` | `name`（原文件名，只取扩展名判断类型）, `data`（base64，解码后不超过 `MAX_FILE` = 5 MiB） | `{"path"}`，图片存进电脑临时目录的 `tether-uploads`。只收 png/jpg/gif/webp/bmp/heic，否则 `bad_request` |
| `get_file` | `path`（电脑上的绝对路径） | `{"name","mime","data"}`（base64），只读图片且不超过 `MAX_FILE`；不存在回 `not_found` |
| `answer` | `session`, `approval`, `decision` | `{}` |
| `interrupt` | `session` | `{}` |
| `close_session` | `session` | `{}` |
| `term_open` | `command?`（字符串数组）或 `agent?`，`cwd?`, `cols`, `rows` | `{"term":TermInfo}`，并自动附着 |
| `term_attach` | `term` | `{"term":TermInfo}`，随后以二进制帧发送滚动缓冲区，再发实时输出 |
| `term_detach` | `term` | `{}` |
| `term_resize` | `term`, `cols`, `rows` | `{}` |
| `term_kill` | `term` | `{}` |
| `list_terms` | — | `{"terms":[TermInfo]}` |
| `ping` | `n` | 不回 `ok`，回 `{"t":"pong","n":...}` |

终端输入不走请求，直接发 `0x02` 二进制帧。

### 4.1 Codex 会话自动选路（T11b）

`codex` 和 `codex-desktop` 都在会话启动时自动选路；手机无需增加消息字段或选项：

- 续接已有对话（`resume` 有值、不是 `fork`），且电脑上的 Codex 桌面版已经加载并持有该对话的 owner：经桌面版 IPC 跟随。手机的消息、中断和审批交给 owner 执行，快照/patch 转为原有事件。回合进行中的消息按 FIFO 排队，上一回合结束后才开始下一回合，不发 steer。
- 桌面版开着，但对话没有 owner（新对话、分叉、桌面版没加载的对话）：沿用后台直连 app-server。
- 桌面版没开（连不上 IPC）：会话以错误结束，文字为「电脑上的 Codex 桌面版没有打开。请先在电脑上打开 Codex，再发送。」不改走直连。桌面版 3 秒内不回应：「电脑上的 Codex 桌面版没有回应。请确认它已正常打开，再发送。」桌面版回答了 Tether 不认识的内容（例如更新后版本号变了）：「电脑上的 Codex 桌面版已更新，Tether 暂时没法接管它正开着的这个对话。可以在电脑上继续，或者分叉成新对话在手机上继续。」（手机据“分叉成新对话”提供分叉按钮）。跟随中的错误不会自动重发消息到另一条路。
- 设置 `TETHER_CODEX_DIRECT=1` 时始终直连。实现将该环境变量存在的任何值都视为强制直连。
- 自动选路绝不通过 `codex://threads/...` 打开或切换桌面对话。Codex 忽略 `create_session.reveal` 的自动打开提示（旧手机默认会发送 `true`），新建、分叉和回退也不跳转窗口；用户主动调用的独立 `reveal` 请求保持原行为。
- 管道关闭或 owner 断开后，会话以错误结束，文字为「电脑上的 Codex 桌面版关闭了。请在电脑上重新打开 Codex，再发送。」下一条消息由服务器按 `native_id` 重开并重新选路。
- 60 秒空闲释放保持不变。释放直连会话会结束其 app-server；释放跟随会话仅广播 `following:false` 并断开 Tether 自己的 IPC 连接，不停止桌面版。

`user_text` 也可能是用户在电脑上的 Codex 桌面版直接输入的文字。手机发出的文字仍由服务器记录，跟随事件会去掉这部分回声；首次快照不重复播放已有历史，但同步进行中的回合和待审批项。

## 5. 电脑主动推送

| `t` | 字段 | 说明 |
|---|---|---|
| `event` | `session`, `event`（Event） | 只发给订阅了该会话的连接 |
| `session_update` | `session`（SessionInfo） | 发给所有连接：新建、改名、状态变化 |
| `session_closed` | `session`（id） | 发给所有连接 |
| `term_exit` | `term`, `code?` | 发给附着了该终端的连接 |
| `notify` | `kind`, `session?`, `term?`, `title`, `body` | 发给所有连接；`kind`：`needs_you`、`done`、`failed` |

## 6. 心跳与重连

- 手机每 20 秒发 `ping`；电脑 60 秒收不到任何帧即关闭连接。
- 重连后手机重新 `hello`，对每个关注的会话用 `subscribe(after = 本地最后的 seq)` 补齐。
- 同一 `item` 的 `assistant_delta` 只用于实时显示，不存历史；回合结束时的 `assistant_text` 以完整文本替换。

## 7. 数据类型

字段名一律 snake_case；可选字段缺省时省略或为 `null`，两种都要接受。

```text
AgentInfo   { id, name, kind: "codex"|"claude"|"acp"|"terminal"|"echo",
              available: bool, detail?: string, chat: bool, terminal_command: string[] }

SessionInfo { id, agent, title, cwd, state: SessionState, status_line?: string,
              created_at: u64, updated_at: u64, last_seq: u64,   // 时间均为 Unix 毫秒
              model?: string, effort?: string, chat: bool, speed?: string,
              thread?: string,    // agent 自己的对话 id（HistoryThread.id），开始后才有
              permission?: string }  // PermissionInfo.id；null 表示按旧规则

ModelInfo   { id, name, description?: string, efforts: string[], default_effort?: string,
              is_default: bool, speeds: SpeedInfo[], default_speed?: string }

SpeedInfo   { id, name, description?: string }               // 比标准更快的速度，如 {id:"priority",name:"Fast"}

PermissionInfo { id, name, description?: string }            // 权限档位，如 Codex 的 ":read-only" ":workspace" ":danger-full-access"

ProjectInfo { id, name, roots: string[] }                    // agent 应用里定义的项目，可含多个文件夹

HistoryThread { id, title, preview, cwd, origin?: string, created_at: u64, updated_at: u64,
                chat: bool, project?: string }                     // agent 自己保存的对话；project 为 ProjectInfo.id

SessionState = "idle" | "working" | "needs_you" | "error" | "exited"

TermInfo    { id, command: string[], cwd, cols: u16, rows: u16, running: bool, created_at: u64 }

Event       { seq: u64, at: u64, e: <事件类型>, ...事件字段 }      // 扁平结构

事件类型（e）：
  user_text        { text, images? }             // 手机消息，或在 Codex 桌面版直接输入的文字；images 为电脑上的图片路径（get_file 读取）。桌面版带附件发的消息只给用户输入的部分
  assistant_delta  { item, text }                // 不存历史
  assistant_text   { item, text }
  thinking         { item, text }
  tool_start       { item, tool, summary }
  tool_end         { item, ok: bool, output }    // output 截断到 8 KiB
  approval         { approval: Approval }
  approval_done    { approval: string, decision: Decision }
  state            { state: SessionState, status_line?: string }
  turn_done        { summary?: string }
  notice           { level: "info"|"warn"|"error", text }

Approval    { id, kind: "command"|"file_change"|"tool"|"other",
              title, detail, cwd?: string, choices: Decision[] }

Decision    = "allow_once" | "allow_session" | "deny" | "abort"
```

Rust 定义在 `crates/proto`，TypeScript 由 ts-rs 生成到 `packages/client/src/generated/`。

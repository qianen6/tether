# tether-adapter-codex

实现冻结的 `AgentAdapter` 接口，使用 codex-cli 0.153.4 的 app-server v2
按行协议。公开接口、依赖清单和工作区契约保持不变。

## 分层

- `src/protocol.rs`：数字/字符串请求 ID、JSON 编解码、关联响应、30 秒响应
  超时、可选原始输入/输出录制。发送的数据没有 `jsonrpc` 字段。
- `src/translate.rs`：纯函数映射通知、审批和四种决定。工具输出和审批 diff
  的 UTF-8 尾部总长不超过 8 KiB，包含截断提示符。
- `src/adapter.rs`：异步进程管理、初始化/恢复、开始/追加输入、打断、审批、
  退出。进程和内存流共用同一协议状态机；`spawn` 立即返回，启动错误通过
  唯一的 `Exited` 输出报告。关闭命令发送端等同于 Shutdown。

`SpawnOptions.program` 覆盖可执行文件；实际参数为 `app-server` 加 `args`。
`env` 和 `cwd` 传给子进程。Windows 通过 `which` 解析 `.cmd`/`.exe`，
使用 CREATE_NO_WINDOW。关闭时先关闭 stdin，2 秒后仍未退出则终止进程；
Windows 同时清理包装脚本的子进程树。stderr 独立排空，失败时附带最后 8 KiB。

Send 在初始化期间先输出 working 并排队；已有回合时使用带
`expectedTurnId` 的 `turn/steer`。回合 ID 未到达时暂存打断请求。
子线程的通知被忽略，子线程或过期回合的审批得到错误响应。
重复/迟到的回合通知与响应不会重新激活已结束的回合。
不支持的服务器请求得到 -32601 和中文 warn 通知；无效审批得到 -32602。

## 离线验收

```powershell
cargo test -p tether-adapter-codex --offline
cargo clippy -p tether-adapter-codex --offline -- -D warnings
cargo clippy -p tether-adapter-codex --offline --all-targets -- -D warnings
cargo fmt -p tether-adapter-codex -- --check
```

## 人工真实测试与录制

以下命令由人工显式运行，使用已配置的 Codex 账号/模型。自动验收仅编译
这些 `#[ignore]` 测试，没有执行它们。审批测试要求模型明确请求 shell 审批；
模型或主机配置未产生审批时，测试会超时失败，而不是跳过断言。

```powershell
$env:TETHER_RECORD_DIR = Join-Path $env:TEMP 'tether-codex-recordings'
cargo test -p tether-adapter-codex --offline --test live -- --ignored --nocapture
```

`TETHER_RECORD_DIR` 默认关闭，`SpawnOptions.env` 中同名值优先于宿主环境；
空值显式关闭录制。每次 spawn 使用 UUID 前缀写入 `.stdin.jsonl`、
`.stdout.jsonl`、`.stderr.log` 三个文件，保留原始内容，避免会话互相覆盖。
目录和文件创建错误通过 `Exited.error` 报告。

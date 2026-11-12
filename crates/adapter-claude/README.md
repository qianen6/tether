# Claude Agent 适配器（T05）

- `protocol.rs`：stream-json 信封、启动参数、控制请求、精确的审批回复。
- `translate.rs`：流式/完整消息的 item 对齐、工具输出与审批卡片、回合结束状态。
- `adapter.rs`：异步进程薄封装和可注入内存流的会话循环；初始化完成前缓存输入。
- `tests.rs`：通过 `tether_agent_api::testkit::line_duplex` 回放手写 JSONL，核对完整出站 JSON。

`allow_session` 仅在本次 `permission_suggestions` 为非空数组时出现；
回复原样带回本次 input/suggestions。迟到、重复或未提供的决定不会消耗有效审批。
`abort` 只在 deny 的内层回复中设置 `interrupt: true`；普通打断使用独立控制请求。
`tool_end.output` 使用冻结的 `truncate_tail`，本地预留省略号字节，完整结果不超过 8 KiB。

进程启动解析 `.exe`/`.cmd`，Windows 设置 `CREATE_NO_WINDOW`。
关闭命令通道等同 Shutdown；stdout EOF、初始化失败与进程退出最后均由进程封装发送一次 Exited。
stdin 的单写者 FIFO 与命令、stdout 并行轮询，子进程不读输入时仍能处理 Shutdown 和发送端关闭。
stderr 独立排空并只保留末尾 8 KiB，退出时回收任务和子进程。

验收（全程离线，不运行真实 Claude 或模型）：

```sh
cargo test -p tether-adapter-claude --offline
cargo clippy -p tether-adapter-claude --offline -- -D warnings
cargo clippy -p tether-adapter-claude --offline --all-targets -- -D warnings
cargo fmt -p tether-adapter-claude -- --check
```

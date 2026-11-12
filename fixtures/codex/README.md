# Codex app-server 回放

这些 JSONL **全部为手写合成数据**，不是实际账号或模型的录制；字段按
`.ref/codex-schema/`（codex-cli 0.153.4）整理。文件只包含服务器 → 客户端
的数据，没有 `jsonrpc` 字段。初始化由内存测试的假服务器完成，请求编号
1、2 分别用于 initialize 和 thread/start，3 用于首个 turn/start。

- `qa.jsonl`：中文流式问答、完整文本替换。
- `command-approval.jsonl`：命令审批；`// ANSWER` 是测试暂停点。
- `file-approval.jsonl`：文件修改审批，diff 来自先前的 item/started。
- `interrupt.jsonl`：打断；`// INTERRUPT` 是测试暂停点。
- `error.jsonl`：重试错误通知和最终失败。
- `tools.jsonl`：reasoning、shell、edit、MCP、webSearch 的生命周期。

`//` 注释和空行由 `tether_agent_api::testkit::LineEnd::send_fixture` 跳过。
真实人工测试可设置 `TETHER_RECORD_DIR`，每次 spawn 生成独立的
`codex-<uuid>.stdin.jsonl`、`.stdout.jsonl` 和 `.stderr.log`。stdout JSONL
可以直接交给 `send_fixture`；stdin 用于校验请求与编号。录制文件保留原文，
提交前由录制者检查内容；测试不会自动把真实会话写进本目录。

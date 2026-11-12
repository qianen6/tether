# T10 整合

**阶段 2，串行。** 前置：T01–T08 已合并到 `main`（T09 可以晚一点）。
**归属路径：`crates/host`、`apps/cli`、`e2e/`，以及处理 `docs/requests/` 时涉及的契约文件。**

## 目标

把所有模块组装成可用的 `Host` 和 `tether` 命令行，打通真实的端到端流程。

## 步骤

1. **处理契约修改请求**：逐个阅读 `docs/requests/*.md`，合理的就修改 `PROTOCOL.md`、`CONTRACTS.md` 和相应代码，并在请求文件末尾写上处理结果。
2. **`crates/host`**：
   - 按系统确定数据目录；加载或创建 `config.toml`（电脑名、`derp_map_url`）和 `agents.toml`（自定义 agent）。
   - 组装：`Store` → `TermManager` → agent 列表 → `Server`（127.0.0.1 随机端口）→ `Tunnel`（`forward` 指向网关）。
   - `pair`：`server.new_pair_secret(ttl)` 作为 `payload` 调用 `tunnel.pair`；手机加入后（`PhoneJoined`）与随后的 `on_paired`（设备）配对，调用 `store.set_phone_key`（两者到达顺序不定，参考 `.ref/boop/src/pocket.ts` 的 `link`）。
   - `revoke`：`server.revoke` 返回设备后，再调用 `tunnel.revoke(phone_key)`。
   - 启动时用 `tunnel.set_allowed` 同步存储里所有设备的 `phone_key`（处理上次关机期间被移除的设备）。
3. **agent 发现**：
   - 内置：`codex`（`codex`）、`claude`（`claude`）。
   - ACP 预设：Gemini CLI（`gemini --experimental-acp`）、OpenCode（`opencode acp`）、Kimi CLI、Qwen Code、Cursor（`cursor-agent acp`）、Copilot（`copilot --acp`）。具体命令以 `.ref/agent-client-protocol` 里的文档为准，查不到的标注"未验证"。
   - 自定义：`agents.toml` 中 `mode = "acp"` 用 ACP 适配器，`mode = "terminal"` 只出现在终端列表里。
   - 用 `which` 检测是否安装；没装的 `available = false`，`detail` 写"未找到命令 xxx"。
   - 每个 agent 的 `terminal_command` 用于终端模式；另外提供一个"Shell"（Windows 用 PowerShell，其他系统用 `$SHELL`）。
4. **`apps/cli`**：
   - `tether serve`：前台运行，日志写到终端和数据目录下的文件。
   - `tether pair`：连接正在运行的 serve（数据目录下的本地控制接口：Unix 用套接字，Windows 用命名管道或 127.0.0.1 加随机令牌），让它开配对窗口，在终端里显示二维码（`qrcode`）。
   - `tether devices`、`tether revoke <id>`：同样通过本地控制接口。
5. **端到端测试 `e2e/`**（Rust 集成测试，`#[ignore]`，需要联网）：
   - 启动 `Host`（临时数据目录）→ `pair` → 用 `tailpocket-phone` 扫码，拿到手机端本地转发端口 → 用 WebSocket 发 `pair` → 拿到令牌。
   - Echo 会话：完整的一轮对话加审批。
   - 终端：打开 shell，经隧道执行 `echo`。
   - 吊销：连接立即断开，隧道也连不上了。
   - 真实 codex：经隧道完成一次带命令审批的任务。
   - 另写一个 TS 端到端测试：`packages/client` 经 `tailpocket-phone` 的转发端口连接，跑完上面的流程（放在 `packages/client/test/e2e.live.test.ts`）。

## 验收

```sh
cargo build --workspace --offline
cargo test --workspace --offline
cargo clippy --workspace -- -D warnings
pnpm -r test
# 人工执行（联网）：
cargo test -p tether-e2e -- --ignored --nocapture
pnpm --filter @tether/client test:live
```

人工执行时，在 Windows 上走一遍：`tether serve` → `tether pair` → 用 `tailpocket-phone` 配对 → 确认能列出本机的 codex 并完成一次对话。

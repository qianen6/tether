# AGENTS.md — 给 Codex 的项目规则

本仓库是 **Tether**（临时名）：手机通过加密隧道控制电脑上的编程 agent（Codex、Claude Code、ACP agent、任意终端程序）。
每个 Codex 会话只执行 `docs/tasks/` 中的**一个**任务文件。开始前按顺序阅读：

1. 本文件
2. `docs/PLAN.md`（整体计划与并行方式）
3. `docs/PROTOCOL.md`（手机 ↔ 电脑通信协议，已冻结）
4. `docs/CONTRACTS.md`（各 crate / 包的接口，已冻结）
5. 你的任务文件 `docs/tasks/Txx-*.md`

## 硬性规则

1. **只修改任务文件"归属路径"里列出的文件。** 需要改归属范围以外的文件（尤其是 `crates/proto`、根 `Cargo.toml`、`docs/PROTOCOL.md`、`docs/CONTRACTS.md`），不要改，而是在 `docs/requests/<任务编号>.md` 写明要改什么、为什么，然后用不改的方式继续（例如在自己的 crate 里临时适配）。
2. **契约冻结。** 不得更改 `PROTOCOL.md` 与 `CONTRACTS.md` 定义的类型、函数签名和消息格式。可以在自己的 crate 里增加内部私有代码。
3. **没有网络。** 依赖、参考仓库、TailPocket 程序都已由 T00 预先下载（`cargo fetch`、`pnpm install`、`.ref/`、`vendor/`）。缺东西时停止并在最终报告里说明，不要尝试联网。
4. **不跑需要真实账号或真实模型的测试**，除非任务文件明确要求。这类测试标记为 `#[ignore]`（Rust）或放在 `*.live.test.ts`（TS），并在报告里写清手动运行命令。
5. **Claude Code 不做真实运行测试**，只用 `fixtures/` 里的录制数据测试。
6. 完成前必须运行任务文件"验收"一节的全部命令且通过。不能通过就如实报告，不要删除或跳过测试。
7. 最终回复用中文，包含：完成了什么、改了哪些文件、验收命令输出摘要、未完成或有疑问的地方、对契约的修改请求（如有）。

## 技术约定

- Rust：edition 2024，`tokio` 异步，错误用 `anyhow`（库内部可用 `thiserror`），日志用 `tracing`。禁止 `unwrap()` 出现在非测试代码中处理外部输入。
- 每个 crate 自带单元测试；涉及子进程的逻辑必须能在**内存流**上测试（`tokio::io::duplex` 或等价方式），启动真实进程只是一层薄封装。
- 跨平台：Windows / macOS / Linux。路径用 `std::path`；在 Windows 上用 `which` 解析 `.cmd`/`.exe`；GUI 场景启动子进程要加 `CREATE_NO_WINDOW`（`#[cfg(windows)]`）。
- TypeScript：严格模式（`strict: true`），ESM，**Node 22.12 以上**，测试用 `vitest`。本机的 Node 在 conda 环境 `tether` 里（Node 22 + pnpm，不用系统的 Node 20）：PowerShell 先 `conda activate tether`，Git Bash 先 `. scripts/env.sh`。协议类型只从 `packages/client/src/generated/` 导入，不手写重复定义。
- 格式化：`cargo fmt`、`cargo clippy -- -D warnings`（只针对自己的 crate）、`pnpm -r lint`（如该包配置了）。
- 提交：在自己的分支上提交，信息格式 `Txx: 简述`。不要修改 `main`。

## 命名

- Rust 包名：`crates/<dir>` 一律叫 `tether-<dir>`（如 `tether-adapter-codex`）；`apps/cli` 叫 `tether`（二进制 `tether`）；`apps/desktop/src-tauri` 叫 `tether-desktop`；`e2e/` 叫 `tether-e2e`（由 T10 创建并加入 workspace）。
- pnpm 包名：`packages/client` 叫 `@tether/client`；`apps/desktop` 叫 `desktop`；`apps/mobile-debug` 叫 `mobile-debug`。
- 根 `Cargo.toml`、`pnpm-workspace.yaml` 只由 T00 和 T10 修改。

## 目录

```
crates/proto            协议类型（冻结；ts-rs 生成 TS）
crates/store            SQLite：设备、会话、事件
crates/agent-api        agent 适配器的公共接口与测试工具
crates/adapter-codex    Codex（codex app-server）
crates/adapter-claude   Claude Code（stream-json）
crates/adapter-acp      ACP 通用客户端
crates/terminals        PTY 终端
crates/tunnel           控制 tailpocket-host 随附程序
crates/server           会话管理 + WebSocket 网关 + 提醒
crates/host             组装以上所有模块，对外提供 Host API
apps/cli                tether 命令行
apps/desktop            Tauri 电脑端外壳
apps/mobile-debug       React Native（Expo）调试 App
packages/client         TypeScript 客户端 SDK
fixtures/               录制的协议数据（各 agent 的 JSONL）
vendor/tailpocket/      TailPocket 发布文件（下载脚本 + SHA256，二进制不提交）
.ref/                   参考源码（只读，不提交）：boop、herdr-anywhere、tailpocket、codex-schema、acp
```

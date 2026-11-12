# T00 脚手架、契约骨架、预下载

**阶段 0，串行。** 本任务完成并合入 `main` 后，阶段 1 的所有任务才能并行开工。本任务**需要联网**。

## 目标

1. 建好 monorepo，所有 crate 和包按 `docs/CONTRACTS.md` 生成**可编译的骨架**。
2. 完整实现三个共享模块：`crates/proto`、`crates/agent-api`（含 `EchoAdapter`、`testkit`）、TS 类型生成。
3. 预先下载一切依赖和参考资料，使后续任务能在**无网络**环境下编译和测试。

## 归属路径

整个仓库。

## 现状

- 已有 `crates/proto`（早期草稿，基于 QUIC，需要按 `PROTOCOL.md` 重写）、`crates/link`（iroh 连接层，**删除**）、`crates/host` 与 `apps/cli`（空占位，重建）、根 `Cargo.toml`。
- 目录不是 git 仓库。

## 步骤

### 1. 仓库

- `git init`，主分支 `main`。`.gitignore`：`target/`、`node_modules/`、`.ref/`、`vendor/tailpocket/bin/`、`vendor/tailpocket/pkg/`、`*.db`、`.env`。
- 删除 `crates/link`。
- 新建 `NOTICE`，列出 `PLAN.md` "借用的开源代码"中的三个项目及其 MIT 许可。

### 2. Rust workspace

根 `Cargo.toml` 的成员：`crates/{proto,store,agent-api,adapter-codex,adapter-claude,adapter-acp,terminals,tunnel,server,host}`、`apps/cli`、`apps/desktop/src-tauri`。

在 `[workspace.dependencies]` 中**预先声明后续任务会用到的全部依赖**（后续任务离线，无法新增未下载的依赖）：
`tokio`（full）、`serde`、`serde_json`、`anyhow`、`thiserror`、`tracing`、`tracing-subscriber`、`uuid`（v4）、`rand`、`base64`、`sha2`、`subtle`、`rusqlite`（`bundled`）、`axum`（`ws`）、`tokio-tungstenite`、`futures`、`bytes`、`portable-pty`、`which`、`directories`、`qrcode`、`clap`（derive）、`ts-rs`、`toml`、`tempfile`、`hex`、`tauri`（v2）、`tauri-build`、`tauri-plugin-autostart`、`tauri-plugin-single-instance`、`keepawake`。
版本用 crates.io 上的最新稳定版。每个 crate 的 `Cargo.toml` 先把契约需要的依赖写好。

### 3. 共享模块（完整实现）

- **`crates/proto`**：按 `PROTOCOL.md` 第 2–7 节和 `CONTRACTS.md` 实现全部类型、`binary::encode/decode`、`now_ms`。所有类型派生 `ts_rs::TS` 并导出到 `packages/client/src/generated/`。单元测试：每种消息的 JSON 往返；`Event` 是扁平结构（`{"seq":..,"at":..,"e":"user_text","text":..}`）；二进制帧编解码，包括边界情况（空数据、255 字节 id、非法帧）。
- **`crates/agent-api`**：契约中的全部类型、`EchoAdapter`、`testkit::run`、`testkit::line_duplex`。测试：EchoAdapter 普通回合与审批回合的输出序列符合"适配器行为约定"。

### 4. 骨架（只写签名）

其余 crate 按 `CONTRACTS.md` 写出公开类型和函数，函数体 `todo!("Txx")`（写上负责的任务编号）。`apps/cli` 建立 `clap` 子命令 `serve`、`pair`、`devices`、`revoke`，内部调用 `host` 骨架。

### 5. TypeScript

- 根目录 `pnpm-workspace.yaml`，包含 `packages/*` 和 `apps/*`（这样 T11 新建的 `apps/mobile-debug` 会自动加入，不用再改这个文件）。
- `packages/client`：TypeScript + vitest + `ws`（测试用）；`src/index.ts` 按契约导出 `TetherClient` 骨架（方法抛出 `new Error("T08")`）。
- `apps/desktop`：Tauri 2 + Vite + React + TypeScript 模板（前端只放一个占位页）；`src-tauri` 加入 workspace。
- `apps/mobile-debug` 本任务**不创建**（T11 联网创建）。
- 运行 `pnpm install`。

### 6. 预下载

- `scripts/fetch-vendor.sh`：从 `github.com/jt-wang/tailpocket` 的 **v0.2.2** 发布下载 `SHA256SUMS` 以及以下文件：当前平台的 `tailpocket-host-*`、`tailpocket-phone-*`、`react-native-tailpocket.tgz`、`tailpocket-client.tgz`。逐个校验 SHA256，不符则失败退出。二进制放在 `vendor/tailpocket/bin/`，tgz 放在 `vendor/tailpocket/pkg/`。在 `vendor/tailpocket/VERSION` 记录版本号。
- `scripts/fetch-refs.sh`：浅克隆到 `.ref/`：`jt-wang/boop`、`jt-wang/herdr-anywhere`、`jt-wang/tailpocket`（v0.2.2 标签）、`agentclientprotocol/agent-client-protocol`；运行 `codex app-server generate-json-schema --out .ref/codex-schema`。
- `scripts/link-shared.sh`：在新 worktree 里把 `.ref`、`vendor/tailpocket/bin`、`vendor/tailpocket/pkg`、`node_modules` 链接到主仓库（Windows 下用 junction）。
- 运行 `cargo fetch` 和 `cargo build --workspace`（Tauri 首次编译较慢）。

### 7. CI 骨架

`.github/workflows/ci.yml`：在 ubuntu / macos / windows 上运行 `cargo fmt --check`、`cargo clippy --workspace -- -D warnings`（允许 `todo!`）、`cargo test --workspace`、`pnpm -r test`。

## 验收

```sh
cargo build --workspace --offline
cargo test -p tether-proto -p tether-agent-api --offline
cargo test -p tether-proto --offline export_bindings   # 生成 packages/client/src/generated/*.ts
pnpm -r exec tsc --noEmit
ls .ref/boop .ref/herdr-anywhere .ref/tailpocket .ref/agent-client-protocol .ref/codex-schema
ls vendor/tailpocket/bin vendor/tailpocket/pkg
```

最后在 `main` 上提交：`T00: scaffold and contracts`。

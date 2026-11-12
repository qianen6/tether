# Tether 开发计划：后端与通信（Codex 并行版）

> 产品名 Tether 为临时名。本阶段只做后端与通信；正式前端（视觉参考 Ben Journal 的 Live Tiles）以后再定。
> 每个任务文件（`docs/tasks/`）就是交给一个 Codex 会话的完整任务说明。

## 目标

本阶段完成时：

- 电脑端 `tether serve` 常驻运行，手机扫码配对后，在任意网络下都能经 TailPocket 隧道连上。
- 手机能列出电脑上的 agent，新建会话、发消息、看流式回复、处理审批、打断；也能打开终端直接操作任意命令行程序。
- 断线重连不丢消息；吊销设备立即生效。
- 前端只需调用 `packages/client`（TypeScript SDK）。

## 已定选型

| 层 | 选型 |
|---|---|
| 隧道 | TailPocket v0.2.x（Tailcat / WireGuard）；Tailcat 公共中继，可改自建 DERP |
| 协议 | 隧道内 WebSocket；JSON 文本帧 + 终端二进制帧（见 `PROTOCOL.md`） |
| 电脑端 | Rust（tokio、axum、rusqlite、portable-pty）；Tauri 2 外壳 |
| 手机端 | React Native + Expo（开发构建）；本阶段只做调试 App |
| 平台 | 电脑 Windows / macOS / Linux；手机 Android / iOS（鸿蒙暂不支持） |
| 测试范围 | 真实运行只测 **Codex**；Claude Code 只用录制数据测试 |

## 阶段与并行方式

```
阶段 0（串行，1 个 Codex）    T00 脚手架 + 契约骨架 + 预下载
                                   │
阶段 1（并行，最多 9 个 Codex）    ├─ T01 隧道 tunnel           ─┐
                                   ├─ T02 存储 store              │
                                   ├─ T03 服务 server（会话+网关）  │ 只依赖 T00 的契约骨架，
                                   ├─ T04 Codex 适配器             │ 互不依赖，可同时开工
                                   ├─ T05 Claude 适配器            │
                                   ├─ T06 ACP 适配器               │
                                   ├─ T07 终端 terminals           │
                                   ├─ T08 TS 客户端 SDK            │
                                   └─ T09 电脑端外壳（Tauri）      ─┘
                                   │
阶段 2（串行，1 个 Codex）    T10 整合：host 组装、agent 发现、CLI、端到端测试
                                   │
阶段 3（并行）                ├─ T11 手机调试 App（需真机，人工参与）
                                   └─ T12 打包与 CI 发布
```

**为什么能并行**：T00 先把所有 crate 和包按 `CONTRACTS.md` 建成可编译的骨架（函数体 `todo!()`）。阶段 1 的每个任务只实现自己归属的 crate，依赖别人的地方用骨架签名编译、用测试替身测试，所以互相不用等。

**资源有限时的优先顺序**：T02 → T03 → T04 → T01 → T07 → T08 → T06 → T05 → T09。前四个完成即可开始 T10 的主体工作。

## 任务一览

| 编号 | 任务 | 归属路径 | 依赖 | 真实环境测试 |
|---|---|---|---|---|
| T00 | 脚手架、契约骨架、预下载 | 全仓库 | — | 无 |
| T01 | 隧道：控制 tailpocket-host，含 Windows 验证 | `crates/tunnel` | T00 | 本机 tailpocket-host + tailpocket-phone |
| T02 | 存储：SQLite | `crates/store` | T00 | 无 |
| T03 | 服务：会话管理 + WebSocket 网关 + 提醒 | `crates/server` | T00 | 无（用 EchoAdapter） |
| T04 | Codex 适配器 | `crates/adapter-codex`、`fixtures/codex` | T00 | **本机真实 codex**（`#[ignore]`，人工运行） |
| T05 | Claude Code 适配器 | `crates/adapter-claude`、`fixtures/claude` | T00 | 无（只用录制数据） |
| T06 | ACP 适配器 | `crates/adapter-acp`、`fixtures/acp` | T00 | 无（用模拟 ACP agent） |
| T07 | 终端 | `crates/terminals` | T00 | 本机 PTY |
| T08 | TS 客户端 SDK | `packages/client` | T00 | 无（用模拟服务器） |
| T09 | 电脑端外壳（Tauri） | `apps/desktop` | T00 | 本机编译运行 |
| T10 | 整合 | `crates/host`、`apps/cli`、`e2e/`、契约文档 | T01–T08 | 本机隧道端到端 + 真实 codex |
| T11 | 手机调试 App | `apps/mobile-debug` | T08、T10 | **Android 真机** |
| T12 | 打包与发布 CI | `.github/`、`scripts/` | T09、T10 | GitHub Actions |

## 怎么让 Codex 并行跑

T00 完成并合入 `main` 后，每个任务开一个 git worktree 和分支，在里面启动一个 Codex：

```sh
# 在仓库根目录
git worktree add ../tether-T02 -b task/T02
git worktree add ../tether-T03 -b task/T03
git worktree add ../tether-T04 -b task/T04
# ……

# 每个 worktree 里各开一个终端（PowerShell）。先进入 conda 环境 tether，
# 这样 Codex 运行的 node / pnpm 都是 Node 22。
cd ../tether-T02
conda activate tether
codex "阅读 AGENTS.md，然后完成 docs/tasks/T02-store.md 中的任务。"
```

完整的命令：

```sh
git worktree add ../tether-T04 -b task/T04
(cd ../tether-T04 && sh scripts/link-shared.sh)     # 链接参考资料和 TailPocket，离线安装 JS 依赖
# ……Codex 完成任务、合并到 main 之后：
sh scripts/remove-worktree.sh ../tether-T04          # 安全删除，见下方说明
```

注意：

- `.ref/`、`vendor/tailpocket/bin/`、`target/`、`node_modules/` 不进 git。`link-shared.sh` 把前三个链接到主仓库，并离线安装 JS 依赖，所以 worktree 里不需要联网。
- **删除 worktree 一定要用 `scripts/remove-worktree.sh`**，不要直接 `rm -rf` 或 `git worktree remove`。worktree 里的 `.ref` 等是指向主仓库的链接，直接递归删除可能顺着链接删掉主仓库的文件。脚本会先只删链接本身，并在发现有链接指向外部时拒绝删除。
- 本机 Node 需要 22.12 以上，否则 vitest 跑不起来。项目用 conda 环境 `tether`（Node 22 + pnpm），不改动系统里的 Node 20。新电脑上创建：`conda create -n tether -c conda-forge "nodejs=22" pnpm`。
- Codex 默认沙箱没有网络。所有依赖由 T00 预先下载；每个 worktree 里 `cargo build --offline` 可用。
- 每个任务完成后，人工检查报告，确认验收命令通过，再合并到 `main`。
- 合并顺序：T02 → T07 → T03 → T04 → T05 → T06 → T01 → T08 → T09。各任务只改自己的目录，正常不会有冲突。
- 任务提交的 `docs/requests/Txx.md`（契约修改请求）由 T10 统一处理。

## 需要人工参与的环节

| 环节 | 内容 |
|---|---|
| T00 之前 | 确认本机能联网下载（T00 需要下载依赖和 TailPocket 发布文件） |
| T01 | 如果 Windows 上 tailpocket-host 运行失败，需要人工判断：修复，或改在 macOS/Linux 上开发、Windows 稍后处理 |
| T04、T10 | 手动运行 codex 真实测试：`cargo test -p tether-adapter-codex -- --ignored`（会调用你配置的 API 中转） |
| T11 | Android 真机，用**手机流量、不开 VPN**测试；需要 JDK 17 和 Android SDK，或者 Expo 账号（EAS 云构建） |
| T12 | GitHub 仓库与 Actions 权限 |

## 借用的开源代码

| 来源 | 协议 | 用途 |
|---|---|---|
| jt-wang/tailpocket | MIT | 隧道（随附程序、React Native SDK） |
| jt-wang/boop | MIT | 配对防爆破、harness 接口设计、DSH 适配器参考 |
| jt-wang/herdr-anywhere | MIT | 终端交互、吊销断开设计 |

所有借用之处在仓库 `NOTICE` 中注明。

## 本阶段之后

正式前端设计、DeepSeek Harness 适配器、签名自动更新、iOS 上架与推送、内置助手。

# Tether

用手机控制电脑上的编程 agent：Codex、Claude Code、DeepSeek Harness（DSH）、ACP agent，或者任意终端程序。
手机和电脑之间走加密隧道（[TailPocket](https://github.com/jt-wang/tailpocket)，基于 WireGuard），不需要公网 IP，也不用自己架服务器。

- 扫码配对，在任何网络下都能连上自己的电脑
- 在手机上新建对话、看流式回复、处理审批、随时打断；断线重连不丢消息
- 接着电脑上 Codex 桌面版里的对话继续聊；具体能力取决于已安装的 Codex 版本和对话状态
- 对接 DSH 0.1 / 0.2，支持会话、历史、工具调用和审批；也可以填写正在运行的 `dsh web` 地址
- 打开终端直接操作任意命令行程序
- 吊销设备立即生效

> 名字是临时的。目前是开发预览版，手机端仍是调试 App（`apps/mobile-debug`），不是经过完整跨设备验收的稳定版。

## 安装电脑端

不提供安装包：从源码编译，在后台以你自己的身份运行（登录后自动启动）。最省事的做法是让电脑上的 AI 编程助手照
[`DEPLOY.md`](DEPLOY.md) 装，例如对它说：

> 克隆 https://github.com/qianen6/tether ，按里面的 DEPLOY.md 把 Tether 装好并帮我配对手机。

也可以自己运行：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install.ps1   # Windows
```

```sh
sh scripts/install.sh                                          # macOS / Linux
```

需要 Rust 和 C 编译器，缺什么 [`DEPLOY.md`](DEPLOY.md) 里有安装命令。开着"智能应用控制"的 Windows 电脑目前装不了（它会拦截没有签名的程序）。

## 手机端

Android：到 [Releases](../../releases) 下载 `tether-android.apk`。iOS 不作为本阶段的发布承诺。

## 出问题时

同样交给电脑上的 AI：

> Tether 出问题了，按 DEPLOY.md 查一下。

它会按 [`DEPLOY.md`](DEPLOY.md) 的"出问题时"排查。环境和配置问题它直接处理；如果是 Tether 自己的 bug，它不改代码，
会整理一份报告给你。

## 当前状态与已知问题

- 已实现 DSH 0.2 的 token / cookie 登录和 `remote.mux` 适配，并保留 0.1 支持。Tether 启动的 DSH 进程树会随失败或退出清理。
- 部分 Windows 环境报告 `dsh web 没有打印地址就退出了`，或 `read codex stdout: invalid utf-8 ...`。触发条件仍在排查；尚未证实是某个版本导致，也未宣称这两类报错已经修复。
- 电脑端 Agent 列表中的路径检查主要验证程序可执行及版本输出，不代表完整的会话通信验收。
- 真实账号、模型和真机测试不是默认测试的一部分。录制数据和单元测试通过，不代表所有安装方式与设备组合都已验证。
- `scripts/install.sh`（macOS / Linux）还没有在真机上完整跑过。

## 组成

```
crates/proto            协议类型（ts-rs 生成 TypeScript）
crates/store            SQLite：设备、会话、事件
crates/agent-api        agent 适配器的公共接口
crates/adapter-codex    Codex（codex app-server）
crates/adapter-claude   Claude Code（stream-json）
crates/adapter-acp      ACP 通用客户端
crates/adapter-dsh      DeepSeek Harness（dsh web，0.1 / 0.2）
crates/terminals        PTY 终端
crates/tunnel           控制 tailpocket-host 随附程序
crates/server           会话管理 + WebSocket 网关 + 提醒
crates/host             组装以上模块
apps/cli                tether 命令行（安装脚本运行的就是它）
apps/desktop            Tauri 电脑端（目前不发布）
apps/mobile-debug       React Native（Expo）手机 App
packages/client         TypeScript 客户端 SDK
fixtures/               协议回放数据（合成或脱敏录制，各目录 README 说明来源）
```

协议见 [`docs/PROTOCOL.md`](docs/PROTOCOL.md)，各模块接口见 [`docs/CONTRACTS.md`](docs/CONTRACTS.md)。

## 构建与开发

开发规则见 [`AGENTS.md`](AGENTS.md)。需要：Rust（edition 2024，CI 固定 1.95.0）、Node 22.12 以上、pnpm 10.33.2，以及目标平台的 Tauri 构建依赖。agent 自身的运行时要求与 Tether 的构建环境分开；例如 DSH 的 Node 版本要求应以其安装包为准。

```sh
scripts/fetch-vendor.sh            # 先准备 TailPocket：pnpm 的本地依赖会引用 vendor/
pnpm install --frozen-lockfile
cargo fetch --locked

cargo test --workspace --offline   # 电脑端测试；真实环境测试默认忽略
pnpm -r test                       # SDK、电脑端界面、手机 App 的测试
```

手机 App 用 EAS 云构建，步骤见 [`apps/mobile-debug/README.md`](apps/mobile-debug/README.md)（先 `npx eas-cli init` 关联你自己的 Expo 项目）。

Windows 上运行 `.sh` 脚本请使用 Git Bash。本仓库不包含个人 Expo 项目绑定、账号凭据、配对数据库或开发机器上的安装备份。

## 发布

Releases 只放 Android App（`tether-android.apk`）、使用说明 PDF 和 `SHA256SUMS`；电脑端从源码安装，不发布安装包。
Android App 在 EAS 上构建（`npx eas-cli build -p android --profile preview`），再用 `gh release create` 上传。

`.github/workflows/release.yml` 保留为手动运行（Actions → release → Run workflow）：构建桌面版安装包和命令行版，作为这次运行的附件下载，不会发布。

## 许可

项目代码采用 MIT，见 [LICENSE](LICENSE)。第三方材料保留各自许可，来源见 [NOTICE](NOTICE)，随附许可原文见 [LICENSES](LICENSES/README.md)；字体许可保留在对应字体目录。

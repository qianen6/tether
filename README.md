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

## 当前状态与已知问题

- 已实现 DSH 0.2 的 token / cookie 登录和 `remote.mux` 适配，并保留 0.1 支持。Tether 启动的 DSH 进程树会随失败或退出清理。
- 部分 Windows 环境报告 `dsh web 没有打印地址就退出了`，或 `read codex stdout: invalid utf-8 ...`。触发条件仍在排查；尚未证实是某个版本导致，也未宣称这两类报错已经修复。
- 电脑端 Agent 列表中的路径检查主要验证程序可执行及版本输出，不代表完整的会话通信验收。
- 真实账号、模型和真机测试不是默认测试的一部分。录制数据和单元测试通过，不代表所有安装方式与设备组合都已验证。

## 下载

已发布的安装包见 [Releases](../../releases)，以对应发布页实际包含的文件为准。桌面端目标平台为 Windows / macOS / Linux；手机端目前主要验证 Android，iOS 不作为本阶段发布承诺。
安装包目前还没有代码签名，首次运行的提示和放行方法写在每个版本的说明里。

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
apps/cli                tether 命令行（无界面运行电脑端）
apps/desktop            Tauri 电脑端
apps/mobile-debug       React Native（Expo）手机 App
packages/client         TypeScript 客户端 SDK
fixtures/               协议回放数据（合成或脱敏录制，各目录 README 说明来源）
```

协议见 [`docs/PROTOCOL.md`](docs/PROTOCOL.md)，各模块接口见 [`docs/CONTRACTS.md`](docs/CONTRACTS.md)。

## 构建

需要：Rust（edition 2024，CI 固定 1.95.0）、Node 22.12 以上、pnpm 10.33.2，以及目标平台的 Tauri 构建依赖。agent 自身的运行时要求与 Tether 的构建环境分开；例如 DSH 的 Node 版本要求应以其安装包为准。

```sh
scripts/fetch-vendor.sh            # 先准备 TailPocket：pnpm 的本地依赖会引用 vendor/
pnpm install --frozen-lockfile
cargo fetch --locked

cargo test --workspace --offline   # 电脑端测试；真实环境测试默认忽略
pnpm -r test                       # SDK、电脑端界面、手机 App 的测试

scripts/package-desktop.sh         # 电脑端安装包（程序里不带本机路径）
```

手机 App 用 EAS 云构建，步骤见 [`apps/mobile-debug/README.md`](apps/mobile-debug/README.md)（先 `npx eas-cli init` 关联你自己的 Expo 项目）。

Windows 上运行 `.sh` 脚本请使用 Git Bash。本仓库不包含个人 Expo 项目绑定、账号凭据、配对数据库或开发机器上的安装备份。

## 发布

推送 `v*` 标签（如 `v0.1.0`）后，GitHub Actions 自动构建各平台安装包和命令行版，连同 `SHA256SUMS` 发布到 Releases（见 `.github/workflows/release.yml`）。
可选设置：变量 `WINDOWS_SIGN_COMMAND` 为 Windows 程序签名；变量 `EXPO_OWNER`、`EAS_PROJECT_ID` 和密钥 `EXPO_TOKEN` 同时构建 Android App。

## 许可

项目代码采用 MIT，见 [LICENSE](LICENSE)。第三方材料保留各自许可，来源见 [NOTICE](NOTICE)，随附许可原文见 [LICENSES](LICENSES/README.md)；字体许可保留在对应字体目录。

# Tether

用手机控制电脑上的编程 agent：Codex、Claude Code、任何 ACP agent，或者任意终端程序。
手机和电脑之间走加密隧道（[TailPocket](https://github.com/jt-wang/tailpocket)，基于 WireGuard），不需要公网 IP，也不用自己架服务器。

- 扫码配对，在任何网络下都能连上自己的电脑
- 在手机上新建对话、看流式回复、处理审批、随时打断；断线重连不丢消息
- 接着电脑上 Codex 桌面版里的对话继续聊，项目、模型、推理强度、速度、权限都跟电脑端一致
- 打开终端直接操作任意命令行程序
- 吊销设备立即生效

> 名字是临时的。手机端目前是功能完整的调试 App（`apps/mobile-debug`）。

## 组成

```
crates/proto            协议类型（ts-rs 生成 TypeScript）
crates/store            SQLite：设备、会话、事件
crates/agent-api        agent 适配器的公共接口
crates/adapter-codex    Codex（codex app-server）
crates/adapter-claude   Claude Code（stream-json）
crates/adapter-acp      ACP 通用客户端
crates/terminals        PTY 终端
crates/tunnel           控制 tailpocket-host 随附程序
crates/server           会话管理 + WebSocket 网关 + 提醒
crates/host             组装以上模块
apps/cli                tether 命令行（无界面运行电脑端）
apps/desktop            Tauri 电脑端
apps/mobile-debug       React Native（Expo）手机 App
packages/client         TypeScript 客户端 SDK
fixtures/               各 agent 的协议回放数据（测试用，合成）
```

协议见 [`docs/PROTOCOL.md`](docs/PROTOCOL.md)，各模块接口见 [`docs/CONTRACTS.md`](docs/CONTRACTS.md)。

## 构建

需要：Rust（edition 2024）、Node 22.12 以上、pnpm。

```sh
pnpm install
scripts/fetch-vendor.sh            # 下载并校验 TailPocket 发布文件（vendor/ 不进 git）

cargo test --workspace             # 电脑端测试
pnpm -r test                       # SDK、电脑端界面、手机 App 的测试

scripts/package-desktop.sh         # 电脑端安装包（程序里不带本机路径）
```

手机 App 用 EAS 云构建，步骤见 [`apps/mobile-debug/README.md`](apps/mobile-debug/README.md)（先 `npx eas-cli init` 关联你自己的 Expo 项目）。

## 许可

MIT，见 [LICENSE](LICENSE)。用到和改编的第三方项目见 [NOTICE](NOTICE)。

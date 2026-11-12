# T09 电脑端外壳（Tauri）

**阶段 1。归属路径：`apps/desktop`。** 只做功能，不做视觉设计（正式界面以后再定）。

## 目标

一个能常驻运行的 Tauri 2 桌面 App，内部调用 `tether-host`。`host` 此时还是骨架（T10 才实现），所以本任务先把外壳和界面接线做好，用一个**假的 Host 实现**开发和测试。

## 要求

1. **Rust 部分**（`apps/desktop/src-tauri`）：
   - 启动时调用 `Host::start`，退出时调用 `shutdown`。用 cargo feature `fake-host` 切换到假实现（返回固定的二维码、两台设备、几个 agent），开发和测试时开启。
   - Tauri 命令：`pair()`、`devices()`、`revoke(id)`、`agents()`、`status()`，对应 Host API。
   - 系统托盘：显示状态（"有 N 个会话在等你"），菜单项：打开窗口、配对新手机、退出。关闭窗口只隐藏到托盘，不退出。
   - 开机自启（`tauri-plugin-autostart`，设置里可以关闭）；只允许运行一个实例（`tauri-plugin-single-instance`）；可选"防止电脑休眠"（`keepawake`）。
   - 把 `vendor/tailpocket/bin/` 里当前平台的 `tailpocket-host` 配置成 Tauri 随附程序（`externalBin`）。
2. **前端**（React + TS，朴素样式即可）：
   - 首页：状态，加"配对新手机"按钮。点击后显示二维码（用 `qrcode` 库在前端渲染 `PairInfo.qr`）和倒计时。
   - 设备列表：名称、配对时间、最后在线，加"移除"按钮（需要确认）。
   - Agent 列表：名称、是否可用、不可用的原因。
   - 设置：开机自启、防止休眠。

## 测试

- Rust：`fake-host` 下的命令函数单元测试。
- 前端：vitest 加 Testing Library，测二维码显示和设备移除的流程。
- 手动：`pnpm --filter desktop tauri dev --features fake-host` 能启动，托盘和窗口正常。

## 验收

```sh
cargo test -p tether-desktop --features fake-host --offline
pnpm --filter desktop test
pnpm --filter desktop exec tsc --noEmit
```

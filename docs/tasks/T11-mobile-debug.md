# T11 手机调试 App

**阶段 3。归属路径：`apps/mobile-debug`。前置：T08、T10。需要联网，需要人工用真机测试。**

## 目标

一个**纯功能**的 React Native 调试 App，在真实手机上验证整套后端和通信。不做视觉设计，正式界面以后另外做。

## 要求

1. 用 Expo 创建项目（开发构建模式，不能用 Expo Go，因为 `react-native-tailpocket` 是原生模块）。依赖：`vendor/tailpocket/pkg/react-native-tailpocket.tgz`、`@tether/client`（workspace 引用）、一个扫码库（`expo-camera`）。`apps/*` 已在 pnpm workspace 中，不需要改 `pnpm-workspace.yaml`。
2. 页面：
   - **配对**：扫码 → 用 TailPocket 配对（前缀 `tether1:`）→ 用拿到的 `payload` 作为 `pairSecret` 连接 → 保存 TailPocket 的 Link 和设备令牌（`expo-secure-store`）。
   - **会话列表**：所有会话的状态（等你回复的排在最前），加新建会话（选 agent 和工作目录）。
   - **Chat**：流式回复、工具调用（可折叠）、审批卡片（按 `choices` 显示按钮）、打断按钮、输入框。
   - **终端**：WebView 加 xterm.js；按键栏（Esc、Tab、Ctrl、方向键、粘贴）。参考 `.ref/herdr-anywhere/app/` 的终端实现。
   - **状态**：连接状态、直连还是中继（TailPocket 的 `path()`）、电脑名。
3. Android：前台服务保持隧道连接（`foregroundServiceType="specialUse"`，参考 `.ref/herdr-anywhere/app/android/app/src/main/AndroidManifest.xml`），收到 `notify` 时发系统通知，点通知跳到对应会话。
4. iOS：用 EAS 云构建出模拟器版本（不需要 Mac）。只在 App 打开时提醒。

## 构建

- Android 本地构建：需要 JDK 17 和 Android SDK（`npx expo run:android`）。或者用 EAS 云构建：`eas build -p android --profile development`（需要 Expo 账号）。
- iOS：`eas build -p ios --profile development-simulator`。

## 人工验收（真机）

用 Android 真机，**关掉 Wi-Fi、用手机流量、不开 VPN**：

1. 电脑 `tether serve` + `tether pair`，手机扫码配对成功。
2. 列出 agent，新建 codex 会话，完成一次带命令审批的任务。
3. 打开终端，运行 PowerShell 命令。
4. 把 App 切到后台，在电脑上触发一次审批，手机收到通知。
5. 手机开关飞行模式后自动重连，消息不丢。
6. 在电脑上移除这台手机，手机立即断开。

在报告中记录每一项的结果、配对耗时、走直连还是中继。

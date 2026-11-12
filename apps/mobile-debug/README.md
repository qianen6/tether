# mobile-debug — Tether 手机调试 App

纯功能的 Expo（React Native）App，用来在真机上验证后端与通信。正式界面以后另做。

## 功能

- 配对：TailPocket 自带扫码界面（前缀 `tether1:`）→ 加入隧道 → 用二维码里的配对密钥连接网关。隧道 Link 保存在 Android Keystore（TailPocket），设备令牌保存在 `expo-secure-store`。
- 会话：列表（等你处理的排最前）、新建（选 agent 和工作目录）、Chat（流式回复、工具调用可展开、审批卡片、打断、关闭）。
- 终端：WebView + xterm.js（打包进 App，不走 CDN）；按键栏 Esc / Tab / Ctrl+C/D/Z/L / 方向键 / 粘贴。
- 状态：电脑名、连接状态、配对耗时、移除配对。直连/中继：TailPocket 0.2 客户端没有 `path()`，暂不显示。
- 后台：连接期间 Android 前台服务（`modules/tether-keepalive`，`specialUse`）保持隧道；收到 `notify` 发系统通知，点通知打开对应会话。

## 构建（EAS 云构建）

```sh
npx eas-cli login                 # 一次
cd apps/mobile-debug
npx eas-cli init                  # 一次：创建 Expo 项目，写入 app.json 的 projectId
npx eas-cli build -p android --profile preview    # 出 APK，扫页面上的二维码下载安装
```

EAS 在构建机上运行 `eas-build-pre-install`（`scripts/fetch-vendor.sh mobile`），下载并校验 TailPocket 的 RN 包和 Android Maven 库（`vendor/` 不进 git）。

iOS 模拟器版：`npx eas-cli build -p ios --profile development-simulator`。

本地 `expo prebuild` 在含中文的路径（如 `C:\项目\…`）下会失败（Expo 模板解压的问题）；EAS 不受影响。本地构建需放到纯英文路径，并装 JDK 17 + Android NDK。

## 测试

```sh
pnpm --filter mobile-debug test        # 纯逻辑：聊天事件归并、会话排序、base64
pnpm --filter mobile-debug typecheck
```

真机验收步骤见 `docs/tasks/T11-mobile-debug.md`。

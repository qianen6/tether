# T12 打包与发布 CI

**阶段 3。归属路径：`.github/`、`scripts/release-*`。前置：T09、T10。**

## 目标

推送 `v*` 标签后，由 GitHub Actions 自动构建所有平台的安装包，发布到 GitHub Releases。

## 要求

1. **电脑端**（`tauri-action`）：
   - Windows x64：`.msi` 和 `.exe`（NSIS）
   - macOS arm64 和 x64：`.dmg`
   - Linux x64：`.AppImage` 和 `.deb`
   - 构建前运行 `scripts/fetch-vendor.sh`，下载对应平台的 `tailpocket-host` 并校验 SHA256，作为随附程序打包。
   - 同时发布命令行版 `tether`（Windows / macOS / Linux 各一个压缩包），给不需要界面的用户（例如 Linux 服务器）。
2. **手机端**：Android 调试 APK 用 EAS 构建（需要仓库密钥 `EXPO_TOKEN`），产物附到同一个 Release。iOS 本阶段不发布。
3. **签名**：本阶段不签名，在 Release 说明里写明首次运行的提示（Windows SmartScreen，"仍要运行"；macOS 系统设置 → 隐私与安全性 → "仍要打开"）。预留以后加签名的环境变量。
4. Release 说明自动列出：版本、各平台下载链接、`SHA256SUMS`。

## 验收

- 在 fork 或测试仓库推送 `v0.0.1-test` 标签，所有任务都成功，Release 页面包含全部文件。
- 下载 Windows 安装包，在一台干净的 Windows 电脑上安装并运行，能显示配对二维码（人工）。

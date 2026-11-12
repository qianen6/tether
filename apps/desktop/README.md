# T09 桌面外壳

只做功能，沿用系统字体、普通按钮和简单列表；本任务不引入视觉设计系统或 DESIGN.md。

## 离线开发

在仓库根目录的 PowerShell 中：

```powershell
conda activate tether
$env:CARGO_NET_OFFLINE = "true"
pnpm --filter desktop tauri dev --features fake-host
```

`fake-host` 只替换桌面内部的 Host 实现：二维码内容固定，过期时间为本次请求后十分钟；两台设备、三个 agent 以及会话状态均为演示数据。设备移除会更新本进程的内存状态，重启恢复。它不启动 agent、网关或隧道，也不调用 Host 骨架中的 `todo!()`。

不启用此 feature 时，同一组命令委托给 `tether-host::Host`，供 T10 接入。当前阶段仅编译检查真实 Host 路径，不运行它。

## 行为

- `pair`、`devices`、`revoke(id)`、`agents`、`status` 对应 Host API；私有 IPC DTO 只做可序列化映射。AgentInfo 在前端直接引用 client 生成的类型。
- 状态、设备、agent 每两秒刷新；请求失败显示错误，保留已有数据。窗口关闭仅隐藏；托盘可打开窗口、生成配对二维码、退出。第二实例会唤起第一实例。
- 托盘显示“有 N 个会话在等你”。托盘生成的二维码通过事件传到前端，并保留最近的有效配对，防止前端尚未订阅时丢失。
- 配对倒计时使用 Host 给出的 Unix 毫秒截止时间；过期即移除二维码。设备移除先确认，可取消、按 Esc 关闭；失败保留设备并允许重试。
- 开机自启仅在首次启动时默认启用；之后以 autostart 插件/操作系统状态为准，不覆盖用户关闭的选择。设置文件位于 Tauri app data 目录下的 `desktop-settings.json`。
- 防休眠默认关闭，用户选择持久化；启用只防止空闲系统休眠，不强制屏幕常亮。`keepawake` guard 在专用线程创建和释放，兼容 Windows 的线程级 execution state。退出会释放 guard，并等待 Host shutdown 完成。

## TailPocket 随附程序

`bundle.externalBin = ["binaries/tailpocket-host"]`。`build.rs` 从已经下载的 `vendor/tailpocket/bin/tailpocket-host-<os>-<arch>[.exe]` 复制到被忽略的 staging 目录：

```text
apps/desktop/src-tauri/binaries/tailpocket-host-x86_64-pc-windows-msvc.exe
```

这符合 Tauri 的 `-<target-triple>` 文件名规则，Tauri 再把它放到可执行文件旁。构建不会下载文件，也不修改 vendor。跨平台打包时，需预先准备对应平台的 vendor 文件；源文件缺失会使构建失败。

## 验收

```powershell
cargo test -p tether-desktop --features fake-host --offline
pnpm --filter desktop test
pnpm --filter desktop exec tsc --noEmit
cargo fmt -p tether-desktop --check
cargo clippy -p tether-desktop --features fake-host --offline --no-deps -- -D warnings
cargo check -p tether-desktop --offline
pnpm --filter desktop build
```

Rust 测试涵盖实际 Tauri 命令函数、假 Host 状态变化和 shutdown、设置存储、首次自启初始化、防休眠线程和失败恢复、随附程序命名及逐字节复制。前端测试涵盖二维码、截止时间、托盘事件竞态、移除确认/取消/重试、设置与 IPC 参数。测试不需要账号、模型或真实 agent。

Windows MSVC 的应用与测试可执行文件统一通过链接器嵌入 Common Controls v6 manifest，避免 MockRuntime 测试加载 `TaskDialogIndirect` 时出现 `STATUS_ENTRYPOINT_NOT_FOUND`；Tauri 不再重复生成二进制专用的 manifest 资源。

GUI 检查：运行上面的 dev 命令；点击配对查看二维码和倒计时；移除时先取消；关闭窗口后确认进程仍在，托盘可重新打开；再次启动确认只有一个实例；从托盘配对；切换设置；最后使用托盘“退出”。

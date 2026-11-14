# 部署 Tether 电脑端（给帮用户安装的 AI 看）

Tether 让手机控制这台电脑上的编程 agent（Codex、Claude Code、DeepSeek Harness、其他 ACP agent、终端）。
电脑端不提供安装包：从这个仓库的源码编译命令行版 `tether`，在后台以当前用户身份运行。

你的任务是**照下面的步骤把它装好并让用户的手机配对成功**；装好以后用户说"Tether 出问题了"，照
[出问题时](#出问题时) 排查。只做部署和排错，不要修改仓库里的代码。

## 规则

1. **不要改代码，不要改 `Cargo.lock`**。编译失败、或者查下来是 Tether 自己的 bug，就把错误原样告诉用户、
   写成报告（见"出问题时"第 5 步），不要自己"修"。环境和配置的问题（找不到程序、自动运行、重新安装）可以直接处理。
2. **不要注册成 Windows 服务 / 系统级服务，也不要用管理员身份运行 tether。** 它必须以用户自己的身份运行，
   才能读到用户的 Codex、Claude、DSH 的登录信息和配置。安装脚本用的是"登录时运行"（Windows 计划任务、
   macOS launchd 用户代理、Linux systemd 用户服务），照用即可。
3. **安装 Rust 或 C++ 编译工具之前先问用户。** Windows 上的 C++ 工具有几个 GB。
4. **不要关闭任何安全功能**（Windows 智能应用控制、Defender、SmartScreen、macOS Gatekeeper）。被拦就停下来告诉用户，见"常见问题"。
5. 隧道程序 `tailpocket-host` 由安装脚本下载，并用仓库里提交的 `vendor/tailpocket/SHA256SUMS` 核对。
   校验不通过时脚本会停下，**不要绕过校验**。

## 步骤

### 1. 拿到源码

```sh
git clone https://github.com/qianen6/tether.git tether
cd tether
```

已经有源码就 `git pull`。放在一个长期保留的目录里：运行的就是这个目录里编译出来的程序，删了就不能用了。

### 2. 准备编译工具（缺什么装什么，装之前先问用户）

| 系统 | 需要 | 安装命令 |
|---|---|---|
| Windows | Rust | `winget install --id Rustlang.Rustup -e` |
| Windows | C++ 编译工具 | `winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"` |
| macOS | Rust | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| macOS | C 编译器 | `xcode-select --install` |
| Linux | Rust | 同 macOS |
| Linux | C 编译器 | `sudo apt install build-essential`（其他发行版用对应的包管理器） |

装完 Rust 后要**新开一个终端**，`cargo` 才在 PATH 里。

### 3. 运行安装脚本

Windows（PowerShell）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
```

macOS / Linux：

```sh
sh scripts/install.sh
```

脚本会：下载并核对隧道程序 → 编译（第一次几分钟）→ 设置登录时自动运行并立即在后台启动 →
列出各个 agent 是否可用 → 在浏览器里打开配对二维码。可以重复运行，升级就是 `git pull` 后再运行一次。

Windows 上如果用户正在运行 Tether 桌面版，脚本会停下并说明：两者用同一个数据目录，要先在托盘里退出桌面版。

### 4. 配对手机

1. 手机上安装 Tether App（Android：本仓库 GitHub Release 页里的 `tether-android.apk`）。
2. 用 App 扫描浏览器里打开的二维码。二维码 10 分钟内有效，只能配对一台手机。
3. 过期或要配对更多手机：`target/release/tether pair --open`（Windows：`target\release\tether.exe pair --open`）。

### 5. 检查

```sh
target/release/tether agents     # 每个 agent 是否可用；不可用的会写原因
target/release/tether devices    # 已配对的手机
```

把 `agents` 的结果告诉用户。用户要用的 agent 显示"未找到"，但其实装了，就指定它的位置：

```sh
target/release/tether locate codex "C:\Users\<用户>\AppData\Roaming\npm\codex.cmd"   # 程序本身或它所在的文件夹
target/release/tether locate dsh --clear                                         # 改回自动查找
```

`locate` 会先运行一次 `--version` 检查，通过才保存。

## 命令一览

| 命令 | 作用 |
|---|---|
| `tether serve` | 在前台运行（安装脚本已经让它在后台运行，平时不用手动执行） |
| `tether pair [--open] [--minutes N]` | 显示配对二维码；`--open` 同时在浏览器里打开 |
| `tether devices` / `tether revoke <id>` | 列出 / 移除已配对的手机 |
| `tether agents` | 列出 agent 及是否可用 |
| `tether locate <agent> <位置>` / `--clear` | 指定 / 取消 agent 的程序位置 |

除 `serve` 外的命令都要求 `tether serve` 正在运行。数据目录：Windows `%APPDATA%\Tether`，macOS
`~/Library/Application Support/Tether`，Linux `~/.local/share/tether`；日志是其中的 `tether.log`。

## 常见问题

- **Windows 提示"智能应用控制已阻止"**：这台电脑开着智能应用控制（Smart App Control），它会拦截没有签名的程序，
  包括本机编译的 `tether.exe` 和下载的 `tailpocket-host`。这种电脑上目前装不了。**不要替用户关掉它**：关掉后不重装系统就打不开了。
  如实告诉用户，由用户决定。
- **`tether agents` 里 DeepSeek Harness（DSH）可用，但手机上说"还没有可用的模型"**：Tether 运行的是 `dsh web`，
  用的是 DSH 的 web 配置，和 DSH 桌面版的模型设置是分开的。让用户运行 `dsh web`，在网页的「模型」页里配好模型。
- **想让手机接着电脑上正在开着的 DSH 对话**：把那个 `dsh web` 启动时打印的整行（`dsh web: http://127.0.0.1:端口/?token=…`）
  交给 `tether locate dsh "<整行>"`。DSH 重启后 token 会变，到时 Tether 会自己启动一个，需要的话再填一次。
- **手机连不上**：确认 `tether devices` 里有这台手机；看 `tether.log` 里 `tunnel` 相关的报错。电脑和手机都要能上网（不需要在同一个局域网）。
- **macOS / Linux 自动运行后找不到某个 agent**：自动运行时的 PATH 是安装时终端里的 PATH。
  装了新 agent 后重新运行一次安装脚本，或用 `tether locate` 指定位置。

## 出问题时

用户只要对你说一句"Tether 出问题了，按 DEPLOY.md 查一下"。按下面的顺序查，查到原因就处理，处理完再跑一次
`tether agents` 确认。命令都在源码目录里运行（Windows 是 `target\release\tether.exe`）。

1. **它在运行吗**：运行 `tether agents`。报 `tether serve is not running` 就是没在运行，启动它：
   - Windows：`Start-ScheduledTask -TaskName Tether`
   - macOS：`launchctl kickstart -k gui/$(id -u)/dev.tether.serve`
   - Linux：`systemctl --user restart tether.service`
   - 或者直接重新运行安装脚本。

   还是起不来，看数据目录里 `tether.log` 的最后几十行。
2. **某个 agent 用不了**：`tether agents` 里那一行写了原因。
   - "未找到命令 X"：找到它装在哪（Windows `where X`，macOS / Linux `which X`；npm 装的在 `npm prefix -g`
     下），再 `tether locate X <位置>`。
   - DSH 可用，但手机上说没有模型：见"常见问题"，请用户在 `dsh web` 的「模型」页配置。
3. **手机连不上**：
   - `tether devices` 里没有这台手机，就 `tether pair --open` 重新配对；
   - 看 `tether.log` 里含 `tunnel` 的报错；
   - 确认电脑能上网。
4. **更新代码后出问题、或者源码目录被挪动过**：重新运行安装脚本。它会停掉旧的、重新编译、重新设置自动运行。
5. **还是不行，或者是 Tether 自己的错**（编译失败、程序崩溃、日志里的报错指向 Tether 的代码）：停下来，给用户一份报告：
   - 现象和怎么复现；
   - `tether agents` 的输出；
   - `tether.log` 里相关的几十行（**先去掉 token、`?token=` 后面的内容等敏感信息**）；
   - 系统、`git rev-parse --short HEAD`。

   用户可以把报告发给维护者。不要改代码；用户明确要你改，那就是开发任务，按 [`AGENTS.md`](AGENTS.md) 做，
   并提醒用户：改过的代码以后 `git pull` 可能冲突。

不要做的：

- 不关安全功能（规则 4）。
- 不替用户在 Codex、Claude、DSH 里登录或填写 API key，只告诉用户去哪里填。
- 重启 Tether 会断开手机上所有正在进行的会话。如果你自己就是用户从手机上通过 Tether 打开的会话，重启前先告诉用户：
  重启后这个会话会断开，要回到电脑前或重新打开。

## 卸载

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install.ps1 -Uninstall   # Windows
```

```sh
sh scripts/install.sh --uninstall                                          # macOS / Linux
```

会停止 Tether 并删除自动运行。配对过的手机和设置留在数据目录里，不需要的话连同源码目录一起删除。

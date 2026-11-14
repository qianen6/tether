Tether {{VERSION}}：用手机通过加密隧道控制电脑上的编程 agent（Codex、Claude Code、DeepSeek Harness、ACP agent、终端）。开发预览版。

## 电脑端：从源码安装

不提供安装包。对电脑上的 Codex 或 Claude Code 说：

> 克隆 https://github.com/qianen6/tether ，按里面的 DEPLOY.md 把 Tether 装好并帮我配对手机。

它会检查编译工具（缺什么先问你）、下载并核对隧道程序、编译、设置登录后自动运行，最后在浏览器里打开配对二维码。
也可以自己运行 `scripts/install.ps1`（Windows）或 `scripts/install.sh`（macOS / Linux），见 [DEPLOY.md](https://github.com/qianen6/tether/blob/{{VERSION}}/DEPLOY.md)。

开着"智能应用控制"的 Windows 电脑目前装不了：它会拦截没有签名的程序。

## 手机端

| 文件 | 说明 |
|---|---|
| `tether-android.apk` | Android App。没有上架应用商店，安装时需要允许"安装未知应用"。 |
| `Tether使用说明.pdf` | 17 页使用说明：安装、配对、审批、提醒、终端、出问题时怎么办。 |
| `SHA256SUMS` | 以上文件的校验值。 |

## 出问题时

对电脑上的 AI 说："Tether 出问题了，按 DEPLOY.md 查一下。"

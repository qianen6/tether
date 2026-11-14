#!/bin/sh
# 在 macOS / Linux 上从源码安装 Tether 的电脑端服务：检查编译工具；下载 TailPocket
# 隧道程序并核对仓库里记录的 SHA-256；编译 tether（命令行版）；登录后自动运行
# （macOS 用 launchd，Linux 用 systemd 用户服务）并立即启动；列出能用的 agent；
# 在浏览器里打开配对二维码。可以重复运行：git pull 后再运行一次就是升级。
#
#   scripts/install.sh                  安装并配对
#   scripts/install.sh --no-pair        不打开配对二维码
#   scripts/install.sh --no-autostart   不设置自动运行，只启动这一次
#   scripts/install.sh --data-dir DIR   用另一个数据目录
#   scripts/install.sh --uninstall      停止并删除自动运行（配对和设置保留）
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXE="$ROOT/target/release/tether"
LABEL="dev.tether.serve"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT="$UNIT_DIR/tether.service"

AUTOSTART=1
PAIR=1
UNINSTALL=0
DATA_DIR=""
while [ $# -gt 0 ]; do
  case "$1" in
    --no-autostart) AUTOSTART=0 ;;
    --no-pair) PAIR=0 ;;
    --uninstall) UNINSTALL=1 ;;
    --data-dir) shift; DATA_DIR="$1" ;;
    *) echo "不认识的参数：$1" >&2; exit 2 ;;
  esac
  shift
done

step() { printf '\033[36m==> %s\033[0m\n' "$1"; }
tether() {
  if [ -n "$DATA_DIR" ]; then "$EXE" --data-dir "$DATA_DIR" "$@"; else "$EXE" "$@"; fi
}

stop_tether() {
  case "$(uname -s)" in
    Darwin) launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true ;;
    Linux) systemctl --user stop tether.service 2>/dev/null || true ;;
  esac
  # 隧道程序会随它一起退出。
  pkill -f "^$EXE( |$)" 2>/dev/null || true
}

if [ "$UNINSTALL" = 1 ]; then
  step "停止 Tether 并删除自动运行"
  stop_tether
  case "$(uname -s)" in
    Darwin) rm -f "$PLIST" ;;
    Linux)
      systemctl --user disable tether.service 2>/dev/null || true
      rm -f "$UNIT"
      systemctl --user daemon-reload 2>/dev/null || true
      ;;
  esac
  echo "已卸载。配对过的手机和设置还在数据目录里；源码目录可以直接删除。"
  exit 0
fi

step "检查编译工具"
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "没有找到 Rust（cargo）。先安装：curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh ，再运行本脚本。" >&2
  exit 1
fi
if ! command -v cc >/dev/null 2>&1; then
  case "$(uname -s)" in
    Darwin) echo "没有找到 C 编译器。先运行：xcode-select --install ，再运行本脚本。" >&2 ;;
    *) echo "没有找到 C 编译器。先安装（Debian/Ubuntu）：sudo apt install build-essential ，再运行本脚本。" >&2 ;;
  esac
  exit 1
fi

step "下载 TailPocket 隧道程序"
# 版本和 scripts/fetch-vendor.sh 保持一致；校验值用仓库里提交的，不用现下载的。
VERSION=$(sed -n 's/.*TAILPOCKET_VERSION:-\(v[0-9.]*\).*/\1/p' "$ROOT/scripts/fetch-vendor.sh" | head -n 1)
[ -n "$VERSION" ] || { echo "scripts/fetch-vendor.sh 里没有找到 TailPocket 的版本" >&2; exit 1; }
case "$(uname -s)" in Darwin) os=darwin ;; Linux) os=linux ;; *) echo "不支持的系统：$(uname -s)" >&2; exit 1 ;; esac
case "$(uname -m)" in
  x86_64 | amd64) arch=amd64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) echo "不支持的处理器：$(uname -m)" >&2; exit 1 ;;
esac
FILE="tailpocket-host-$os-$arch"
WANT=$(awk -v f="$FILE" '{ n = $2; sub(/^\*/, "", n); if (n == f) print $1 }' "$ROOT/vendor/tailpocket/SHA256SUMS")
[ -n "$WANT" ] || { echo "vendor/tailpocket/SHA256SUMS 里没有 $FILE" >&2; exit 1; }
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
BIN="$ROOT/vendor/tailpocket/bin"
DEST="$BIN/$FILE"
if [ -f "$DEST" ] && [ "$(sha256 "$DEST")" = "$WANT" ]; then
  echo "已有 $FILE（$VERSION，校验通过）"
else
  mkdir -p "$BIN"
  curl -fsSL --retry 2 "https://github.com/jt-wang/tailpocket/releases/download/$VERSION/$FILE" -o "$DEST.part"
  GOT=$(sha256 "$DEST.part")
  if [ "$GOT" != "$WANT" ]; then
    rm -f "$DEST.part"
    echo "下载的 $FILE 校验不对：应为 $WANT，实际 $GOT。没有使用它。" >&2
    exit 1
  fi
  mv "$DEST.part" "$DEST"
  chmod +x "$DEST"
  echo "已下载 $FILE（$VERSION，校验通过）"
fi

step "停止正在运行的 Tether（如果有）"
stop_tether

step "编译 tether（第一次要几分钟）"
(cd "$ROOT" && cargo build --release --locked -p tether)

# 自动运行时的环境很干净：把现在的 PATH 带过去，否则找不到 codex、claude 等命令。
xml() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'; }

if [ "$AUTOSTART" = 0 ]; then
  step "在后台启动 Tether"
  nohup "$EXE" ${DATA_DIR:+--data-dir "$DATA_DIR"} serve >/dev/null 2>&1 &
else
  case "$(uname -s)" in
    Darwin)
      step "设置登录后自动运行（launchd：$LABEL）并启动"
      mkdir -p "$(dirname "$PLIST")"
      {
        echo '<?xml version="1.0" encoding="UTF-8"?>'
        echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
        echo '<plist version="1.0"><dict>'
        echo "  <key>Label</key><string>$LABEL</string>"
        echo '  <key>ProgramArguments</key><array>'
        echo "    <string>$(xml "$EXE")</string>"
        if [ -n "$DATA_DIR" ]; then
          echo "    <string>--data-dir</string><string>$(xml "$DATA_DIR")</string>"
        fi
        echo '    <string>serve</string>'
        echo '  </array>'
        echo "  <key>EnvironmentVariables</key><dict><key>PATH</key><string>$(xml "$PATH")</string></dict>"
        echo '  <key>RunAtLoad</key><true/>'
        echo '  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>'
        echo '</dict></plist>'
      } > "$PLIST"
      launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
      launchctl bootstrap "gui/$(id -u)" "$PLIST"
      ;;
    Linux)
      step "设置登录后自动运行（systemd 用户服务 tether）并启动"
      if ! command -v systemctl >/dev/null 2>&1; then
        echo "没有 systemd：只启动这一次，开机后请自己运行 $EXE serve" >&2
        nohup "$EXE" ${DATA_DIR:+--data-dir "$DATA_DIR"} serve >/dev/null 2>&1 &
      else
        mkdir -p "$UNIT_DIR"
        {
          echo '[Unit]'
          echo 'Description=Tether: control the coding agents on this computer from your phone'
          echo ''
          echo '[Service]'
          if [ -n "$DATA_DIR" ]; then
            echo "ExecStart=\"$EXE\" --data-dir \"$DATA_DIR\" serve"
          else
            echo "ExecStart=\"$EXE\" serve"
          fi
          echo "Environment=\"PATH=$PATH\""
          echo 'Restart=on-failure'
          echo 'RestartSec=5'
          echo ''
          echo '[Install]'
          echo 'WantedBy=default.target'
        } > "$UNIT"
        systemctl --user daemon-reload
        systemctl --user enable --now tether.service
      fi
      ;;
  esac
fi

step "等待 Tether 就绪"
i=0
until tether agents >/dev/null 2>&1; do
  i=$((i + 1))
  if [ "$i" -ge 60 ]; then
    echo "Tether 60 秒内没有就绪。看数据目录里的 tether.log。" >&2
    exit 1
  fi
  sleep 1
done
tether agents

if [ "$PAIR" = 1 ]; then
  step "配对手机"
  tether pair --open || echo "配对二维码没有打开，稍后运行 $EXE pair --open" >&2
fi

echo ""
echo "Tether 已在后台运行。常用命令："
echo "  $EXE pair --open      再配对一台手机"
echo "  $EXE agents           看哪些 agent 可用"
echo "  $EXE locate <agent> <位置>   手动指定程序位置（DSH 也可以填 dsh web 打印的地址）"
echo "  $ROOT/scripts/install.sh --uninstall   卸载"

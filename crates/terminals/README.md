# tether-terminals（T07）

公开接口保持 `docs/CONTRACTS.md` 原样。`TermManager` 可在无 Tokio runtime 的线程中使用；
每个活动终端用一个阻塞读取线程和一个进程监督线程，已退出终端共用一个弱引用清理线程。

- `new(DEFAULT_SCROLLBACK)` 保留 512 KiB；`new(0)` 关闭历史缓冲，但仍广播实时数据。
- 历史写入、`Data` 广播、attach 的历史复制与订阅，共用 `Output` 的一把锁。
  广播队列保留 1024 个 chunk；慢消费者须处理 `RecvError::Lagged` 并重新 attach。
- 缓冲超限后从完整行开始保留；超长行的剩余部分跨读取丢弃至 LF，避免残留 UTF-8/ANSI 前缀。
- `Exit` 在最终输出排空后发送一次，同时更新 `running=false`；晚 attach 也能收到退出码。
  `kill` 移除注册项，已有接收端仍能收到 `Exit`。自然退出保留 10 分钟后自动清理。
- 私有 `Timing` 注入静默阈值、保留时间和轮询间隔；默认 8 秒静默，每段静默只提醒一次。
- Windows 用 `which`/PATHEXT 解析命令；`.cmd`/`.bat` 经 `cmd.exe /d /v:off /s /c` 执行。
  支持空格、中文路径及参数中的 `%`、`!`、`&`、双引号和末尾反斜杠（含 `%*` 转发 shim）。
  批处理参数中的换行明确返回错误；`.exe` 参数不受这一批处理限制。
- portable-pty 0.9 的 ConPTY 启用了继承光标查询：创建时回答初始 DSR，并只过滤这一次宿主查询，
  防止没有手机附着时命令挂起，以及重连回放产生第二次光标回复。后续应用 DSR 保持原样。
  ConPTY 使用伪控制台启动，不新建可见控制台；依赖没有暴露 `creation_flags` 设置接口。

## 离线验收（本次为 Windows / ConPTY）

```powershell
cargo test -p tether-terminals --offline
$env:CARGO_NET_OFFLINE = 'true'
cargo clippy -p tether-terminals -- -D warnings
cargo clippy -p tether-terminals --offline --all-targets -- -D warnings
cargo fmt -p tether-terminals -- --check
```

真实 shell 测试使用 `cmd.exe /d /q` 和 `powershell.exe -NoProfile`；不调用模型或账号。
Unix 分支使用 `sh`，本次未在 macOS/Linux 上运行。内存测试覆盖读取、抢占 attach、静默和退出顺序。

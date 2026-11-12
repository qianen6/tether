# T07 终端

**阶段 1。归属路径：`crates/terminals`。**

## 目标

实现 `CONTRACTS.md` 中的 `TermManager`：用 `portable-pty`（Windows 上是 ConPTY）运行任意命令行程序，供手机远程操作。

## 要求

1. `open`：用 `which` 解析 `command[0]`（Windows 下支持 `.exe`、`.cmd`、`.bat`），设置 `cwd` 和终端大小；环境变量加 `TERM=xterm-256color`、`COLORTERM=truecolor`。终端 id 用 uuid。
2. 输出：在独立线程里读 PTY（`portable-pty` 的读取是阻塞的），读到的数据写入环形滚动缓冲区（默认 512 KiB），再广播 `TermChunk::Data`。
3. `attach`：返回当前滚动缓冲区的副本，加一个广播接收端。**拷贝缓冲区和订阅广播必须在同一把锁内完成**，否则会漏数据或重复。
4. `input` / `resize` / `kill`。进程退出后广播 `Exit(code)`，`TermInfo.running = false`；已退出的终端保留到被 `kill`，或退出 10 分钟后自动清理。
5. `Attention`：输出里出现响铃字符 `\x07`，或者"有过输出、随后静默超过 8 秒"时广播，用于"终端在等你"的提醒。每次新输出后重新计时，同一段静默只报一次。
6. 滚动缓冲区截断时，不要把 UTF-8 多字节字符或 ANSI 转义序列切断在开头（从下一个换行处开始保留即可）。

## 测试

在当前平台上运行真实 shell（Windows 用 `cmd.exe /c` 或 `powershell -NoProfile -Command`，Unix 用 `sh -c`）：

- `echo hello` → 收到包含 `hello` 的输出，然后 `Exit(Some(0))`。
- 交互：启动 shell → `input("echo abc\r")` → 收到 `abc`。
- `attach` 时已经有输出：拿到的滚动缓冲区里有之前的输出，之后的输出从广播收到，不丢不重。
- 环形缓冲区超出容量时被正确截断。
- `resize` 不报错；`kill` 后收到 `Exit`。
- `Attention`：输出响铃字符时触发；静默检测用可注入的时钟或较短的阈值测试。

## 验收

```sh
cargo test -p tether-terminals --offline
cargo clippy -p tether-terminals -- -D warnings
```

在报告中说明测试是在哪个系统上跑的。

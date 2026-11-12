# T01 隧道：控制 tailpocket-host

**阶段 1。归属路径：`crates/tunnel`。**

## 目标

用 Rust 实现 `CONTRACTS.md` 中 `crates/tunnel` 的 API：启动并控制 TailPocket 的电脑端随附程序 `tailpocket-host`。同时**验证它在 Windows 上能正常工作**（上游没测过 Windows）。

## 参考

- `.ref/tailpocket/sdk/node/index.js`、`index.d.ts`：上游 Node SDK，按行收发 JSON 控制 `tailpocket-host`（约 200 行）。照它的协议用 Rust 实现。
- `.ref/tailpocket/cmd/tailpocket-host/`：随附程序本身的源码，弄清每条命令和事件。
- `.ref/tailpocket/docs/`：协议与安全说明。
- `.ref/boop/src/pocket.ts`：Boop 怎么用它（状态文件 0600、配对窗口、吊销）。

## 要求

1. `Tunnel::start`：启动随附程序（`stdin`/`stdout` 管道，Windows 下隐藏窗口），传入 `forward`、名称、已保存的身份和已配对手机、`derpMapUrl`。状态变化时写 `state_path`，原子写入（先写临时文件再改名）；Unix 下权限 0600。
2. `pair`：打开配对窗口，`payload` 和 `qr_prefix` 原样传入；返回 `PairWindow`，手机加入后通过 `joined` 送出 `PhoneJoined`。
3. `revoke` / `set_allowed`：调用随附程序对应命令。
4. 随附程序意外退出：自动重启（指数退避，最长 30 秒），重启后恢复身份和已配对手机。
5. `close`：通知随附程序退出，等它结束（最多 5 秒，超时就杀掉）。
6. 协议解析逻辑与进程管理分离，可以用内存流测试。

## 测试

- 单元测试：用内存流模拟随附程序，覆盖启动、配对、加入、吊销、意外退出后重启、状态文件写入。
- 集成测试（`tests/real_helper.rs`，标记 `#[ignore]`）：启动 `vendor/tailpocket/bin/` 里真实的 `tailpocket-host`，把 `forward` 指向测试里起的回显 TCP 服务；再用 `tailpocket-phone` 程序扫这个二维码配对，经隧道发数据，确认收到回显；然后吊销，确认连接断开且无法重连。`tailpocket-phone` 的用法看 `.ref/tailpocket/cmd/tailpocket-phone/`。

**这个集成测试需要联网（走 Tailcat 公共中继）**，在 Codex 沙箱里跑不了。写好后在报告中给出手动运行命令，由人工在 Windows 上执行。

## 验收

```sh
cargo test -p tether-tunnel --offline
cargo clippy -p tether-tunnel -- -D warnings
# 人工在 Windows 上执行：
cargo test -p tether-tunnel -- --ignored --nocapture
```

人工执行结果需在报告中注明：配对耗时、走直连还是中继、吊销是否立即生效。若 Windows 上失败，记录完整错误输出。

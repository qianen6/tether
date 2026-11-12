# T01 — TailPocket 隧道

公开 API 与 `docs/CONTRACTS.md` 一致，未改变手机通信协议。

## 实现边界

- `src/protocol.rs`：上游 HOST-IPC 的按行 JSON、字段映射、1 MiB 帧上限、可取消的分片读取；独立写任务串行写入，避免写满 stdin 阻塞关闭操作。未知事件兼容忽略，已知事件字段损坏则断开并恢复。
- `src/state.rs`：只接受不存在的状态文件为首次启动；损坏文件返回错误，不静默重置身份。串行处理 `save`，同目录临时文件、`sync_all`、原子替换；成功后才回复 `saved: true`。Unix 文件及临时文件均为 0600。Windows 使用 tempfile 的替换重命名，不采用先删除旧文件的方式。
- `src/process.rs`：薄进程封装、管道、`kill_on_drop`、Windows `CREATE_NO_WINDOW`；内部 `which` 路径解析兼容 PATH、PATHEXT、`.exe`/`.cmd`、空格与中文，不增加冻结 lockfile 的依赖边。
- `src/supervisor.rs`：请求 ID 关联、配对窗口和超时、进程退出/EOF/写管道失败检测、重启与清理。测试通过注入内存随附程序覆盖这些路径，不启动真实隧道。

`tempfile` 从原有 dev-dependency 移到普通 dependency；依赖图不变，根 `Cargo.lock` 不变。

## 生命周期语义

- 首次启动等待 `ready`，上限 30 秒；首次配置/启动错误直接返回。`forward` 必须为非零端口的 loopback TCP 地址。
- 运行后意外退出按 1、2、4、8、16、30、30…秒退避；连续运行满 60 秒后重置退避。重启以最近一次成功落盘的 identity/allowed 初始化；断开和恢复中 `address()` 返回 `None`。
- 重启不复播旧命令或旧配对二维码，避免重复操作；挂起请求返回错误，所有旧 `joined` 发送端被释放。恢复未就绪时的新请求返回错误，调用方可在就绪后重试。
- 请求响应上限 30 秒。`pair` 的 TTL 向上取整为秒，零使用上游默认 120 秒，上限 900 秒；payload 和二维码前缀不改写。上游 `expiresAt`（Unix 秒）转换为契约的 Unix 毫秒。
- helper 的 `pairingClosed`、本地过期计时器、关闭或断线均释放未加入窗口。取消等待 `pair()` 后收到窗口回应会发送 `cancel`。手机成功加入只发送一次 `PhoneJoined`。
- `close` 通过关闭 stdin 发出退出通知，最多等待 5 秒，超时强制终止并回收。强杀后的 OS 回收另设 5 秒防挂死上限。丢弃 `Tunnel` 也会通知监督任务清理；不在析构中阻塞线程。
- 生产代码不记录 identity、二维码、payload 或原始协议帧；stderr 不转发到应用日志。上游 IPC 的固定错误码作为请求错误返回。

## 离线验收

```powershell
$env:CARGO_NET_OFFLINE = 'true'
cargo test -p tether-tunnel --offline
cargo clippy -p tether-tunnel -- -D warnings
cargo fmt -p tether-tunnel -- --check
```

单元测试不创建网络连接。真实随附程序只出现在下列忽略测试中。

## Windows 人工真实隧道验收（本任务未执行）

在仓库根目录、有公网连接的 PowerShell 中运行任务指定命令：

```powershell
cargo test -p tether-tunnel -- --ignored --nocapture
```

也可只运行集成测试并保存完整输出（PowerShell）：

```powershell
cargo test -p tether-tunnel --offline --test real_helper -- --ignored --nocapture 2>&1 |
    Tee-Object -FilePath "$env:TEMP\tether-T01-real-helper.log"
```

默认读取 `vendor/tailpocket/bin/tailpocket-{host,phone}-windows-amd64.exe`；macOS/Linux 按平台和架构选择。可通过 `TAILPOCKET_HOST`、`TAILPOCKET_PHONE` 指定二进制绝对路径。

测试启动 loopback TCP 回显服务，将 `pair` 返回的二维码文本交给真实 phone CLI 的 stdin（等价于扫码解码），检查 payload、手机加入、状态落盘、包含 NUL/非 UTF-8 字节的数据回显。随后吊销手机，要求原连接在 5 秒内关闭，原客户端及全新客户端均无法再次抵达回显服务，吊销结果也必须落盘。子进程均隐藏窗口，退出时自动清理。

测试输出配对耗时、吊销断开耗时、两种重连验证结果；失败时保留完整错误链和 phone stderr。**直连/中继路径不在上游 IPC 中暴露，且上游关闭了 Tailcat 日志；地址、耗时或同机运行均不构成路径证据。** 人工执行者需另用网络观测记录路径；测试明确打印“未观测”，不伪造结果。

当前人工结果：未执行；配对耗时、直连/中继、Windows 实际隧道连通性、吊销生效耗时均待人工补录。离线测试只验证 Windows 构建及内存替身行为。

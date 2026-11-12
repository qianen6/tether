# @tether/client

Tether 协议 v1 的浏览器 / React Native 客户端。生产代码不依赖 Node API，协议类型来自 Rust 生成的 `src/generated/`，终端帧复用 `src/binary.ts`。

## 使用

```ts
import { TetherClient } from "@tether/client";

// 示例存储；App 可以用相同的异步 get/set 接口接入自己的持久化存储。
const values = new Map<string, string>();
const client = new TetherClient({
  url: "ws://127.0.0.1:12345/v1/ws", // 手机隧道给出的本地转发地址
  deviceName: "Phone",
  pairSecret: "pairing-payload", // 首次配对提供；后续实例省略，自动读取 storage 的 token
  storage: {
    get: async key => values.get(key) ?? null,
    set: async (key, value) => { values.set(key, value); },
  },
});

await client.connect();
const session = await client.createSession("codex");
const unsubscribe = await client.subscribe(session.id, event => console.log(event));
await client.send(session.id, "Describe this project");

const terminal = await client.openTerminal({ cols: 80, rows: 24 });
terminal.onData(bytes => console.log(new TextDecoder().decode(bytes)));
terminal.onExit(code => console.log(code));
terminal.write("echo hello\r\n");
await terminal.resize(120, 40);
await terminal.detach();
unsubscribe();
client.close();
```

## 行为

- `connect()` 合并并发调用，收到 `welcome` 且完成恢复后返回。配对只发送一次 `pair`，等待 `storage.set("token", ...)` 完成；同一连接直接进入认证状态，不额外发送 `hello`。后续连接使用令牌。
- 服务端 `error` 以 `TetherError` 抛出，保留 `code` 和 `message`；认证 `rejected` 的错误同时带有对应的 `code` / `reason`，停止自动重连。请求 30 秒超时，握手 5 秒超时。
- 每 20 秒发送 `ping`；任何接收帧都会刷新 40 秒无帧超时。重连从 1 秒指数退避至 30 秒，叠加 0–25% 正向随机抖动（总延迟上限 30 秒）。
- 活动订阅重连时从已交付的持久事件游标补发。每个监听者独立过滤重复 `seq`；协议保证历史和实时事件按序发送。`assistant_delta` 按到达顺序交付，不更新持久游标，也不按 `seq` 去重。`assistant_text` 是完整文本，UI 按 `item` 替换增量内容。
- **断线会立即拒绝尚未回复的请求；不会自动重发 `send_message` 等有副作用的命令。** 服务端可能已经执行了这类请求，因此由事件补发恢复已提交的结果，而不是重复执行命令。`disconnected` 后可等待 `state` 回到 `open`，再提交新的操作。
- 已打开/附着的终端在重连后自动 `term_attach`。恢复期间 `write()` 抛出 `disconnected`。滚动缓冲可能再次到达，它是原始字节流，不做事件式去重。安装 `onData` 前的数据暂存上限为 8 MiB；超出时丢弃最早的整块数据。
- `detach()` / `kill()` 立即取消恢复意图，已退出终端不再自动附着。恢复中返回 `not_found` 的会话停止续订，终端触发 `onExit(null)`。
- `close()` 停止所有定时器、拒绝待处理请求并清除订阅/终端恢复意图。事件监听者保留；再次显式 `connect()` 后需重新订阅。监听者抛出的异常不影响其他监听者和协议处理。
- 文本和二进制发送/接收均检查 8 MiB 单帧上限。

## 离线验收

PowerShell 使用 conda 的 Node 22（PowerShell 7 下先设置原生命令参数兼容模式）：

```powershell
$PSNativeCommandArgumentPassing = 'Legacy'
(& 'C:/ProgramData/anaconda3/Scripts/conda.exe' shell.powershell hook) | Out-String | Invoke-Expression
conda activate tether
pnpm --filter @tether/client test
pnpm --filter @tether/client exec tsc --noEmit
```

测试只连接本机 `ws` 模拟服务器，不访问互联网、账号、模型或真实 agent。套件来自 `test/`。仓库未给本包配置 lint 命令。

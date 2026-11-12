# T08 TypeScript 客户端 SDK

**阶段 1。归属路径：`packages/client`。**

## 目标

实现 `CONTRACTS.md` 中 `packages/client` 的 `TetherClient`，手机 App 和电脑端界面都用它。协议类型只从 `src/generated/` 导入（T00 由 ts-rs 生成）。

## 要求

1. **连接与认证**：有 `pairSecret` 时先发 `pair`，拿到令牌后通过 `storage.set("token", ...)` 保存；否则发 `hello`。收到 `rejected` 就抛出带 `reason` 的错误，不再重连。
2. **请求响应**：`req` 递增，按 `req` 匹配 `ok` / `error`；默认超时 30 秒；`error` 抛出带 `code` 的 `TetherError`。
3. **心跳**：每 20 秒 `ping`，40 秒收不到任何帧就判定断线。
4. **自动重连**：指数退避（1 秒起，最长 30 秒，加随机抖动）；重连成功后：
   - 对每个活动订阅，用 `after = 该会话已收到的最大 seq` 重新 `subscribe`；
   - 按 `seq` 去重，保证回调收到的事件不重不漏、顺序递增（`assistant_delta` 例外，它没有持久的 seq，按到达顺序转发）；
   - 重新附着已打开的终端。
5. **终端**：二进制帧按 `PROTOCOL.md` 第 2 节编解码；`TerminalHandle.write` 接受字符串（UTF-8 编码）或字节。
6. **事件**：`state` 变化、`session_update`、`session_closed`、`notify`。
7. 不依赖 Node 专有 API，能在 React Native 和浏览器里运行（只用 `WebSocket`、`TextEncoder`、`TextDecoder`、`Uint8Array`）。

## 测试（vitest）

在测试里用 `ws` 写一个**模拟服务器**，实现协议的一个子集（配对、hello、会话、订阅补发、终端回显），覆盖：

- 首次配对保存令牌；之后用令牌连接；被拒绝时不重连。
- 请求超时和错误码。
- 发送中途断线 → 重连 → 补发 → 回调收到的事件序列完整无重复。
- 心跳超时触发重连。
- 终端二进制帧往返，以及重连后自动重新附着。

T10 会再加上连接真实 `tether serve` 的端到端测试，本任务不需要。

## 验收

```sh
pnpm --filter @tether/client test
pnpm --filter @tether/client exec tsc --noEmit
```

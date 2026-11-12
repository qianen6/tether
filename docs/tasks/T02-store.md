# T02 存储：SQLite

**阶段 1。归属路径：`crates/store`。**

## 目标

实现 `CONTRACTS.md` 中 `crates/store` 的全部 API。

## 要求

1. 建表：`devices`（id、name、token_hash、phone_key、paired_at、last_seen）、`sessions`（id、agent、title、cwd、state、status_line、created_at、updated_at、last_seq、native_id）、`events`（session、seq、at、json，主键 session+seq）。
2. 用 `PRAGMA user_version` 做迁移；开启 WAL；外键级联：删除会话时删除其事件。
3. 令牌：32 字节随机数，base64url 编码后返回明文；数据库只存 SHA-256。`device_by_token` 先算哈希再查，比较用常量时间（`subtle`）。
4. `append_event` 拒绝瞬时事件（`assistant_delta`），返回错误；拒绝 `seq` 不递增的事件。
5. `events_after` 按 `seq` 升序，最多 `limit` 条。
6. 并发：内部 `Mutex<Connection>`，所有方法 `&self`。

## 测试

- 设备：签发、按令牌查找、错误令牌、吊销后查不到、`set_phone_key`、`touch_device`。
- 会话：插入、更新、列表、删除后事件也被删除。
- 事件：追加、分页读取、拒绝瞬时事件、拒绝重复 `seq`。
- 文件数据库：关闭后重新打开，数据还在；迁移版本号正确。
- 多线程并发写 1000 条事件不出错。

## 验收

```sh
cargo test -p tether-store --offline
cargo clippy -p tether-store -- -D warnings
```

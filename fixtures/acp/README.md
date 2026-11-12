# ACP v1 模拟对话

全部内容由 Rust 测试模拟生成/使用，不是任何真实 agent 的运行录制。每行是一条完整 JSON-RPC 2.0 消息，无包装字段。

| 文件 | 方向 / 用途 |
| --- | --- |
| initialize.client.jsonl | 客户端初始化请求的精确期望 |
| initialize.agent.jsonl | agent 初始化响应，能力/认证方法由测试按场景调整 |
| new-session.agent.jsonl | 新会话响应 |
| stream.jsonl | 两段文本流和 prompt 的 end_turn 响应 |
| tools.jsonl | 思考、计划、工具初始/增量更新、成功/失败输出 |
| permission.jsonl | 带四种规范 option kind 的权限请求 |
| decisions.client.jsonl | allow_once / allow_always / reject_once 对应的客户端响应与取消响应 |
| cancel.client.jsonl | session/cancel 通知的精确期望 |
| cancelled.jsonl | 取消后的最后工具更新与 cancelled 响应 |
| resume-unsupported.jsonl | 未声明 loadSession 时的初始化和新建会话响应 |
| load-session.jsonl | loadSession 的历史通知与空对象响应 |
| auth-error.jsonl | 标准 -32000 认证错误 |

普通流程请求 ID 为 initialize=1、new/load=2、首轮 prompt=3；agent 请求的 ID 独立关联且保留字符串/数字类型。`mock-session` 是内存 agent 的会话 ID。测试另外构造限额、错序、取消竞态、畸形输入、非文本、多个 messageId、多个审批等边界。

从仓库根目录运行 `python fixtures/acp/validate.py` 可逐行对本地 ACP v1 schema 的具体请求/响应类型校验，而非只检查 JSON 语法。

# T05 合成回放数据

这些 JSONL 全部手写，既未启动 Claude CLI，也未连接真实账号或模型。
`// after_*` 是 `testkit::LineEnd::send_fixture` 支持的注释，用于暂停回放，
先核对适配器写出的控制请求或审批回复，再继续后半段。

覆盖：流式问答、Bash 允许、Edit 拒绝、带权限建议的会话允许、abort、
interrupt、审批取消及迟到回复、result 错误、resume、初始化错误。
出站 JSON 由测试按完整对象比较，保留原始 input 和 permission_suggestions。

运行：`cargo test -p tether-adapter-claude --offline`。

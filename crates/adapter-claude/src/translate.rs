//! Pure translation: no child processes, channel ownership or wire writes.

use serde_json::Value;
use tether_agent_api::AgentOutput;
use tether_proto::{
    Approval, ApprovalKind, Decision, EventKind, MAX_TOOL_OUTPUT, NoticeLevel, SessionState,
    truncate_tail,
};

use crate::protocol::{Message, Permission};

#[derive(Debug, Default)]
pub(super) struct Translator {
    message_id: Option<String>,
}

pub(super) fn state(state: SessionState) -> AgentOutput {
    AgentOutput::Event(EventKind::State {
        state,
        status_line: None,
    })
}

pub(super) fn notice(level: NoticeLevel, text: impl Into<String>) -> AgentOutput {
    AgentOutput::Event(EventKind::Notice {
        level,
        text: text.into(),
    })
}

// The frozen proto helper prepends an ellipsis in addition to its byte budget.
// Reserve those bytes locally so the *whole* output still fits MAX_TOOL_OUTPUT.
pub(super) fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        text.to_owned()
    } else {
        truncate_tail(text, limit.saturating_sub('…'.len_utf8()))
    }
}

fn head(text: &str, limit: usize) -> String {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

pub(super) fn tool_summary(tool: &str, input: &Value) -> String {
    match tool {
        "Bash" => field(input, "command").into(),
        "Edit" | "MultiEdit" | "Write" => field(input, "file_path").into(),
        _ => head(&pretty(input), MAX_TOOL_OUTPUT),
    }
}

fn edit_detail(input: &Value) -> String {
    format!(
        "old_string:\n{}\nnew_string:\n{}",
        head(field(input, "old_string"), 4096),
        head(field(input, "new_string"), 4096)
    )
}

pub(super) fn approval(permission: &Permission, cwd: Option<String>) -> Approval {
    let input = &permission.input;
    let (kind, title, detail) = match permission.tool.as_str() {
        "Bash" => {
            let command = field(input, "command");
            let description = field(input, "description");
            (
                ApprovalKind::Command,
                format!("运行：{}", command.chars().take(80).collect::<String>()),
                if description.is_empty() {
                    command.into()
                } else {
                    format!("{command}\n{description}")
                },
            )
        }
        "Edit" | "MultiEdit" => {
            let detail = if let Some(edits) = input["edits"].as_array() {
                edits
                    .iter()
                    .map(edit_detail)
                    .collect::<Vec<_>>()
                    .join("\n\n")
            } else {
                edit_detail(input)
            };
            (
                ApprovalKind::FileChange,
                format!("修改 {}", field(input, "file_path")),
                head(&detail, MAX_TOOL_OUTPUT),
            )
        }
        "Write" => (
            ApprovalKind::FileChange,
            format!("写入 {}", field(input, "file_path")),
            head(field(input, "content"), 4096),
        ),
        tool => (
            ApprovalKind::Tool,
            format!("使用工具 {tool}"),
            head(&pretty(input), MAX_TOOL_OUTPUT),
        ),
    };
    let mut choices = vec![Decision::AllowOnce];
    if !permission.suggestions.is_empty() {
        choices.push(Decision::AllowSession);
    }
    choices.extend([Decision::Deny, Decision::Abort]);
    Approval {
        id: permission.id.clone(),
        kind,
        title,
        detail,
        cwd,
        choices,
    }
}

fn tool_output(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| {
                block["text"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| pretty(block))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        _ => pretty(content),
    }
}

impl Translator {
    pub fn translate(&mut self, message: &Message) -> Vec<AgentOutput> {
        let mut out = Vec::new();
        match message {
            Message::System {
                subtype,
                session_id: Some(id),
            } if subtype == "init" && !id.is_empty() => {
                out.push(AgentOutput::NativeId(id.clone()));
            }
            Message::StreamEvent { event } => match field(event, "type") {
                "message_start" => {
                    self.message_id = event["message"]["id"].as_str().map(str::to_owned)
                }
                "message_stop" => self.message_id = None,
                "content_block_delta" if field(&event["delta"], "type") == "text_delta" => {
                    if let (Some(id), Some(index), Some(text)) = (
                        &self.message_id,
                        event["index"].as_u64(),
                        event["delta"]["text"].as_str(),
                    ) {
                        out.push(AgentOutput::Event(EventKind::AssistantDelta {
                            item: format!("{id}:{index}"),
                            text: text.into(),
                        }));
                    }
                }
                _ => {}
            },
            Message::Assistant { message } => {
                if let Some(blocks) = message["content"].as_array() {
                    for (index, block) in blocks.iter().enumerate() {
                        let id = message["id"].as_str();
                        let event = match field(block, "type") {
                            "text" => id.zip(block["text"].as_str()).map(|(id, text)| {
                                EventKind::AssistantText {
                                    item: format!("{id}:{index}"),
                                    text: text.into(),
                                }
                            }),
                            "thinking" => id.zip(block["thinking"].as_str()).map(|(id, text)| {
                                EventKind::Thinking {
                                    item: format!("{id}:{index}"),
                                    text: text.into(),
                                }
                            }),
                            "tool_use" => block["id"].as_str().zip(block["name"].as_str()).map(
                                |(id, name)| EventKind::ToolStart {
                                    item: id.into(),
                                    tool: name.into(),
                                    summary: tool_summary(name, &block["input"]),
                                },
                            ),
                            _ => None,
                        };
                        if let Some(event) = event {
                            out.push(AgentOutput::Event(event));
                        }
                    }
                }
            }
            Message::User { message } => {
                if let Some(blocks) = message["content"].as_array() {
                    for block in blocks {
                        if field(block, "type") == "tool_result"
                            && let Some(id) = block["tool_use_id"].as_str()
                        {
                            out.push(AgentOutput::Event(EventKind::ToolEnd {
                                item: id.into(),
                                ok: !block["is_error"].as_bool().unwrap_or(false),
                                output: tail(&tool_output(&block["content"]), MAX_TOOL_OUTPUT),
                            }));
                        }
                    }
                }
            }
            Message::Result { data } => {
                self.message_id = None;
                if data
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    let text = data
                        .get("errors")
                        .and_then(Value::as_array)
                        .map(|errors| {
                            errors
                                .iter()
                                .map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| pretty(v)))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .filter(|text| !text.is_empty())
                        .or_else(|| {
                            data.get("result")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| {
                            format!(
                                "Claude 回合出错：{}",
                                data.get("subtype")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown")
                            )
                        });
                    out.push(notice(NoticeLevel::Error, text));
                }
                let mut summary = Vec::new();
                if let Some(duration) = data.get("duration_ms").filter(|v| v.is_number()) {
                    summary.push(format!("耗时 {duration} ms"));
                }
                if let Some(cost) = data.get("total_cost_usd").and_then(Value::as_f64) {
                    summary.push(format!("费用 ${cost:.6}"));
                }
                out.push(AgentOutput::Event(EventKind::TurnDone {
                    summary: (!summary.is_empty()).then(|| summary.join("，")),
                }));
                out.push(state(SessionState::Idle));
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn translate(translator: &mut Translator, value: Value) -> Vec<AgentOutput> {
        translator.translate(&serde_json::from_value(value).unwrap())
    }

    #[test]
    fn stream_and_complete_blocks_share_item_ids() {
        let mut t = Translator::default();
        assert_eq!(
            translate(
                &mut t,
                json!({"type":"system", "subtype":"init", "session_id":"native"})
            ),
            [AgentOutput::NativeId("native".into())]
        );
        assert!(translate(&mut t, json!({"type":"stream_event", "event":{"type":"message_start", "message":{"id":"m"}}})).is_empty());
        assert_eq!(
            translate(
                &mut t,
                json!({"type":"stream_event", "event":{"type":"content_block_delta", "index":0, "delta":{"type":"text_delta", "text":"hello"}}})
            ),
            [AgentOutput::Event(EventKind::AssistantDelta {
                item: "m:0".into(),
                text: "hello".into()
            })]
        );
        let out = translate(
            &mut t,
            json!({"type":"assistant", "message":{"id":"m", "content":[
            {"type":"text", "text":"hello"}, {"type":"thinking", "thinking":"plan"},
            {"type":"tool_use", "id":"tool", "name":"Bash", "input":{"command":"pwd"}}, {"type":"future_block"}]}}),
        );
        assert_eq!(
            out,
            [
                AgentOutput::Event(EventKind::AssistantText {
                    item: "m:0".into(),
                    text: "hello".into()
                }),
                AgentOutput::Event(EventKind::Thinking {
                    item: "m:1".into(),
                    text: "plan".into()
                }),
                AgentOutput::Event(EventKind::ToolStart {
                    item: "tool".into(),
                    tool: "Bash".into(),
                    summary: "pwd".into()
                }),
            ]
        );
    }

    #[test]
    fn tool_results_keep_utf8_tail_within_eight_kib() {
        let mut t = Translator::default();
        let text = format!("prefix{}END", "中文🙂".repeat(2000));
        let out = translate(
            &mut t,
            json!({"type":"user", "message":{"content":[
                {"type":"tool_result", "tool_use_id":"tool", "is_error":true, "content":text},
                {"type":"tool_result", "tool_use_id":"two", "content":[{"type":"text", "text":"a"}, {"type":"text", "text":"b"}]}
            ]}}),
        );
        match out[0].event().unwrap() {
            EventKind::ToolEnd { item, ok, output } => {
                assert_eq!(item, "tool");
                assert!(!ok);
                assert!(output.len() <= MAX_TOOL_OUTPUT);
                assert!(output.starts_with('…'));
                assert!(output.ends_with("END"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            out[1].event(),
            Some(&EventKind::ToolEnd {
                item: "two".into(),
                ok: true,
                output: "a\nb".into()
            })
        );
    }

    #[test]
    fn result_error_precedes_turn_done_and_idle() {
        let mut t = Translator::default();
        let out = translate(
            &mut t,
            json!({"type":"result", "is_error":true, "errors":["bad", "worse"], "duration_ms":125, "total_cost_usd":0.0123}),
        );
        assert_eq!(
            out,
            [
                notice(NoticeLevel::Error, "bad\nworse"),
                AgentOutput::Event(EventKind::TurnDone {
                    summary: Some("耗时 125 ms，费用 $0.012300".into())
                }),
                state(SessionState::Idle)
            ]
        );
        assert_eq!(
            translate(&mut t, json!({"type":"result"})),
            [
                AgentOutput::Event(EventKind::TurnDone { summary: None }),
                state(SessionState::Idle)
            ]
        );
        let out = translate(
            &mut t,
            json!({"type":"result", "is_error":true, "result":"failure"}),
        );
        assert_eq!(out[0], notice(NoticeLevel::Error, "failure"));
    }

    #[test]
    fn all_approval_cards_and_summaries() {
        for (tool, input, title, kind, summary) in [
            (
                "Bash",
                json!({"command":"pwd", "description":"where"}),
                "运行：pwd",
                ApprovalKind::Command,
                "pwd",
            ),
            (
                "Edit",
                json!({"file_path":"a.rs", "old_string":"old", "new_string":"new"}),
                "修改 a.rs",
                ApprovalKind::FileChange,
                "a.rs",
            ),
            (
                "MultiEdit",
                json!({"file_path":"a.rs", "edits":[{"old_string":"a", "new_string":"b"}]}),
                "修改 a.rs",
                ApprovalKind::FileChange,
                "a.rs",
            ),
            (
                "Write",
                json!({"file_path":"a.rs", "content":"body"}),
                "写入 a.rs",
                ApprovalKind::FileChange,
                "a.rs",
            ),
            (
                "Read",
                json!({"path":"a.rs"}),
                "使用工具 Read",
                ApprovalKind::Tool,
                "",
            ),
        ] {
            let p = Permission {
                id: "p".into(),
                tool: tool.into(),
                input: input.clone(),
                suggestions: vec![],
            };
            let card = approval(&p, Some("cwd".into()));
            assert_eq!(card.title, title);
            assert_eq!(card.kind, kind);
            assert_eq!(card.cwd.as_deref(), Some("cwd"));
            assert_eq!(
                card.choices,
                [Decision::AllowOnce, Decision::Deny, Decision::Abort]
            );
            if tool != "Read" {
                assert_eq!(tool_summary(tool, &input), summary);
            }
            if tool == "Bash" {
                assert_eq!(card.detail, "pwd\nwhere");
            }
            if tool == "Edit" {
                assert_eq!(card.detail, "old_string:\nold\nnew_string:\nnew");
            }
            if tool == "MultiEdit" {
                assert_eq!(card.detail, "old_string:\na\nnew_string:\nb");
            }
            if tool == "Read" {
                assert_eq!(card.detail, pretty(&input));
            }
        }
    }

    #[test]
    fn cards_truncate_on_utf8_boundaries_and_offer_session_only_with_suggestions() {
        let p = Permission {
            id: "p".into(),
            tool: "Bash".into(),
            input: json!({"command":"中".repeat(100)}),
            suggestions: vec![json!({"type":"addRules"})],
        };
        let card = approval(&p, None);
        assert_eq!(card.title, format!("运行：{}", "中".repeat(80)));
        assert_eq!(card.detail, "中".repeat(100));
        assert!(card.choices.contains(&Decision::AllowSession));
        let p = Permission {
            tool: "Write".into(),
            input: json!({"content":"中".repeat(3000)}),
            ..p
        };
        let card = approval(&p, None);
        assert!(card.detail.len() <= 4096);
        assert_eq!(card.detail, "中".repeat(1365));
        let p = Permission {
            tool: "Other".into(),
            ..p
        };
        assert!(approval(&p, None).detail.len() <= 8192);
    }

    #[test]
    fn unknown_and_non_text_stream_events_do_not_invent_deltas() {
        let mut t = Translator::default();
        for value in [
            json!({"type":"future"}),
            json!({"type":"system", "subtype":"status"}),
            json!({"type":"stream_event", "event":{"type":"content_block_delta", "index":0, "delta":{"type":"text_delta", "text":"orphan"}}}),
            json!({"type":"stream_event", "event":{"type":"message_stop"}}),
            json!({"type":"stream_event", "event":{"type":"content_block_delta", "index":0, "delta":{"type":"thinking_delta", "thinking":"x"}}}),
            json!({"type":"control_cancel_request", "request_id":"x"}),
            json!({"type":"control_response", "response":{"subtype":"success", "request_id":"x"}}),
            json!({"type":"control_request", "request_id":"x", "request":{"subtype":"can_use_tool"}}),
        ] {
            assert!(translate(&mut t, value).is_empty());
        }
    }
}

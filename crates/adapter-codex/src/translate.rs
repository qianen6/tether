//! Pure wire-to-domain translation. No IO, clocks, processes or mutable state.

use anyhow::Context;
use serde_json::Value;
use tether_proto::{
    Approval, ApprovalKind, Decision, EventKind, MAX_TOOL_OUTPUT, NoticeLevel, SessionState,
    truncate_tail,
};

/// A message as `turn/start` and `turn/steer` take it: the text, then each
/// picture by its path on the computer.
pub(crate) fn input(text: &str, images: &[String]) -> Value {
    let mut input = vec![serde_json::json!({"type":"text","text":text,"text_elements":[]})];
    input.extend(
        images
            .iter()
            .map(|path| serde_json::json!({"type":"localImage","path":path})),
    );
    Value::Array(input)
}

pub(crate) fn state(state: SessionState) -> EventKind {
    EventKind::State {
        state,
        status_line: None,
    }
}

pub(crate) fn notice(level: NoticeLevel, text: impl Into<String>) -> EventKind {
    EventKind::Notice {
        level,
        text: text.into(),
    }
}

/// `serviceTier` in words: "default" is standard, anything else faster.
pub(crate) fn speed_word(speed: Option<&str>) -> &'static str {
    match speed {
        None => "默认",
        Some("default") => "标准",
        Some(_) => "快速",
    }
}

/// "，权限：完全访问" for a notice, nothing when none was chosen.
pub(crate) fn permission_part(permission: Option<&str>) -> String {
    match permission {
        None => String::new(),
        Some(":read-only") => "，权限：只读".into(),
        Some(":workspace") => "，权限：默认权限".into(),
        Some(":danger-full-access") => "，权限：完全访问".into(),
        Some(other) => format!("，权限：{}", other.trim_start_matches(':')),
    }
}

pub(crate) fn decision(decision: Decision) -> &'static str {
    match decision {
        Decision::AllowOnce => "accept",
        Decision::AllowSession => "acceptForSession",
        Decision::Deny => "decline",
        Decision::Abort => "cancel",
    }
}

/// proto::truncate_tail prefixes an ellipsis on truncation. Reserve its bytes
/// locally so the frozen helper still yields a *total* payload of at most 8 KiB.
pub(crate) fn tail(text: &str) -> String {
    let text = &strip_ansi(text);
    if text.len() > MAX_TOOL_OUTPUT {
        truncate_tail(text, MAX_TOOL_OUTPUT - '…'.len_utf8())
    } else {
        text.to_owned()
    }
}

/// Codex colours its log lines (`ESC[2m…`); the phone shows plain text.
pub(crate) fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // Parameters, then one final byte in @..~.
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn required<'a>(v: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    v.get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string {field}"))
}

fn text(v: &Value, field: &str) -> String {
    v.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn display(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

fn changes(item: &Value, with_diff: bool) -> anyhow::Result<String> {
    let changes = item["changes"]
        .as_array()
        .context("missing changes array")?;
    changes
        .iter()
        .map(|change| {
            let path = required(change, "path")?;
            Ok(if with_diff {
                format!("{path}\n{}", required(change, "diff")?)
            } else {
                path.to_owned()
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(|parts| parts.join("\n"))
}

pub(crate) fn notification(method: &str, params: &Value) -> anyhow::Result<Vec<EventKind>> {
    match method {
        "turn/started" => Ok(vec![state(SessionState::Working)]),
        "item/agentMessage/delta" => Ok(vec![EventKind::AssistantDelta {
            item: required(params, "itemId")?.to_owned(),
            text: required(params, "delta")?.to_owned(),
        }]),
        "item/started" | "item/completed" => item(&params["item"], method == "item/completed"),
        "turn/completed" => {
            let mut events = Vec::new();
            if !params["turn"]["error"].is_null() {
                events.push(notice(
                    NoticeLevel::Error,
                    error_text(&params["turn"]["error"]),
                ));
            }
            events.push(EventKind::TurnDone { summary: None });
            events.push(state(SessionState::Idle));
            Ok(events)
        }
        // Codex retries itself (`willRetry`); a final failure arrives again
        // in turn/completed, so only a retry is worth its own line here.
        "error" if params["willRetry"].as_bool() == Some(true) => Ok(vec![notice(
            NoticeLevel::Warn,
            format!("{}（正在重试）", error_text(&params["error"])),
        )]),
        "error" => Ok(vec![notice(
            NoticeLevel::Error,
            error_text(&params["error"]),
        )]),
        _ => Ok(Vec::new()),
    }
}

pub(crate) fn error_text(error: &Value) -> String {
    // CC Switch and the like rewrite config.toml; a thread then refuses to
    // resume under the other provider.
    if error["data"]["type"] == "modelProviderMismatch" {
        let was = error["data"]["modelProvider"].as_str().unwrap_or("?");
        let now = error["data"]["currentModelProvider"]
            .as_str()
            .unwrap_or("?");
        return format!(
            "这个对话是用「{was}」开始的，但电脑上的 Codex 现在配置的是「{now}」（可能被 CC Switch 等工具改了）。请把 Codex 切回「{was}」再继续，或者新开一个对话。"
        );
    }
    let mut message = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| display(error));
    // The provider's JSON inside the message: say what to do about it.
    if message.contains("model_not_found") {
        let hint = if message.contains("ChatGPT account") {
            "电脑上的 Codex 现在登录的是 ChatGPT 账号，不支持这个模型。请在手机上换一个模型，或在电脑上把 Codex 切回中转。"
        } else {
            "电脑上的 Codex 找不到这个模型。请在手机上换一个模型，或确认中转支持它。"
        };
        message = format!("{hint}\n{message}");
    }
    if let Some(details) = error
        .get("additionalDetails")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        message.push('\n');
        message.push_str(details);
    }
    message
}

pub(crate) fn item(item: &Value, completed: bool) -> anyhow::Result<Vec<EventKind>> {
    let kind = required(item, "type")?;
    if !matches!(
        kind,
        "agentMessage"
            | "reasoning"
            | "commandExecution"
            | "fileChange"
            | "mcpToolCall"
            | "webSearch"
    ) {
        return Ok(Vec::new());
    }
    let id = required(item, "id")?.to_owned();
    let event = match (kind, completed) {
        ("agentMessage", true) => EventKind::AssistantText {
            item: id,
            text: required(item, "text")?.to_owned(),
        },
        ("reasoning", true) => EventKind::Thinking {
            item: id,
            text: item
                .get("summary")
                .and_then(Value::as_array)
                .map(|s| {
                    s.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        },
        ("commandExecution", false) => EventKind::ToolStart {
            item: id,
            tool: "shell".into(),
            summary: required(item, "command")?.to_owned(),
        },
        ("commandExecution", true) => EventKind::ToolEnd {
            item: id,
            ok: item["exitCode"].as_i64() == Some(0),
            output: tail(&text(item, "aggregatedOutput")),
        },
        ("fileChange", false) => EventKind::ToolStart {
            item: id,
            tool: "edit".into(),
            summary: changes(item, false)?,
        },
        ("fileChange", true) => EventKind::ToolEnd {
            item: id,
            ok: item["status"] == "completed",
            output: tail(&changes(item, true)?),
        },
        ("mcpToolCall", false) => EventKind::ToolStart {
            item: id,
            tool: format!(
                "mcp:{}:{}",
                required(item, "server")?,
                required(item, "tool")?
            ),
            summary: display(&item["arguments"]),
        },
        ("mcpToolCall", true) => EventKind::ToolEnd {
            item: id,
            ok: item["status"] == "completed"
                && item["error"].is_null()
                && item["result"]["isError"] != true,
            output: tail(&display(if item["error"].is_null() {
                &item["result"]
            } else {
                &item["error"]
            })),
        },
        ("webSearch", false) => EventKind::ToolStart {
            item: id,
            tool: "webSearch".into(),
            summary: text(item, "query"),
        },
        ("webSearch", true) => EventKind::ToolEnd {
            item: id,
            ok: true,
            output: tail(&display(if !item["results"].is_null() {
                &item["results"]
            } else if !item["action"].is_null() {
                &item["action"]
            } else {
                &item["query"]
            })),
        },
        _ => return Ok(Vec::new()),
    };
    Ok(vec![event])
}

/// `previous_item` is a value captured by the adapter, not shared mutable state.
/// None means unsupported, which the caller answers with a JSON-RPC error.
pub(crate) fn request(
    method: &str,
    params: &Value,
    approval_id: String,
    previous_item: Option<&Value>,
) -> anyhow::Result<Option<Approval>> {
    let kind = match method {
        "item/commandExecution/requestApproval" => ApprovalKind::Command,
        "item/fileChange/requestApproval" => ApprovalKind::FileChange,
        _ => return Ok(None),
    };
    let item_id = required(params, "itemId")?;
    let prior = previous_item.unwrap_or(&Value::Null);
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .or_else(|| prior.get("cwd").and_then(Value::as_str))
        .map(str::to_owned);
    let reason = text(params, "reason");
    let (title, detail) = if kind == ApprovalKind::Command {
        let command = params
            .get("command")
            .and_then(Value::as_str)
            .or_else(|| prior.get("command").and_then(Value::as_str))
            .unwrap_or(item_id);
        (
            format!("运行：{command}"),
            format!(
                "{command}\ncwd: {}\n{reason}",
                cwd.as_deref().unwrap_or_default()
            ),
        )
    } else {
        let change_text = if previous_item.is_some() {
            changes(prior, true)?
        } else {
            item_id.to_owned()
        };
        ("修改文件".into(), tail(&format!("{reason}\n{change_text}")))
    };
    Ok(Some(Approval {
        id: approval_id,
        kind,
        title,
        detail,
        cwd,
        choices: vec![
            Decision::AllowOnce,
            Decision::AllowSession,
            Decision::Deny,
            Decision::Abort,
        ],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn translated(item: Value, completed: bool) -> EventKind {
        notification(
            if completed {
                "item/completed"
            } else {
                "item/started"
            },
            &json!({"item":item}),
        )
        .unwrap()
        .remove(0)
    }

    #[test]
    fn log_colours_are_dropped() {
        assert_eq!(
            tail("\u{1b}[2m2026-11-09\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m 中文"),
            "2026-11-09 ERROR 中文"
        );
        assert_eq!(strip_ansi("no colour"), "no colour");
    }

    #[test]
    fn maps_messages_reasoning_and_deltas() {
        assert_eq!(
            notification(
                "item/agentMessage/delta",
                &json!({"itemId":"a","delta":"你"})
            )
            .unwrap(),
            [EventKind::AssistantDelta {
                item: "a".into(),
                text: "你".into()
            }]
        );
        assert_eq!(
            translated(json!({"type":"agentMessage","id":"a","text":"你好"}), true),
            EventKind::AssistantText {
                item: "a".into(),
                text: "你好".into()
            }
        );
        assert_eq!(
            translated(
                json!({"type":"reasoning","id":"r","summary":["one","two"],"content":["not the summary"]}),
                true
            ),
            EventKind::Thinking {
                item: "r".into(),
                text: "one\ntwo".into()
            }
        );
        assert!(
            notification(
                "item/started",
                &json!({"item":{"type":"agentMessage","id":"a","text":""}})
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            notification("item/completed", &json!({"item":{"type":"futureItem"}}))
                .unwrap()
                .is_empty()
        );
        assert!(notification("item/agentMessage/delta", &json!({"itemId":"a"})).is_err());
    }

    #[test]
    fn maps_command_output_and_null_exit_code() {
        assert_eq!(
            translated(
                json!({"type":"commandExecution","id":"c","command":"echo hi"}),
                false
            ),
            EventKind::ToolStart {
                item: "c".into(),
                tool: "shell".into(),
                summary: "echo hi".into()
            }
        );
        for (exit, ok) in [(json!(0), true), (json!(1), false), (Value::Null, false)] {
            assert_eq!(
                translated(
                    json!({"type":"commandExecution","id":"c","exitCode":exit,"aggregatedOutput":null}),
                    true
                ),
                EventKind::ToolEnd {
                    item: "c".into(),
                    ok,
                    output: "".into()
                }
            );
        }
    }

    #[test]
    fn maps_edits_mcp_and_web_tools() {
        let mut edit = json!({"type":"fileChange","id":"f","status":"completed","changes":[{"path":"a.txt","diff":"+hi"},{"path":"b.txt","diff":"-bye"}]});
        assert_eq!(
            translated(edit.clone(), false),
            EventKind::ToolStart {
                item: "f".into(),
                tool: "edit".into(),
                summary: "a.txt\nb.txt".into()
            }
        );
        assert_eq!(
            translated(edit.clone(), true),
            EventKind::ToolEnd {
                item: "f".into(),
                ok: true,
                output: "a.txt\n+hi\nb.txt\n-bye".into()
            }
        );
        edit["status"] = json!("declined");
        assert!(matches!(
            translated(edit, true),
            EventKind::ToolEnd { ok: false, .. }
        ));
        let mut mcp = json!({"type":"mcpToolCall","id":"m","server":"s","tool":"t","arguments":{"q":1},"status":"completed","result":{"content":[]}});
        assert!(
            matches!(translated(mcp.clone(), false), EventKind::ToolStart { tool, .. } if tool == "mcp:s:t")
        );
        assert!(matches!(
            translated(mcp.clone(), true),
            EventKind::ToolEnd { ok: true, .. }
        ));
        mcp["error"] = json!({"message":"failure"});
        assert!(
            matches!(translated(mcp, true), EventKind::ToolEnd { ok: false, output, .. } if output.contains("failure"))
        );
        let web =
            json!({"type":"webSearch","id":"w","query":"q","action":{"type":"search","query":"q"}});
        assert!(
            matches!(translated(web.clone(), false), EventKind::ToolStart { tool, summary, .. } if tool == "webSearch" && summary == "q")
        );
        assert!(
            matches!(translated(web, true), EventKind::ToolEnd { ok: true, output, .. } if output.contains("search"))
        );
    }

    #[test]
    fn all_tool_outputs_are_utf8_tails_bounded_to_8k() {
        let long = format!("{}TAIL", "汉字".repeat(4000));
        for item in [
            json!({"id":"a","type":"commandExecution","exitCode":0,"aggregatedOutput":long}),
            json!({"id":"b","type":"fileChange","status":"completed","changes":[{"path":"x","diff":long}]}),
            json!({"id":"c","type":"mcpToolCall","status":"completed","result":long}),
            json!({"id":"d","type":"webSearch","query":long}),
        ] {
            let EventKind::ToolEnd { output, .. } = translated(item, true) else {
                panic!()
            };
            assert!(output.len() <= MAX_TOOL_OUTPUT);
            assert!(output.ends_with("TAIL"));
            assert!(output.starts_with('…'));
        }
        assert_eq!(tail(&"x".repeat(MAX_TOOL_OUTPUT)).len(), MAX_TOOL_OUTPUT);
    }

    #[test]
    fn maps_turns_errors_and_ignores_unknown_notifications() {
        assert_eq!(
            notification("turn/started", &json!({})).unwrap(),
            [state(SessionState::Working)]
        );
        assert_eq!(
            notification(
                "turn/completed",
                &json!({"turn":{"error":{"message":"bad","additionalDetails":"details"}}})
            )
            .unwrap(),
            [
                notice(NoticeLevel::Error, "bad\ndetails"),
                EventKind::TurnDone { summary: None },
                state(SessionState::Idle)
            ]
        );
        assert_eq!(
            notification("turn/completed", &json!({"turn":{"error":null}}))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            notification(
                "error",
                &json!({"willRetry":true,"error":{"message":"busy"}})
            )
            .unwrap(),
            [notice(NoticeLevel::Warn, "busy（正在重试）")]
        );
        assert_eq!(
            notification("error", &json!({"error":{"message":"bad"}})).unwrap(),
            [notice(NoticeLevel::Error, "bad")]
        );
        assert!(
            notification("new/notification", &json!({}))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn maps_both_approvals_and_all_four_decisions() {
        let prior = json!({"command":"echo hi","cwd":"/work"});
        let command = request(
            "item/commandExecution/requestApproval",
            &json!({"itemId":"c","command":null,"cwd":null,"reason":"why"}),
            "1".into(),
            Some(&prior),
        )
        .unwrap()
        .unwrap();
        assert_eq!(command.kind, ApprovalKind::Command);
        assert_eq!(command.title, "运行：echo hi");
        assert_eq!(command.cwd.as_deref(), Some("/work"));
        assert_eq!(command.detail, "echo hi\ncwd: /work\nwhy");
        assert_eq!(
            command
                .choices
                .iter()
                .copied()
                .map(decision)
                .collect::<Vec<_>>(),
            ["accept", "acceptForSession", "decline", "cancel"]
        );
        let prior = json!({"changes":[{"path":"a.txt","diff":"+hello"}]});
        let file = request(
            "item/fileChange/requestApproval",
            &json!({"itemId":"f","reason":"write"}),
            "2".into(),
            Some(&prior),
        )
        .unwrap()
        .unwrap();
        assert_eq!(file.kind, ApprovalKind::FileChange);
        assert_eq!(file.detail, "write\na.txt\n+hello");
        assert_eq!(file.choices, command.choices);
        let prior = json!({"changes":[{"path":"a.txt","diff":"字".repeat(9000)}]});
        assert!(
            request(
                "item/fileChange/requestApproval",
                &json!({"itemId":"f"}),
                "3".into(),
                Some(&prior)
            )
            .unwrap()
            .unwrap()
            .detail
            .len()
                <= MAX_TOOL_OUTPUT
        );
        assert!(
            request(
                "item/fileChange/requestApproval",
                &json!({}),
                "4".into(),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn all_other_server_requests_are_unsupported() {
        for method in [
            "item/tool/requestUserInput",
            "mcpServer/elicitation/request",
            "item/permissions/requestApproval",
            "item/tool/call",
            "account/chatgptAuthTokens/refresh",
            "attestation/generate",
            "applyPatchApproval",
            "execCommandApproval",
            "unknown",
        ] {
            assert!(
                request(method, &Value::Null, "id".into(), None)
                    .unwrap()
                    .is_none()
            );
        }
    }
}

//! DSH session frames → Tether events. Pure: the session loop feeds it the
//! mux frames of one session; `read_history` feeds it stored events.
//!
//! Frame shapes are the ones recorded from DSH 0.1.1-rc.2 in
//! `fixtures/dsh/` (`session/event`, `approval/*`, `question/*`).

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use tether_proto::{
    Approval, ApprovalKind, Decision, EventKind, MAX_TOOL_OUTPUT, NoticeLevel, SessionState,
    truncate_tail,
};

/// Tools whose call is shown some other way (a question is its own message).
const HIDDEN_TOOLS: &[&str] = &["ask_user_question"];
const SHELL_TOOLS: &[&str] = &["pwsh", "bash", "shell", "powershell"];
const FILE_TOOLS: &[&str] = &["write", "edit", "str_replace_editor"];

#[derive(Default)]
pub(crate) struct Translator {
    /// Stored events: include the person's messages, skip live chunks.
    history: bool,
    /// Tool calls seen so far, by call id: name and parsed arguments.
    tools: HashMap<String, (String, Value)>,
    hidden: HashSet<String>,
    last_state: Option<(SessionState, Option<String>)>,
    questions: u32,
    cwd: Option<String>,
}

pub(crate) fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for key in path {
        cur = cur.get(key)?;
    }
    cur.as_str()
}

/// The text parts of a content array, joined.
pub(crate) fn text_of(content: Option<&Value>) -> String {
    content
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn args_of(raw: Option<&Value>) -> Value {
    match raw {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

fn first_str<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| args.get(k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
}

fn one_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(200).collect();
    if line.chars().count() > 200 || s.trim().lines().count() > 1 {
        out.push('…');
    }
    out
}

impl Translator {
    pub(crate) fn live(cwd: Option<String>) -> Self {
        Self {
            cwd,
            ..Self::default()
        }
    }

    pub(crate) fn history() -> Self {
        Self {
            history: true,
            ..Self::default()
        }
    }

    /// A state change; repeats of the same state and line are dropped.
    pub(crate) fn state(&mut self, state: SessionState, line: Option<String>) -> Vec<EventKind> {
        let next = (state, line);
        if self.last_state.as_ref() == Some(&next) {
            return Vec::new();
        }
        self.last_state = Some(next.clone());
        vec![EventKind::State {
            state: next.0,
            status_line: next.1,
        }]
    }

    pub(crate) fn current_state(&self) -> Option<SessionState> {
        self.last_state.as_ref().map(|s| s.0)
    }

    /// One mux frame payload that belongs to this session.
    pub(crate) fn frame(&mut self, payload: &Value) -> Vec<EventKind> {
        match payload.get("type").and_then(Value::as_str) {
            Some("session/event") => match payload.get("event") {
                Some(event) => {
                    let mut event = event.clone();
                    if let Some(view) = payload.get("view") {
                        event["view"] = view.clone();
                    }
                    self.event(&event)
                }
                None => Vec::new(),
            },
            Some("approval/requested") => self.approval(payload),
            Some("approval/resolved") => {
                let Some(id) = str_at(payload, &["approvalId"]) else {
                    return Vec::new();
                };
                let decision = match str_at(payload, &["outcome"]) {
                    Some("allowed-once") => Decision::AllowOnce,
                    Some("rejected") => Decision::Deny,
                    _ => Decision::Abort,
                };
                let mut out = vec![EventKind::ApprovalDone {
                    approval: id.to_owned(),
                    decision,
                }];
                out.extend(self.state(SessionState::Working, None));
                out
            }
            Some("question/requested") => self.question(payload),
            Some("question/resolved") => self.state(SessionState::Working, None),
            Some("stream/error") => vec![EventKind::Notice {
                level: NoticeLevel::Error,
                text: format!(
                    "DSH 事件流出错：{}",
                    str_at(payload, &["error", "message"]).unwrap_or("未知错误")
                ),
            }],
            _ => Vec::new(),
        }
    }

    /// One durable session event (`{type, seq, time, data}`).
    pub(crate) fn event(&mut self, event: &Value) -> Vec<EventKind> {
        let data = event.get("data").cloned().unwrap_or(Value::Null);
        let turn = data.get("turn").and_then(Value::as_u64).unwrap_or(0);
        let step = data.get("step").and_then(Value::as_u64).unwrap_or(0);
        match event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "turn/start" if !self.history => self.state(SessionState::Working, None),
            "assistant/chunk" if !self.history => {
                let chunk = data.get("chunk").cloned().unwrap_or(Value::Null);
                match str_at(&chunk, &["type"]) {
                    Some("text-delta") => {
                        let text = str_at(&chunk, &["text"]).unwrap_or_default();
                        if text.is_empty() {
                            return Vec::new();
                        }
                        vec![EventKind::AssistantDelta {
                            item: format!("a{turn}.{step}"),
                            text: text.to_owned(),
                        }]
                    }
                    Some("reasoning-delta") => {
                        self.state(SessionState::Working, Some("思考中…".into()))
                    }
                    _ => Vec::new(),
                }
            }
            "user/message" if self.history => {
                if str_at(&data, &["source", "kind"]) != Some("user") {
                    return Vec::new();
                }
                let text = text_of(data.get("content"));
                if text.trim().is_empty() {
                    return Vec::new();
                }
                vec![EventKind::UserText { text, images: None }]
            }
            "assistant/message" => self.assistant(&data, turn, step),
            "tool/call" => self.tool_call(&data),
            "tool/result" => self.tool_result(&data, event.get("view")),
            "turn/end" => self.turn_end(&data),
            "llm/retry" if !self.history => {
                let retry = data.get("retry").and_then(Value::as_u64).unwrap_or(1);
                let max = data.get("maxRetries").and_then(Value::as_u64);
                let line = match max {
                    Some(max) => format!("模型暂时不可用，重试 {retry}/{max}…"),
                    None => format!("模型暂时不可用，重试第 {retry} 次…"),
                };
                self.state(SessionState::Working, Some(line))
            }
            "compaction/start" if !self.history => {
                self.state(SessionState::Working, Some("正在压缩上下文…".into()))
            }
            _ => Vec::new(),
        }
    }

    fn assistant(&mut self, data: &Value, turn: u64, step: u64) -> Vec<EventKind> {
        let parts = data
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut thinking = String::new();
        let mut text = String::new();
        for part in &parts {
            match str_at(part, &["type"]) {
                Some("text") => text.push_str(str_at(part, &["text"]).unwrap_or_default()),
                Some("reasoning" | "thinking") => thinking.push_str(
                    first_str(part, &["text", "reasoning", "thinking"]).unwrap_or_default(),
                ),
                Some("tool-call") => {
                    // `tool/call` follows with the same data; remember the
                    // arguments now so an approval can name the command.
                    if let (Some(id), Some(name)) = (str_at(part, &["id"]), str_at(part, &["name"]))
                    {
                        self.tools.insert(
                            id.to_owned(),
                            (name.to_owned(), args_of(part.get("arguments"))),
                        );
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        if !thinking.trim().is_empty() {
            out.push(EventKind::Thinking {
                item: format!("r{turn}.{step}"),
                text: thinking,
            });
        }
        if !text.trim().is_empty() {
            out.push(EventKind::AssistantText {
                item: format!("a{turn}.{step}"),
                text,
            });
        }
        out
    }

    fn tool_call(&mut self, data: &Value) -> Vec<EventKind> {
        let (Some(id), Some(name)) = (str_at(data, &["callId"]), str_at(data, &["name"])) else {
            return Vec::new();
        };
        let args = args_of(data.get("arguments"));
        self.tools
            .insert(id.to_owned(), (name.to_owned(), args.clone()));
        if HIDDEN_TOOLS.contains(&name) {
            self.hidden.insert(id.to_owned());
            return Vec::new();
        }
        let summary = first_str(
            &args,
            &[
                "command",
                "file_path",
                "path",
                "pattern",
                "query",
                "url",
                "description",
                "name",
            ],
        )
        .map(one_line)
        .unwrap_or_default();
        vec![EventKind::ToolStart {
            item: id.to_owned(),
            tool: name.to_owned(),
            summary,
        }]
    }

    fn tool_result(&mut self, data: &Value, view: Option<&Value>) -> Vec<EventKind> {
        let message = data.get("message").cloned().unwrap_or(Value::Null);
        let result = message
            .get("content")
            .and_then(Value::as_array)
            .and_then(|c| {
                c.iter()
                    .find(|p| str_at(p, &["type"]) == Some("tool-result"))
                    .cloned()
            })
            .unwrap_or(Value::Null);
        let Some(id) = str_at(&result, &["toolCallId"])
            .or_else(|| str_at(&message, &["source", "callId"]))
            .map(str::to_owned)
        else {
            return Vec::new();
        };
        if self.hidden.remove(&id) {
            return Vec::new();
        }
        // 0.1 wraps the result in a `tool-result` part; 0.2 puts `isError`
        // and the text parts on the message itself.
        let holder = if result.is_null() { &message } else { &result };
        let mut ok = holder.get("isError").and_then(Value::as_bool) != Some(true);
        let card = view.and_then(|v| v.get("view"));
        let output = match card.filter(|c| str_at(c, &["card"]) == Some("terminal")) {
            Some(card) => {
                if card.get("exitCode").and_then(Value::as_i64).unwrap_or(0) != 0 {
                    ok = false;
                }
                str_at(card, &["output"]).unwrap_or_default().to_owned()
            }
            None => text_of(holder.get("content")),
        };
        vec![EventKind::ToolEnd {
            item: id,
            ok,
            output: truncate_tail(output.trim_end(), MAX_TOOL_OUTPUT),
        }]
    }

    fn turn_end(&mut self, data: &Value) -> Vec<EventKind> {
        let mut out = Vec::new();
        match str_at(data, &["reason", "kind"]) {
            Some("aborted") if !self.history => out.push(EventKind::Notice {
                level: NoticeLevel::Info,
                text: "已停止".into(),
            }),
            Some("error") => {
                let message = str_at(data, &["reason", "error", "message"])
                    .or_else(|| str_at(data, &["reason", "message"]))
                    .unwrap_or("未知错误");
                let text =
                    if str_at(data, &["reason", "error", "code"]) == Some("MISSING_CREDENTIAL") {
                        crate::setup::missing_credential(message)
                    } else {
                        format!("DSH 这一轮出错：{message}")
                    };
                out.push(EventKind::Notice {
                    level: NoticeLevel::Error,
                    text,
                });
            }
            _ => {}
        }
        out.push(EventKind::TurnDone { summary: None });
        if !self.history {
            out.extend(self.state(SessionState::Idle, None));
        }
        out
    }

    fn approval(&mut self, payload: &Value) -> Vec<EventKind> {
        let Some(id) = str_at(payload, &["approvalId"]) else {
            return Vec::new();
        };
        let tool = str_at(payload, &["toolName"]).unwrap_or("工具");
        let reason = str_at(payload, &["reason"]).unwrap_or_default();
        let args = str_at(payload, &["callId"])
            .and_then(|c| self.tools.get(c))
            .map(|(_, a)| a.clone())
            .unwrap_or(Value::Null);
        let lower = tool.to_ascii_lowercase();
        let (kind, title, what) = if SHELL_TOOLS.contains(&lower.as_str()) {
            let command = first_str(&args, &["command"]).unwrap_or_default();
            (
                ApprovalKind::Command,
                format!("运行：{}", one_line(command)),
                command.to_owned(),
            )
        } else if FILE_TOOLS.contains(&lower.as_str()) {
            let path = first_str(&args, &["file_path", "path"]).unwrap_or_default();
            (
                ApprovalKind::FileChange,
                format!("修改：{path}"),
                serde_json::to_string_pretty(&args).unwrap_or_default(),
            )
        } else {
            (
                ApprovalKind::Tool,
                format!("使用工具：{tool}"),
                if args.is_null() {
                    String::new()
                } else {
                    serde_json::to_string_pretty(&args).unwrap_or_default()
                },
            )
        };
        let detail = [what.trim(), reason.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut out = vec![EventKind::Approval {
            approval: Approval {
                id: id.to_owned(),
                kind,
                title,
                detail,
                cwd: self.cwd.clone(),
                // DSH approves one operation at a time; Abort also stops the turn.
                choices: vec![Decision::AllowOnce, Decision::Deny, Decision::Abort],
            },
        }];
        out.extend(self.state(SessionState::NeedsYou, None));
        out
    }

    fn question(&mut self, payload: &Value) -> Vec<EventKind> {
        let questions = payload
            .get("questions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if questions.is_empty() {
            return Vec::new();
        }
        self.questions += 1;
        let mut text = String::new();
        for (i, q) in questions.iter().enumerate() {
            if i > 0 {
                text.push_str("\n\n");
            }
            if questions.len() > 1 {
                text.push_str(&format!("{}) ", i + 1));
            }
            if let Some(header) = str_at(q, &["header"]).filter(|h| !h.is_empty()) {
                text.push_str(&format!("**{header}**："));
            }
            text.push_str(str_at(q, &["question"]).unwrap_or_default());
            if let Some(detail) = str_at(q, &["detail"]).filter(|d| !d.is_empty()) {
                text.push_str(&format!("\n{detail}"));
            }
            let options = q.get("options").and_then(Value::as_array);
            for (n, option) in options.into_iter().flatten().enumerate() {
                text.push_str(&format!(
                    "\n{}. {}",
                    n + 1,
                    str_at(option, &["label"]).unwrap_or_default()
                ));
                if let Some(d) = str_at(option, &["description"]).filter(|d| !d.is_empty()) {
                    text.push_str(&format!(" — {d}"));
                }
            }
            if q.get("multiSelect").and_then(Value::as_bool) == Some(true) {
                text.push_str("\n（可多选，用逗号分开序号）");
            }
        }
        text.push_str(if questions.len() > 1 {
            "\n\n直接回复即可：每行回答一个问题，可以写序号或自己的答案。"
        } else {
            "\n\n直接回复即可：写序号或自己的答案。"
        });
        let mut out = vec![EventKind::AssistantText {
            item: format!("q{}", self.questions),
            text,
        }];
        out.extend(self.state(SessionState::NeedsYou, Some("DSH 在等你回答问题".into())));
        out
    }
}

/// The person's reply to `questions` (the `question/requested` list) as
/// DSH's `answers`: a number or a label picks options, anything else is the
/// person's own answer. With several questions, one line per question.
pub(crate) fn answers(questions: &[Value], reply: &str) -> Value {
    let lines: Vec<&str> = if questions.len() > 1 {
        reply
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect()
    } else {
        vec![reply.trim()]
    };
    let list = questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let id = str_at(q, &["id"]).unwrap_or_default();
            let line = lines.get(i).copied().unwrap_or_default();
            let line = line
                .strip_prefix(&format!("{})", i + 1))
                .unwrap_or(line)
                .trim();
            let labels: Vec<&str> = q
                .get("options")
                .and_then(Value::as_array)
                .map(|o| o.iter().filter_map(|x| str_at(x, &["label"])).collect())
                .unwrap_or_default();
            let multi = q.get("multiSelect").and_then(Value::as_bool) == Some(true);
            let pick = |token: &str| -> Option<String> {
                let token = token.trim().trim_end_matches(['.', '。']);
                if let Ok(n) = token.parse::<usize>() {
                    return labels.get(n.wrapping_sub(1)).map(|s| s.to_string());
                }
                labels
                    .iter()
                    .find(|l| l.eq_ignore_ascii_case(token))
                    .map(|s| s.to_string())
            };
            if line.is_empty() {
                return serde_json::json!({"id": id, "selected": []});
            }
            let tokens: Vec<&str> = if multi {
                line.split([',', '，', '、', ' '])
                    .filter(|t| !t.trim().is_empty())
                    .collect()
            } else {
                vec![line]
            };
            let picked: Option<Vec<String>> = tokens.iter().map(|t| pick(t)).collect();
            match picked {
                Some(selected) if !selected.is_empty() => {
                    serde_json::json!({"id": id, "selected": selected})
                }
                _ => serde_json::json!({"id": id, "selected": [], "custom": line}),
            }
        })
        .collect();
    Value::Array(list)
}

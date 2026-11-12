//! ACP content and patch semantics, independent of the process transport.

use std::collections::HashMap;

use serde_json::{Map, Value};
use tether_agent_api::AgentOutput;
use tether_proto::{EventKind, MAX_TOOL_OUTPUT, truncate_tail};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

pub(crate) fn emit(out: &UnboundedSender<AgentOutput>, event: EventKind) {
    let _ = out.send(AgentOutput::Event(event));
}

/// `truncate_tail` adds an ellipsis outside its byte budget. Reserve room for
/// that prefix here without changing the frozen proto implementation.
pub(crate) fn tool_tail(text: &str) -> String {
    if text.len() <= MAX_TOOL_OUTPUT {
        text.to_owned()
    } else {
        truncate_tail(text, MAX_TOOL_OUTPUT - '…'.len_utf8())
    }
}

#[derive(Default)]
pub(crate) struct Transcript {
    messages: Vec<Message>,
    thoughts: HashMap<String, String>,
    boundary: bool,
}

struct Message {
    native: Option<String>,
    item: String,
    text: String,
}

impl Transcript {
    pub(crate) fn chunk(&mut self, update: &Value, out: &UnboundedSender<AgentOutput>) {
        let Some(text) = content_text(&update["content"]) else {
            return;
        };
        let native = update["messageId"].as_str();
        let thought = update["sessionUpdate"] == "agent_thought_chunk";
        if thought {
            let item = self
                .thoughts
                .entry(native.unwrap_or("").to_owned())
                .or_insert_with(|| Uuid::new_v4().to_string());
            emit(
                out,
                EventKind::Thinking {
                    item: item.clone(),
                    text,
                },
            );
            return;
        }

        let position = match native {
            Some(id) => self
                .messages
                .iter()
                .position(|m| m.native.as_deref() == Some(id)),
            None if !self.boundary => self.messages.len().checked_sub(1),
            None => None,
        };
        let index = position.unwrap_or_else(|| {
            self.messages.push(Message {
                native: native.map(str::to_owned),
                item: Uuid::new_v4().to_string(),
                text: String::new(),
            });
            self.messages.len() - 1
        });
        self.boundary = false;
        let message = &mut self.messages[index];
        message.text.push_str(&text);
        emit(
            out,
            EventKind::AssistantDelta {
                item: message.item.clone(),
                text,
            },
        );
    }

    pub(crate) fn boundary(&mut self) {
        self.boundary = true;
    }

    pub(crate) fn finish(&mut self, out: &UnboundedSender<AgentOutput>) {
        for message in self.messages.drain(..) {
            emit(
                out,
                EventKind::AssistantText {
                    item: message.item,
                    text: message.text,
                },
            );
        }
        self.thoughts.clear();
        self.boundary = false;
    }
}

fn content_text(content: &Value) -> Option<String> {
    match content["type"].as_str()? {
        "text" => content["text"].as_str().map(str::to_owned),
        "resource" => content["resource"]["text"].as_str().map(str::to_owned),
        "resource_link" => content["uri"].as_str().map(str::to_owned),
        // Image/audio capabilities are not advertised. Do not fabricate text
        // for unsupported content blocks or assume they contain a `text` field.
        _ => None,
    }
}

#[derive(Default)]
pub(crate) struct Tools(HashMap<String, Tool>);

struct Tool {
    fields: Map<String, Value>,
    finished: bool,
}

impl Tools {
    pub(crate) fn apply(
        &mut self,
        update: &Value,
        out: &UnboundedSender<AgentOutput>,
    ) -> Option<Value> {
        let id = update["toolCallId"].as_str()?;
        let fields = update.as_object()?;
        let first = !self.0.contains_key(id);
        let tool = self.0.entry(id.to_owned()).or_insert_with(|| Tool {
            fields: Map::new(),
            finished: false,
        });
        // In v1 omitted/null fields leave previous values unchanged; content
        // is replaced (not appended), and [] explicitly clears it.
        for (key, value) in fields {
            if key != "sessionUpdate" && !value.is_null() {
                tool.fields.insert(key.clone(), value.clone());
            }
        }
        let snapshot = Value::Object(tool.fields.clone());
        if first {
            let title = snapshot["title"].as_str().unwrap_or(id);
            emit(
                out,
                EventKind::ToolStart {
                    item: id.to_owned(),
                    tool: snapshot["kind"].as_str().unwrap_or(title).to_owned(),
                    summary: title.to_owned(),
                },
            );
        }
        if !tool.finished && matches!(snapshot["status"].as_str(), Some("completed" | "failed")) {
            tool.finished = true;
            emit(
                out,
                EventKind::ToolEnd {
                    item: id.to_owned(),
                    ok: snapshot["status"] == "completed",
                    output: tool_tail(&tool_output(&snapshot)),
                },
            );
        }
        Some(snapshot)
    }

    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }
}

fn tool_output(tool: &Value) -> String {
    let content = tool["content"].as_array().map(|blocks| {
        blocks
            .iter()
            .filter_map(|block| match block["type"].as_str()? {
                "content" => content_text(&block["content"]),
                "diff" => Some(format!(
                    "{}\n---\n{}\n+++\n{}",
                    block["path"].as_str().unwrap_or_default(),
                    block["oldText"].as_str().unwrap_or_default(),
                    block["newText"].as_str().unwrap_or_default()
                )),
                "terminal" => block["terminalId"]
                    .as_str()
                    .map(|id| format!("terminal: {id}")),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    if let Some(text) = content.filter(|text| !text.is_empty()) {
        text
    } else {
        match &tool["rawOutput"] {
            Value::Null => String::new(),
            Value::String(text) => text.clone(),
            value => value.to_string(),
        }
    }
}

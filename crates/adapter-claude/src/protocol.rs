//! Claude's JSONL wire format. Keep permission replies identical to T05.

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tether_proto::Decision;
use tokio::io::{AsyncWrite, AsyncWriteExt};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Message {
    System {
        #[serde(default)]
        subtype: String,
        session_id: Option<String>,
    },
    StreamEvent {
        event: Value,
    },
    Assistant {
        message: Value,
    },
    User {
        message: Value,
    },
    Result {
        #[serde(flatten)]
        data: Map<String, Value>,
    },
    ControlRequest {
        request_id: String,
        request: Value,
    },
    ControlCancelRequest {
        request_id: String,
    },
    ControlResponse {
        response: ControlReply,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
pub(super) struct ControlReply {
    pub subtype: String,
    pub request_id: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct Permission {
    pub id: String,
    pub tool: String,
    pub input: Value,
    pub suggestions: Vec<Value>,
}

impl Permission {
    pub fn parse(id: String, request: &Value) -> Result<Self> {
        ensure!(!id.is_empty(), "empty permission request_id");
        let tool = request["tool_name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .context("can_use_tool has no tool_name")?;
        let input = request.get("input").context("can_use_tool has no input")?;
        ensure!(input.is_object(), "can_use_tool input must be an object");
        Ok(Self {
            id,
            tool: tool.into(),
            input: input.clone(),
            suggestions: request["permission_suggestions"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        })
    }

    pub fn reply(&self, decision: Decision) -> Result<Value> {
        let response = match decision {
            Decision::AllowOnce => json!({"behavior":"allow", "updatedInput":self.input}),
            Decision::AllowSession => {
                ensure!(
                    !self.suggestions.is_empty(),
                    "allow_session was not offered"
                );
                json!({"behavior":"allow", "updatedInput":self.input,
                    "updatedPermissions":self.suggestions})
            }
            Decision::Deny => json!({"behavior":"deny", "message":"用户在手机上拒绝"}),
            Decision::Abort => {
                json!({"behavior":"deny", "message":"用户在手机上拒绝并停止", "interrupt":true})
            }
        };
        Ok(json!({"type":"control_response", "response":{
            "subtype":"success", "request_id":self.id, "response":response
        }}))
    }
}

pub(super) fn arguments(resume: Option<&str>, extra: &[String]) -> Vec<String> {
    let mut args = [
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
    ]
    .map(str::to_owned)
    .to_vec();
    if let Some(id) = resume {
        args.push(format!("--resume={id}"));
    }
    args.extend_from_slice(extra);
    args
}

pub(super) fn initialize(id: &str) -> Value {
    json!({"type":"control_request", "request_id":id,
        "request":{"subtype":"initialize", "hooks":null}})
}

pub(super) fn interrupt(id: &str) -> Value {
    json!({"type":"control_request", "request_id":id, "request":{"subtype":"interrupt"}})
}

/// The Agent SDK's `setModel`; `null` goes back to the default.
pub(super) fn set_model(id: &str, model: Option<&str>) -> Value {
    json!({"type":"control_request", "request_id":id,
        "request":{"subtype":"set_model", "model":model}})
}

pub(super) fn user(text: &str) -> Value {
    json!({"type":"user", "message":{"role":"user", "content":text},
        "parent_tool_use_id":null, "session_id":""})
}

pub(super) async fn write(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .context("write Claude JSONL")?;
    writer.flush().await.context("flush Claude JSONL")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission(suggestions: Value) -> Permission {
        Permission::parse(
            "tool-1".into(),
            &json!({"tool_name":"Bash",
            "input":{"command":"echo \"你好\"", "nested":[1, true, null]},
            "permission_suggestions":suggestions}),
        )
        .unwrap()
    }

    #[test]
    fn exact_permission_replies_for_all_decisions() {
        let p = permission(json!([{"type":"addRules", "destination":"session", "rules":[]} ]));
        for (decision, response) in [
            (
                Decision::AllowOnce,
                json!({"behavior":"allow", "updatedInput":p.input}),
            ),
            (
                Decision::AllowSession,
                json!({"behavior":"allow", "updatedInput":p.input,
                "updatedPermissions":p.suggestions}),
            ),
            (
                Decision::Deny,
                json!({"behavior":"deny", "message":"用户在手机上拒绝"}),
            ),
            (
                Decision::Abort,
                json!({"behavior":"deny", "message":"用户在手机上拒绝并停止", "interrupt":true}),
            ),
        ] {
            assert_eq!(
                p.reply(decision).unwrap(),
                json!({"type":"control_response", "response":{
                "subtype":"success", "request_id":"tool-1", "response":response}})
            );
        }
    }

    #[test]
    fn session_permission_requires_nonempty_array() {
        for suggestions in [Value::Null, json!([]), json!({"unexpected":true})] {
            assert!(
                permission(suggestions)
                    .reply(Decision::AllowSession)
                    .is_err()
            );
        }
        assert!(Permission::parse("x".into(), &json!({"tool_name":"Bash"})).is_err());
        assert!(Permission::parse("x".into(), &json!({"tool_name":"Bash", "input":null})).is_err());
    }

    #[test]
    fn exact_startup_and_control_requests() {
        assert_eq!(
            arguments(None, &[]),
            [
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
                "--include-partial-messages",
                "--permission-prompt-tool",
                "stdio"
            ]
        );
        let args = arguments(
            Some("saved-id"),
            &["--model".into(), "fixture-model".into()],
        );
        assert_eq!(
            &args[8..],
            ["--resume=saved-id", "--model", "fixture-model"]
        );
        assert_eq!(
            initialize("req_1"),
            json!({"type":"control_request", "request_id":"req_1",
            "request":{"subtype":"initialize", "hooks":null}})
        );
        assert_eq!(
            interrupt("req_2"),
            json!({"type":"control_request", "request_id":"req_2",
            "request":{"subtype":"interrupt"}})
        );
        assert_eq!(
            user("hi\n你好"),
            json!({"type":"user", "message":{"role":"user", "content":"hi\n你好"},
            "parent_tool_use_id":null, "session_id":""})
        );
    }
}

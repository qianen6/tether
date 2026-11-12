//! The `dsh web` RPC envelope (DSH 0.1.x, see `fixtures/dsh/README.md`):
//! `POST /api/<method>` with `{type:"client-request", rpcId, method, payload}`,
//! answered by `{type:"server-response", rpcId, result:{ok, value|error}}`.
//! Answers to server requests (approvals, questions) go to `POST /api/respond`.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tether_agent_api::BoxFuture;

use crate::http;

/// Bounded calls; a prompt only queues, so it returns quickly too.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// An error DSH answered with (`result.ok == false`).
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DSH：{}（{}）", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// The code of an [`RpcError`] inside `error`, if it is one.
pub(crate) fn rpc_code(error: &anyhow::Error) -> Option<&str> {
    error.downcast_ref::<RpcError>().map(|e| e.code.as_str())
}

/// One `dsh web`, as the adapter needs it. Tests use a scripted fake.
pub(crate) trait Api: Send + Sync + 'static {
    /// The value of a successful call; `Value::Null` when there is none.
    fn call(&self, method: &str, payload: Value) -> BoxFuture<Value>;
    /// Answer a server request; `true` when DSH accepted the answer.
    fn respond(&self, message: Value) -> BoxFuture<bool>;
}

pub(crate) fn request(method: &str, payload: Value) -> Value {
    json!({
        "type": "client-request",
        "rpcId": uuid::Uuid::new_v4().to_string(),
        "method": method,
        "payload": payload,
    })
}

/// `result.value` of a server response, or its error.
pub(crate) fn unwrap_response(method: &str, response: &Value) -> Result<Value> {
    let result = response
        .get("result")
        .with_context(|| format!("DSH {method} 的回应缺少 result"))?;
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(result.get("value").cloned().unwrap_or(Value::Null));
    }
    let error = result.get("error").cloned().unwrap_or(Value::Null);
    Err(RpcError {
        code: error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("未知错误")
            .to_owned(),
    }
    .into())
}

pub(crate) fn approval_answer(rpc_id: &str, session: &str, approval: &str, outcome: &str) -> Value {
    json!({
        "type": "client-response",
        "rpcId": rpc_id,
        "result": {"ok": true, "value": {"sessionId": session, "approvalId": approval, "outcome": outcome}},
    })
}

pub(crate) fn question_answer(rpc_id: &str, session: &str, answers: Value) -> Value {
    json!({
        "type": "client-response",
        "rpcId": rpc_id,
        "result": {"ok": true, "value": {"sessionId": session, "answer": {"answers": answers}}},
    })
}

pub(crate) fn question_cancel(rpc_id: &str) -> Value {
    json!({
        "type": "client-response",
        "rpcId": rpc_id,
        "result": {"ok": false, "error": {"code": "cancelled", "message": "the user cancelled", "details": {}}},
    })
}

/// The real thing, over loopback HTTP.
pub(crate) struct HttpApi {
    pub origin: String,
}

impl HttpApi {
    pub(crate) fn new(origin: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            origin: origin.into(),
        })
    }
}

impl Api for HttpApi {
    fn call(&self, method: &str, payload: Value) -> BoxFuture<Value> {
        let origin = self.origin.clone();
        let method = method.to_owned();
        Box::pin(async move {
            let body = request(&method, payload);
            let response =
                http::post_json(&origin, &format!("/api/{method}"), &body, CALL_TIMEOUT).await?;
            unwrap_response(&method, &response)
        })
    }

    fn respond(&self, message: Value) -> BoxFuture<bool> {
        let origin = self.origin.clone();
        Box::pin(async move {
            let receipt = http::post_json(&origin, "/api/respond", &message, CALL_TIMEOUT).await?;
            Ok(receipt.get("accepted").and_then(Value::as_bool) == Some(true))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses() {
        let ok = json!({"type":"server-response","rpcId":"1","result":{"ok":true,"value":{"accepted":true}}});
        assert_eq!(
            unwrap_response("x", &ok).unwrap(),
            json!({"accepted": true})
        );
        let empty = json!({"type":"server-response","rpcId":"1","result":{"ok":true}});
        assert_eq!(unwrap_response("x", &empty).unwrap(), Value::Null);
        let err = json!({"result":{"ok":false,"error":{"code":"session-not-found","message":"session \"x\" not found"}}});
        let e = unwrap_response("session.prompt", &err).unwrap_err();
        assert_eq!(rpc_code(&e), Some("session-not-found"));
        assert!(e.to_string().contains("not found"));
    }
}

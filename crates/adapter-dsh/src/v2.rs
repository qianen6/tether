//! DSH 0.2+ `dsh web`: token → cookie, unary calls over HTTP, and every live
//! stream on one WebSocket (`/api/remote.mux`). Recorded in `fixtures/dsh/v2/`.
//!
//! - `GET /connect?token=…&interactions=answer` trades the launch token
//!   printed by `dsh web` for a cookie naming an API peer. The peer lives as
//!   long as the mux socket: when it closes, the cookie is spent.
//! - Unary: `POST /api/<ns>/<method>` with
//!   `{type:"client-request", rpcId, method, payload:{args:{<param>: …}}}`.
//! - Streams: `{type:"open", streamId, endpoint, payload:{args}}` up;
//!   `{type:"item"|"end"|"error", streamId, …}` down.
//!   `session/follow` carries one session's events; `$events` carries the
//!   approvals and questions DSH waits on (`waterfall`), answered with
//!   `POST /api/$events/result`.
//!
//! The rest of the adapter speaks DSH 0.1 (see [`crate::api::Api`]): calls
//! are mapped here, and frames are reshaped into 0.1's mux payloads so the
//! session loop and the translator serve both versions.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tether_agent_api::BoxFuture;
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::{
    Message,
    client::IntoClientRequest,
    http::{HeaderValue, header::COOKIE},
};

use crate::{
    api::{self, Api, RpcError},
    http,
    hub::Frame,
    translate::str_at,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Opening a cold session reads its log from disk.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(30);

/// `http://host:port/?token=…` (what `dsh web` prints) → origin and token.
pub(crate) fn split_url(url: &str) -> Option<(String, Option<String>)> {
    let url = url.trim();
    let rest = url.strip_prefix("http://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() {
        return None;
    }
    let origin = format!("http://{authority}");
    let query = rest[end..]
        .split_once('?')
        .map(|(_, q)| q.split('#').next().unwrap_or(q))
        .unwrap_or_default();
    let token = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == "token")
        .map(|(_, v)| v.to_owned())
        .filter(|t| !t.is_empty());
    Some((origin, token))
}

/// Trade the launch token for the API cookie.
pub(crate) async fn cookie(origin: &str, token: &str) -> Result<String> {
    let path = format!("/connect?token={token}&interactions=answer");
    let (status, cookies) = http::get_cookies(origin, &path, CONNECT_TIMEOUT).await?;
    match status {
        302 | 303 if !cookies.is_empty() => Ok(cookies.join("; ")),
        401 => bail!(
            "DSH 不认这个 token：这个地址是之前某次 dsh web 打印的，DSH 重启后 token 就变了。请用它这次打印的完整地址"
        ),
        403 => bail!("DSH 拒绝了连接（HTTP 403）：地址要写 127.0.0.1 或 localhost"),
        _ => bail!("用 token 连接 DSH 失败（HTTP {status}）"),
    }
}

// ------------------------------------------------------------ call mapping

/// A 0.1 call as the 0.2 endpoint and its named arguments.
pub(crate) fn request(method: &str, payload: Value) -> (String, Value) {
    let wrap =
        |endpoint: &str, payload: Value| (endpoint.to_owned(), json!({ "request": payload }));
    match method {
        "session.create" => wrap("session/create", payload),
        "session.cancel" => wrap("session/cancel", payload),
        "session.selectModel" => wrap("session/selectModel", payload),
        "session.search" => wrap("session/search", payload),
        "session.prompt" => {
            let mut payload = payload;
            if payload.get("requestId").is_none() {
                payload["requestId"] = json!(uuid::Uuid::new_v4().to_string());
            }
            wrap("session/prompt", payload)
        }
        "session.models" => wrap(
            "session/projections",
            json!({ "sessionId": payload.get("sessionId").cloned().unwrap_or(Value::Null) }),
        ),
        "session.history" => {
            let mut request = json!({
                "address": {
                    "kind": "session",
                    "sessionId": payload.get("sessionId").cloned().unwrap_or(Value::Null),
                },
            });
            if let Some(max) = payload.get("maxMessages") {
                request["maxMessages"] = max.clone();
            }
            wrap("session/events", request)
        }
        "session.list" => ("session/list".into(), json!({ "_request": {} })),
        "llm.models" | "host.describe" => ("session/modelCatalog".into(), json!({})),
        "llm.providers" => ("llm/listConfigurableProviders".into(), json!({})),
        "settings.describe" => ("settings/describe".into(), json!({})),
        "credentials.describe" => (
            "credentials/describe".into(),
            json!({ "refs": payload.get("refs").cloned().unwrap_or(json!([])) }),
        ),
        "commands/execute" => {
            let args = payload.get("args").cloned().unwrap_or(payload);
            (
                "commands/execute".into(),
                json!({
                    "agentId": args.get("agentId").cloned().unwrap_or(Value::Null),
                    "line": args.get("line").cloned().unwrap_or(Value::Null),
                    "submittedAttachments": [],
                }),
            )
        }
        other => (other.replace('.', "/"), payload),
    }
}

/// A 0.2 result in the shape the 0.1 call returned.
pub(crate) fn reshape(method: &str, value: Value) -> Value {
    match method {
        "host.describe" => value.get("default").cloned().unwrap_or(Value::Null),
        "session.models" => {
            let selection = value.pointer("/values/modelSelection");
            let current = selection
                .and_then(|s| s.get("next").filter(|n| !n.is_null()))
                .or_else(|| selection.and_then(|s| s.get("lastUsed")))
                .cloned()
                .unwrap_or(Value::Null);
            json!({ "current": current })
        }
        "session.history" => {
            let events: Vec<Value> = value
                .get("records")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|r| str_at(r, &["type"]) == Some("event"))
                .filter_map(|r| r.get("event").map(|e| json!({ "event": e })))
                .collect();
            json!({
                "events": events,
                "hasMore": value.get("hasMore").cloned().unwrap_or(json!(false)),
            })
        }
        "session.list" => {
            let mut value = value;
            if let Some(items) = value.get_mut("items").and_then(Value::as_array_mut) {
                for item in items {
                    let child = item
                        .pointer("/projections/values/subagent")
                        .is_some_and(|s| !s.is_null());
                    if child {
                        item["origin"] = json!("subagent");
                    }
                }
            }
            value
        }
        "llm.providers" => json!({ "providers": value }),
        "credentials.describe" => json!({ "credentials": value }),
        _ => value,
    }
}

/// 0.2 error codes the adapter reacts to, in their 0.1 spelling.
fn code_v1(code: &str) -> String {
    match code {
        "session/not-found" => "session-not-found".into(),
        other => other.to_owned(),
    }
}

// ------------------------------------------------------------- the router

#[derive(Debug, Clone, PartialEq)]
enum Stream {
    Events,
    Follow(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Ask {
    Approval { call_id: Option<String> },
    Question,
}

#[derive(Debug, Clone)]
struct Pending {
    session: String,
    ask: Ask,
}

/// What one downlink frame asks of the connection.
#[derive(Debug, Default)]
pub(crate) struct Routed {
    pub frames: Vec<Frame>,
    /// `$events/result` arguments to send right away (events nobody here
    /// answers are passed on).
    pub answers: Vec<Value>,
    /// A follow that opened (`Ok`) or failed to (`Err`): its stream id.
    pub opened: Vec<(String, Result<(), RpcError>)>,
}

/// Bookkeeping of one mux connection: which stream is what, and which
/// questions DSH is waiting on. Pure, so recordings can drive it.
#[derive(Default)]
pub(crate) struct Router {
    streams: HashMap<String, Stream>,
    /// session → (follow stream id, sessions in Tether using it)
    follows: HashMap<String, (String, usize)>,
    /// Follows not yet answered with their first item.
    opening: HashMap<String, ()>,
    client_id: Option<String>,
    /// Waterfall event id → what it asks.
    pending: HashMap<String, Pending>,
    /// `approval/asked` id → tool call id, to match `approval/decided`.
    asked: HashMap<String, String>,
    /// Live reply attempt → (turn, step).
    attempts: HashMap<String, (Value, Value)>,
    next: u64,
}

fn mux(payload: Value) -> Frame {
    Frame::Mux {
        rpc_id: String::new(),
        payload,
    }
}

impl Router {
    fn stream_id(&mut self) -> String {
        self.next += 1;
        format!("t{}", self.next)
    }

    /// The `$events` stream's open message.
    pub(crate) fn open_events(&mut self) -> Value {
        let id = self.stream_id();
        self.streams.insert(id.clone(), Stream::Events);
        json!({ "type": "open", "streamId": id, "endpoint": "$events", "payload": { "args": {} } })
    }

    /// Follow `session`: the open message when a new stream is needed, and
    /// the stream id to wait on.
    pub(crate) fn follow(&mut self, session: &str) -> (Option<Value>, String) {
        if let Some((id, users)) = self.follows.get_mut(session) {
            *users += 1;
            return (None, id.clone());
        }
        let id = self.stream_id();
        self.streams
            .insert(id.clone(), Stream::Follow(session.to_owned()));
        self.follows.insert(session.to_owned(), (id.clone(), 1));
        self.opening.insert(id.clone(), ());
        let open = json!({
            "type": "open",
            "streamId": id,
            "endpoint": "session/follow",
            "payload": { "args": { "request": {
                "address": { "kind": "session", "sessionId": session },
                "activate": true,
                "assistantStream": true,
            } } },
        });
        (Some(open), id)
    }

    /// One fewer user of `session`'s follow: the cancel message when it was
    /// the last.
    pub(crate) fn unfollow(&mut self, session: &str) -> Option<Value> {
        let (id, users) = self.follows.get_mut(session)?;
        *users -= 1;
        if *users > 0 {
            return None;
        }
        let id = id.clone();
        self.follows.remove(session);
        self.streams.remove(&id);
        self.opening.remove(&id);
        Some(json!({ "type": "cancel", "streamId": id }))
    }

    pub(crate) fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }

    /// Our answer to `event_id` went through: forget it and say how it ended.
    pub(crate) fn answered(&mut self, event_id: &str, outcome: Option<&str>) -> Option<Frame> {
        let pending = self.pending.remove(event_id)?;
        Some(resolved(event_id, &pending, outcome))
    }

    pub(crate) fn is_pending(&self, event_id: &str) -> bool {
        self.pending.contains_key(event_id)
    }

    /// One downlink message.
    pub(crate) fn route(&mut self, message: &Value) -> Routed {
        let mut out = Routed::default();
        let Some(id) = str_at(message, &["streamId"]) else {
            return out;
        };
        let Some(stream) = self.streams.get(id).cloned() else {
            return out;
        };
        match str_at(message, &["type"]) {
            Some("item") => {
                let value = message.get("value").cloned().unwrap_or(Value::Null);
                match stream {
                    Stream::Events => self.event_item(&value, &mut out),
                    Stream::Follow(session) => {
                        if self.opening.remove(id).is_some() {
                            out.opened.push((id.to_owned(), Ok(())));
                        }
                        self.follow_item(&session, &value, &mut out);
                    }
                }
            }
            kind @ (Some("end") | Some("error")) => {
                let error = message.get("error");
                let failure = RpcError {
                    code: code_v1(error.and_then(|e| str_at(e, &["code"])).unwrap_or("ended")),
                    message: error
                        .and_then(|e| str_at(e, &["message"]))
                        .unwrap_or("DSH 结束了这条事件流")
                        .to_owned(),
                };
                self.streams.remove(id);
                match stream {
                    Stream::Events => {
                        self.client_id = None;
                        out.frames.push(Frame::Down(format!(
                            "DSH 的事件流断了：{}",
                            failure.message
                        )));
                    }
                    Stream::Follow(session) => {
                        self.follows.remove(&session);
                        if self.opening.remove(id).is_some() {
                            out.opened.push((id.to_owned(), Err(failure)));
                        } else if kind == Some("error") {
                            out.frames.push(mux(json!({
                                "type": "stream/error",
                                "sessionId": session,
                                "error": { "message": failure.message },
                            })));
                        }
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn event_item(&mut self, value: &Value, out: &mut Routed) {
        match str_at(value, &["type"]) {
            Some("ready") => self.client_id = str_at(value, &["clientId"]).map(str::to_owned),
            Some("waterfall") => {
                let (Some(event_id), Some(event)) =
                    (str_at(value, &["eventId"]), str_at(value, &["event"]))
                else {
                    return;
                };
                let session = str_at(value, &["agentId"]).unwrap_or_default().to_owned();
                let request = value.get("request").cloned().unwrap_or(Value::Null);
                match event {
                    "approval/request" => {
                        let call_id = str_at(&request, &["callId"]).map(str::to_owned);
                        let reason = str_at(&request, &["displayReason", "zh"])
                            .or_else(|| str_at(&request, &["reason"]))
                            .unwrap_or_default();
                        out.frames.push(Frame::Mux {
                            rpc_id: event_id.to_owned(),
                            payload: json!({
                                "type": "approval/requested",
                                "sessionId": session,
                                "approvalId": event_id,
                                "toolName": str_at(&request, &["toolName"]),
                                "callId": call_id,
                                "reason": reason,
                            }),
                        });
                        self.pending.insert(
                            event_id.to_owned(),
                            Pending {
                                session,
                                ask: Ask::Approval { call_id },
                            },
                        );
                    }
                    "user-questions/request" => {
                        out.frames.push(Frame::Mux {
                            rpc_id: event_id.to_owned(),
                            payload: json!({
                                "type": "question/requested",
                                "sessionId": session,
                                "questions": request.get("questions").cloned().unwrap_or(json!([])),
                            }),
                        });
                        self.pending.insert(
                            event_id.to_owned(),
                            Pending {
                                session,
                                ask: Ask::Question,
                            },
                        );
                    }
                    _ => {
                        if let Some(client) = &self.client_id {
                            out.answers.push(json!({
                                "clientId": client,
                                "eventId": event_id,
                                "outcome": { "kind": "next" },
                            }));
                        }
                    }
                }
            }
            // Settled elsewhere (DSH's own window, a timeout, the turn
            // ending). An approval waits for `approval/decided` to say how.
            Some("cancel") => {
                let Some(event_id) = str_at(value, &["eventId"]) else {
                    return;
                };
                if self
                    .pending
                    .get(event_id)
                    .is_some_and(|p| p.ask == Ask::Question)
                    && let Some(frame) = self.answered(event_id, None)
                {
                    out.frames.push(frame);
                }
            }
            Some("emit") => {
                let args = value.get("args").and_then(Value::as_array);
                let first = args
                    .and_then(|a| a.first())
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match str_at(value, &["event"]) {
                    Some("api-session/removed") => out.frames.push(Frame::Host(json!({
                        "type": "host/session-removed",
                        "sessionId": first,
                    }))),
                    Some("api-session/error") => {
                        let message = args
                            .and_then(|a| a.get(1))
                            .map(|m| match m {
                                Value::String(s) => s.clone(),
                                other => str_at(other, &["message"])
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| other.to_string()),
                            })
                            .unwrap_or_else(|| "DSH 出错".into());
                        out.frames.push(Frame::Host(json!({
                            "type": "host/agent-error",
                            "sessionId": first,
                            "message": message,
                        })));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn follow_item(&mut self, session: &str, value: &Value, out: &mut Routed) {
        match str_at(value, &["type"]) {
            Some("event") => {
                let Some(event) = value.get("event") else {
                    return;
                };
                match str_at(event, &["type"]) {
                    Some("approval/asked") => {
                        if let (Some(id), Some(call)) = (
                            str_at(event, &["data", "id"]),
                            str_at(event, &["data", "callId"]),
                        ) {
                            self.asked.insert(id.to_owned(), call.to_owned());
                        }
                    }
                    Some("approval/decided") => {
                        let call =
                            str_at(event, &["data", "id"]).and_then(|id| self.asked.remove(id));
                        let outcome = str_at(event, &["data", "outcome"]);
                        let event_id = self.pending.iter().find_map(|(eid, p)| {
                            (p.session == session
                                && matches!(&p.ask, Ask::Approval { call_id } if call_id.is_some() && *call_id == call))
                                .then(|| eid.clone())
                        });
                        if let Some(event_id) = event_id
                            && let Some(frame) = self.answered(&event_id, outcome)
                        {
                            out.frames.push(frame);
                        }
                    }
                    _ => {}
                }
                out.frames.push(mux(json!({
                    "type": "session/event",
                    "sessionId": session,
                    "event": event,
                })));
            }
            Some("assistant-stream") => {
                let frame = value.get("frame").cloned().unwrap_or(Value::Null);
                let attempt = str_at(&frame, &["attemptId"])
                    .unwrap_or_default()
                    .to_owned();
                match str_at(&frame, &["type"]) {
                    Some("start") => {
                        let turn = frame.get("turn").cloned().unwrap_or(Value::Null);
                        let step = frame.get("step").cloned().unwrap_or(Value::Null);
                        self.attempts.insert(attempt, (turn, step));
                    }
                    Some("chunk") => {
                        let Some((turn, step)) = self.attempts.get(&attempt) else {
                            return;
                        };
                        out.frames.push(mux(json!({
                            "type": "session/event",
                            "sessionId": session,
                            "event": {
                                "type": "assistant/chunk",
                                "data": {
                                    "turn": turn,
                                    "step": step,
                                    "chunk": frame.get("chunk").cloned().unwrap_or(Value::Null),
                                },
                            },
                        })));
                    }
                    Some(_) => {
                        self.attempts.remove(&attempt);
                    }
                    None => {}
                }
            }
            // `snapshot` opens the follow with history the session has
            // already shown (or read through `read_history`).
            _ => {}
        }
    }
}

fn resolved(event_id: &str, pending: &Pending, outcome: Option<&str>) -> Frame {
    let payload = match pending.ask {
        Ask::Approval { .. } => json!({
            "type": "approval/resolved",
            "sessionId": pending.session,
            "approvalId": event_id,
            "outcome": outcome.unwrap_or("cancelled"),
        }),
        Ask::Question => json!({
            "type": "question/resolved",
            "sessionId": pending.session,
        }),
    };
    Frame::Mux {
        rpc_id: event_id.to_owned(),
        payload,
    }
}

/// A 0.1 `client-response` answer as the `$events/result` outcome.
pub(crate) fn outcome_of(message: &Value) -> Value {
    if message.pointer("/result/ok") != Some(&json!(true)) {
        return json!({
            "kind": "rejected",
            "error": {
                "name": "UserQuestionError",
                "message": str_at(message, &["result", "error", "message"]).unwrap_or("the user cancelled"),
                "code": "ASK_ABORTED",
            },
        });
    }
    let value = message
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(outcome) = value.get("outcome") {
        return json!({ "kind": "result", "value": outcome });
    }
    json!({ "kind": "result", "value": value.get("answer").cloned().unwrap_or(Value::Null) })
}

// ---------------------------------------------------------- the connection

/// Unary transport: one envelope to one endpoint. Tests use a fake.
pub(crate) trait Rpc: Send + Sync + 'static {
    fn post(&self, endpoint: &str, envelope: Value) -> BoxFuture<Value>;
}

struct HttpRpc {
    origin: String,
    cookie: String,
}

impl Rpc for HttpRpc {
    fn post(&self, endpoint: &str, envelope: Value) -> BoxFuture<Value> {
        let origin = self.origin.clone();
        let cookie = self.cookie.clone();
        let path = format!("/api/{endpoint}");
        Box::pin(async move {
            http::post_json_with(&origin, &path, &envelope, CALL_TIMEOUT, Some(&cookie)).await
        })
    }
}

type Waiters = HashMap<String, oneshot::Sender<Result<(), RpcError>>>;

/// [`Api`] over one 0.2 connection. Cheap to clone; futures own a handle.
#[derive(Clone)]
pub(crate) struct V2Api(Arc<Inner>);

struct Inner {
    rpc: Arc<dyn Rpc>,
    router: Mutex<Router>,
    waiters: Mutex<Waiters>,
    up: mpsc::UnboundedSender<String>,
    frames: broadcast::Sender<Arc<Frame>>,
}

impl Inner {
    fn router(&self) -> std::sync::MutexGuard<'_, Router> {
        self.router.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn unary(&self, endpoint: &str, args: Value) -> Result<Value> {
        let envelope = json!({
            "type": "client-request",
            "rpcId": uuid::Uuid::new_v4().to_string(),
            "method": endpoint,
            "payload": { "args": args },
        });
        let response = self.rpc.post(endpoint, envelope).await?;
        api::unwrap_response(endpoint, &response).map_err(|error| {
            match error.downcast::<RpcError>() {
                Ok(rpc) => RpcError {
                    code: code_v1(&rpc.code),
                    message: rpc.message,
                }
                .into(),
                Err(other) => other,
            }
        })
    }
}

impl V2Api {
    pub(crate) fn new(
        rpc: Arc<dyn Rpc>,
        up: mpsc::UnboundedSender<String>,
        frames: broadcast::Sender<Arc<Frame>>,
    ) -> Self {
        Self(Arc::new(Inner {
            rpc,
            router: Mutex::default(),
            waiters: Mutex::default(),
            up,
            frames,
        }))
    }

    /// Open the `$events` stream (call once, first).
    pub(crate) fn start(&self) {
        let open = self.0.router().open_events();
        let _ = self.0.up.send(open.to_string());
    }

    /// One downlink text message.
    pub(crate) fn receive(&self, text: &str) {
        let Ok(message) = serde_json::from_str::<Value>(text) else {
            tracing::debug!("dsh mux: not JSON: {text}");
            return;
        };
        let routed = self.0.router().route(&message);
        for frame in routed.frames {
            let _ = self.0.frames.send(Arc::new(frame));
        }
        for (stream, result) in routed.opened {
            let waiter = self
                .0
                .waiters
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&stream);
            if let Some(waiter) = waiter {
                let _ = waiter.send(result);
            }
        }
        for args in routed.answers {
            let inner = self.0.clone();
            tokio::spawn(async move {
                if let Err(error) = inner.unary("$events/result", args).await {
                    tracing::debug!("passing on a DSH event failed: {error:#}");
                }
            });
        }
    }
}

impl Api for V2Api {
    fn call(&self, method: &str, payload: Value) -> BoxFuture<Value> {
        let (endpoint, args) = request(method, payload);
        let method = method.to_owned();
        let inner = self.0.clone();
        Box::pin(async move {
            let value = inner.unary(&endpoint, args).await?;
            Ok(reshape(&method, value))
        })
    }

    fn respond(&self, message: Value) -> BoxFuture<bool> {
        let event_id = str_at(&message, &["rpcId"]).unwrap_or_default().to_owned();
        let (client, pending) = {
            let router = self.0.router();
            (
                router.client_id().map(str::to_owned),
                router.is_pending(&event_id),
            )
        };
        let outcome = outcome_of(&message);
        let label = outcome
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let inner = self.0.clone();
        Box::pin(async move {
            if !pending {
                return Ok(false);
            }
            let client = client.context("还没连上 DSH 的事件流，稍后再试")?;
            inner
                .unary(
                    "$events/result",
                    json!({ "clientId": client, "eventId": event_id, "outcome": outcome }),
                )
                .await?;
            let frame = inner.router().answered(&event_id, label.as_deref());
            if let Some(frame) = frame {
                let _ = inner.frames.send(Arc::new(frame));
            }
            Ok(true)
        })
    }

    fn attach(&self, session: &str) -> BoxFuture<()> {
        let (open, stream) = self.0.router().follow(session);
        let wait = open.map(|open| {
            let (tx, rx) = oneshot::channel();
            self.0
                .waiters
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(stream.clone(), tx);
            let _ = self.0.up.send(open.to_string());
            rx
        });
        let session = session.to_owned();
        Box::pin(async move {
            let Some(rx) = wait else {
                return Ok(());
            };
            match tokio::time::timeout(ATTACH_TIMEOUT, rx).await {
                Ok(Ok(Ok(()))) => Ok(()),
                Ok(Ok(Err(error))) => Err(anyhow::Error::from(error))
                    .with_context(|| format!("打开 DSH 会话 {session} 失败")),
                Ok(Err(_)) => bail!("与 DSH 的连接已关闭"),
                Err(_) => bail!(
                    "打开 DSH 会话 {session} 超时（{} 秒）",
                    ATTACH_TIMEOUT.as_secs()
                ),
            }
        })
    }

    fn detach(&self, session: &str) {
        let cancel = self.0.router().unfollow(session);
        if let Some(cancel) = cancel {
            let _ = self.0.up.send(cancel.to_string());
        }
    }
}

/// A live 0.2 connection: its [`Api`], frames and liveness.
pub(crate) struct Opened {
    pub api: V2Api,
    pub frames: broadcast::Sender<Arc<Frame>>,
    pub alive: Arc<AtomicBool>,
    pub tasks: Vec<JoinHandle<()>>,
}

/// Connect to the `dsh web` at `origin` with its launch `token`.
pub(crate) async fn open(origin: &str, token: &str) -> Result<Opened> {
    let authority = http::authority(origin)?.to_owned();
    let cookie = cookie(origin, token).await?;
    let mut request = format!("ws://{authority}/api/remote.mux")
        .into_client_request()
        .context("DSH 地址无效")?;
    request.headers_mut().insert(
        COOKIE,
        HeaderValue::from_str(&cookie).context("DSH 的 cookie 无效")?,
    );
    let (socket, _) =
        tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request))
            .await
            .context("连接 DSH 的事件流超时")?
            .context("连接 DSH 的事件流失败")?;
    let (mut sink, mut source) = socket.split();
    let (frames, _) = broadcast::channel(4096);
    let (up, mut up_rx) = mpsc::unbounded_channel::<String>();
    let api = V2Api::new(
        Arc::new(HttpRpc {
            origin: origin.to_owned(),
            cookie,
        }),
        up,
        frames.clone(),
    );
    let alive = Arc::new(AtomicBool::new(true));
    let writer = tokio::spawn(async move {
        while let Some(text) = up_rx.recv().await {
            if sink.send(Message::text(text)).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let reader = {
        let api = api.clone();
        let frames = frames.clone();
        let alive = alive.clone();
        tokio::spawn(async move {
            let reason = loop {
                match source.next().await {
                    Some(Ok(Message::Text(text))) => api.receive(&text),
                    Some(Ok(Message::Close(_))) | None => break "DSH 关闭了连接".to_owned(),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => break format!("与 DSH 的连接断开：{error}"),
                }
            };
            alive.store(false, Ordering::SeqCst);
            let _ = frames.send(Arc::new(Frame::Down(reason)));
        })
    };
    api.start();
    Ok(Opened {
        api,
        frames,
        alive,
        tasks: vec![writer, reader],
    })
}

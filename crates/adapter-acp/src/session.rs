//! One connection, one session, and at most one ACP v1 prompt in flight.
//! Requests in both directions remain independently correlated, including
//! permission requests received while waiting for a prompt response.

use std::{collections::VecDeque, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tether_agent_api::{AgentCommand, AgentOutput};
use tether_proto::{Approval, ApprovalKind, Decision, EventKind, NoticeLevel, SessionState};
use tokio::{
    io::{AsyncBufRead, AsyncWrite},
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
    time::Instant,
};
use uuid::Uuid;

use crate::{
    rpc,
    updates::{Tools, Transcript, emit},
};

const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq)]
pub(crate) enum End {
    Shutdown,
    Eof,
}

struct ReaderTask(JoinHandle<()>);

impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) async fn run<R, W>(
    reader: R,
    writer: W,
    cwd: PathBuf,
    resume: Option<String>,
    mut commands: UnboundedReceiver<AgentCommand>,
    out: UnboundedSender<AgentOutput>,
) -> Result<End>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let (frames, mut messages) = mpsc::channel(32);
    let _reader = ReaderTask(tokio::spawn(rpc::read(reader, frames)));
    let mut client = Client::new(writer, cwd, resume, out)?;
    let result = client.run(&mut commands, &mut messages).await;
    if let Err(error) = &result {
        // A broken transport cannot accept cancellation replies, but the UI
        // must still retire every outstanding approval before Exited.
        for permission in std::mem::take(&mut client.permissions) {
            client.approval_done(permission.id, Decision::Abort);
        }
        client.transcript.finish(&client.out);
        client.notice(NoticeLevel::Error, format!("{error:#}"));
        client.state(SessionState::Error);
    }
    result
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Request {
    Initialize,
    New,
    Load,
    Prompt,
}

struct Pending {
    id: u64,
    kind: Request,
}

struct Permission {
    id: String,
    rpc_id: Value,
    options: Vec<(Decision, String)>,
}

struct Queued {
    text: String,
    needs_working: bool,
}

struct Client<W> {
    writer: W,
    out: UnboundedSender<AgentOutput>,
    cwd: String,
    resume: Option<String>,
    session: Option<String>,
    ready: bool,
    auth_methods: bool,
    next_id: u64,
    pending: Option<Pending>,
    deadline: Option<Instant>,
    queued: VecDeque<Queued>,
    active: bool,
    cancelling: bool,
    permissions: Vec<Permission>,
    transcript: Transcript,
    tools: Tools,
}

impl<W: AsyncWrite + Unpin> Client<W> {
    fn new(
        writer: W,
        cwd: PathBuf,
        resume: Option<String>,
        out: UnboundedSender<AgentOutput>,
    ) -> Result<Self> {
        Ok(Self {
            writer,
            out,
            cwd: cwd.to_str().context("ACP cwd is not UTF-8")?.to_owned(),
            resume,
            session: None,
            ready: false,
            auth_methods: false,
            next_id: 1,
            pending: None,
            deadline: None,
            queued: VecDeque::new(),
            active: false,
            cancelling: false,
            permissions: Vec::new(),
            transcript: Transcript::default(),
            tools: Tools::default(),
        })
    }

    async fn run(
        &mut self,
        commands: &mut UnboundedReceiver<AgentCommand>,
        messages: &mut mpsc::Receiver<Result<Value>>,
    ) -> Result<End> {
        self.request(
            Request::Initialize,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {"readTextFile":false, "writeTextFile":false},
                    "terminal":false
                },
                "clientInfo": {"name":"tether", "version":env!("CARGO_PKG_VERSION")}
            }),
        )
        .await?;

        loop {
            let deadline = self.deadline.unwrap_or_else(Instant::now);
            tokio::select! {
                command = commands.recv() => {
                    if self.command(command).await? {
                        return Ok(End::Shutdown);
                    }
                }
                message = messages.recv() => {
                    match message {
                        Some(message) => self.message(message?).await?,
                        None if self.pending.is_some() => bail!("ACP stdout closed with a request in flight"),
                        None => return Ok(End::Eof),
                    }
                }
                _ = tokio::time::sleep_until(deadline), if self.deadline.is_some() => {
                    bail!("ACP {:?} response timed out", self.pending.as_ref().map(|p| p.kind));
                }
                _ = self.out.closed() => {
                    self.cancel().await?;
                    return Ok(End::Shutdown);
                }
            }
            self.start_next().await?;
        }
    }

    fn state(&self, state: SessionState) {
        emit(
            &self.out,
            EventKind::State {
                state,
                status_line: None,
            },
        );
    }

    fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        emit(
            &self.out,
            EventKind::Notice {
                level,
                text: text.into(),
            },
        );
    }

    async fn request(&mut self, kind: Request, method: &str, params: Value) -> Result<()> {
        ensure!(self.pending.is_none(), "ACP request already in flight");
        let id = self.next_id;
        self.next_id = id.checked_add(1).context("ACP request id exhausted")?;
        rpc::write(
            &mut self.writer,
            json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}),
        )
        .await?;
        self.pending = Some(Pending { id, kind });
        self.deadline = (kind != Request::Prompt).then(|| Instant::now() + SETUP_TIMEOUT);
        Ok(())
    }

    async fn command(&mut self, command: Option<AgentCommand>) -> Result<bool> {
        match command {
            Some(AgentCommand::Send { text, images }) => {
                self.state(SessionState::Working);
                self.queued.push_back(Queued {
                    text: tether_agent_api::with_paths(&text, &images),
                    needs_working: self.active || !self.queued.is_empty(),
                });
                if !self.permissions.is_empty() {
                    self.state(SessionState::NeedsYou);
                }
            }
            Some(AgentCommand::Answer { approval, decision }) => {
                self.answer(approval, decision).await?
            }
            Some(AgentCommand::Interrupt) => self.cancel().await?,
            Some(AgentCommand::SetModel { .. }) => self.notice(
                NoticeLevel::Info,
                "这个 agent 不支持在手机上切换模型，请在 agent 自己的设置里修改",
            ),
            Some(AgentCommand::Shutdown) | None => {
                self.cancel().await?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn start_next(&mut self) -> Result<()> {
        if !self.ready || self.pending.is_some() {
            return Ok(());
        }
        if let Some(queued) = self.queued.pop_front() {
            if queued.needs_working {
                self.state(SessionState::Working);
            }
            self.active = true;
            self.cancelling = false;
            self.tools.clear();
            self.request(
                Request::Prompt,
                "session/prompt",
                json!({"sessionId":self.session, "prompt":[{"type":"text", "text":queued.text}]}),
            )
            .await?;
        }
        Ok(())
    }

    async fn message(&mut self, value: Value) -> Result<()> {
        ensure!(value.is_object(), "ACP JSON-RPC message is not an object");
        ensure!(
            value["jsonrpc"] == "2.0",
            "ACP JSON-RPC version must be 2.0"
        );
        if let Some(method) = value.get("method") {
            let method = method.as_str().context("ACP method is not a string")?;
            if let Some(id) = value.get("id") {
                if !rpc::valid_id(id) {
                    return rpc::error(&mut self.writer, Value::Null, -32600, "Invalid request id")
                        .await;
                }
                return self
                    .agent_request(method, id.clone(), &value["params"])
                    .await;
            }
            match method {
                "session/update" => self.update(&value["params"]),
                "$/cancel_request" => self.cancel_request(&value["params"]).await?,
                _ => {} // Unknown notifications, unlike requests, have no response.
            }
            return Ok(());
        }

        ensure!(
            value.get("result").is_some() != value.get("error").is_some(),
            "invalid ACP response envelope"
        );
        let Some(pending) = &self.pending else {
            self.notice(NoticeLevel::Warn, "已忽略未知的 ACP 响应");
            return Ok(());
        };
        if value["id"].as_u64() != Some(pending.id) {
            self.notice(NoticeLevel::Warn, "已忽略不匹配的 ACP 响应 ID");
            return Ok(());
        }
        let kind = self
            .pending
            .take()
            .context("missing ACP pending request")?
            .kind;
        self.deadline = None;
        if let Some(error) = value.get("error") {
            return self.response_error(kind, error).await;
        }
        let result = &value["result"];
        ensure!(result.is_object(), "ACP result must be an object");
        match kind {
            Request::Initialize => self.initialized(result).await?,
            Request::New => {
                self.session = Some(
                    result["sessionId"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .context("ACP session/new missing sessionId")?
                        .to_owned(),
                );
                self.session_ready()?;
            }
            Request::Load => {
                self.transcript.finish(&self.out);
                self.session_ready()?;
            }
            Request::Prompt => {
                let reason = result["stopReason"]
                    .as_str()
                    .context("ACP prompt response missing stopReason")?;
                self.finish_turn(reason).await?;
            }
        }
        Ok(())
    }

    async fn initialized(&mut self, result: &Value) -> Result<()> {
        ensure!(
            result["protocolVersion"].as_u64() == Some(1),
            "agent selected an unsupported ACP protocolVersion (only v1 is supported)"
        );
        self.auth_methods = result["authMethods"]
            .as_array()
            .is_some_and(|a| !a.is_empty());
        let can_load = result["agentCapabilities"]["loadSession"] == true;
        let mut params = json!({"cwd":self.cwd, "mcpServers":[]});
        if let Some(resume) = self.resume.take() {
            if can_load {
                params["sessionId"] = json!(resume);
                self.session = Some(resume);
                return self.request(Request::Load, "session/load", params).await;
            }
            self.notice(NoticeLevel::Info, "该 agent 不支持恢复，已开始新会话");
        }
        self.request(Request::New, "session/new", params).await
    }

    fn session_ready(&mut self) -> Result<()> {
        let id = self.session.clone().context("missing ACP session id")?;
        self.ready = true;
        let _ = self.out.send(AgentOutput::NativeId(id));
        Ok(())
    }

    async fn response_error(&mut self, kind: Request, error: &Value) -> Result<()> {
        let code = error["code"]
            .as_i64()
            .context("ACP error missing integer code")?;
        let message = error["message"]
            .as_str()
            .context("ACP error missing message")?;
        let text = if code == -32000 {
            // The error code is authoritative even if the agent forgot to
            // advertise its authMethods. Never start an interactive login.
            tracing::debug!(
                advertised_auth = self.auth_methods,
                "ACP authentication required"
            );
            "请先在电脑上登录该 agent".to_owned()
        } else {
            format!("ACP 请求失败 ({code}): {message}")
        };
        if kind != Request::Prompt {
            bail!("{text}");
        }
        if code != -32800 {
            self.notice(NoticeLevel::Error, text);
        }
        self.finish_turn(if code == -32800 { "cancelled" } else { "error" })
            .await
    }

    async fn finish_turn(&mut self, reason: &str) -> Result<()> {
        self.cancel_permissions().await?;
        self.transcript.finish(&self.out);
        self.active = false;
        self.cancelling = false;
        emit(
            &self.out,
            EventKind::TurnDone {
                summary: (reason != "end_turn").then(|| reason.to_owned()),
            },
        );
        self.state(SessionState::Idle);
        Ok(())
    }

    fn update(&mut self, params: &Value) {
        if params["sessionId"].as_str() != self.session.as_deref() || self.session.is_none() {
            return;
        }
        let loading = self
            .pending
            .as_ref()
            .is_some_and(|p| p.kind == Request::Load);
        if !self.active && !loading {
            return;
        }
        let update = &params["update"];
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk" | "agent_thought_chunk") => {
                self.transcript.chunk(update, &self.out)
            }
            Some("user_message_chunk") if loading => self.transcript.boundary(),
            Some("tool_call" | "tool_call_update") => {
                self.tools.apply(update, &self.out);
            }
            Some("plan") => {
                if let Some(entries) = update["entries"].as_array() {
                    let lines = entries
                        .iter()
                        .filter_map(|e| {
                            Some(format!(
                                "- [{}] {}",
                                e["status"].as_str().unwrap_or("pending"),
                                e["content"].as_str()?
                            ))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.notice(NoticeLevel::Info, lines);
                }
            }
            _ => {}
        }
    }

    async fn agent_request(&mut self, method: &str, id: Value, params: &Value) -> Result<()> {
        if method != "session/request_permission" {
            return rpc::error(&mut self.writer, id, -32601, "Method not found").await;
        }
        if params["sessionId"].as_str() != self.session.as_deref() || self.session.is_none() {
            return rpc::error(&mut self.writer, id, -32002, "Unknown session").await;
        }
        if !self.active {
            return rpc::error(&mut self.writer, id, -32600, "No active prompt turn").await;
        }
        if self.cancelling {
            return rpc::permission(&mut self.writer, id, None).await;
        }
        if self.permissions.iter().any(|p| p.rpc_id == id) {
            return rpc::error(&mut self.writer, id, -32600, "Duplicate request id").await;
        }
        let Some(options) = permission_options(params) else {
            return rpc::error(&mut self.writer, id, -32602, "Invalid permission params").await;
        };
        let Some(tool) = self.tools.apply(&params["toolCall"], &self.out) else {
            return rpc::error(&mut self.writer, id, -32602, "Missing toolCallId").await;
        };
        let approval_id = Uuid::new_v4().to_string();
        let mut choices: Vec<_> = options.iter().map(|(decision, _)| *decision).collect();
        // Cancellation is a baseline ACP operation, not a PermissionOptionKind.
        choices.push(Decision::Abort);
        let kind = match tool["kind"].as_str() {
            Some("execute") => ApprovalKind::Command,
            Some("edit" | "delete" | "move") => ApprovalKind::FileChange,
            _ => ApprovalKind::Tool,
        };
        emit(
            &self.out,
            EventKind::Approval {
                approval: Approval {
                    id: approval_id.clone(),
                    kind,
                    title: tool["title"]
                        .as_str()
                        .unwrap_or("ACP 工具权限请求")
                        .to_owned(),
                    detail: serde_json::to_string_pretty(
                        &json!({"toolCall":tool, "options":params["options"]}),
                    )?,
                    cwd: Some(self.cwd.clone()),
                    choices,
                },
            },
        );
        self.permissions.push(Permission {
            id: approval_id,
            rpc_id: id,
            options,
        });
        self.state(SessionState::NeedsYou);
        Ok(())
    }

    async fn answer(&mut self, approval: String, decision: Decision) -> Result<()> {
        let Some(index) = self.permissions.iter().position(|p| p.id == approval) else {
            self.notice(NoticeLevel::Warn, "已忽略过期或未知的 ACP 审批答复");
            return Ok(());
        };
        if decision == Decision::Abort {
            return self.cancel().await;
        }
        let option = self.permissions[index]
            .options
            .iter()
            .find(|(d, _)| *d == decision)
            .map(|(_, id)| id.clone());
        let Some(option) = option else {
            self.notice(NoticeLevel::Warn, "该 ACP 权限请求未提供此决策");
            return Ok(());
        };
        let permission = self.permissions.remove(index);
        rpc::permission(&mut self.writer, permission.rpc_id, Some(&option)).await?;
        self.approval_done(permission.id, decision);
        if !self.permissions.is_empty() {
            self.state(SessionState::NeedsYou);
        }
        Ok(())
    }

    fn approval_done(&self, approval: String, decision: Decision) {
        emit(&self.out, EventKind::ApprovalDone { approval, decision });
        self.state(SessionState::Working);
    }

    async fn cancel_permissions(&mut self) -> Result<()> {
        for permission in std::mem::take(&mut self.permissions) {
            rpc::permission(&mut self.writer, permission.rpc_id, None).await?;
            self.approval_done(permission.id, Decision::Abort);
        }
        Ok(())
    }

    async fn cancel_request(&mut self, params: &Value) -> Result<()> {
        if let Some(index) = self
            .permissions
            .iter()
            .position(|p| p.rpc_id == params["requestId"])
        {
            let permission = self.permissions.remove(index);
            rpc::permission(&mut self.writer, permission.rpc_id, None).await?;
            self.approval_done(permission.id, Decision::Abort);
            if !self.permissions.is_empty() {
                self.state(SessionState::NeedsYou);
            }
        }
        Ok(())
    }

    async fn cancel(&mut self) -> Result<()> {
        self.cancel_permissions().await?;
        if self.active && !self.cancelling {
            self.cancelling = true;
            rpc::write(&mut self.writer, json!({"jsonrpc":"2.0", "method":"session/cancel", "params":{"sessionId":self.session}})).await?;
            self.deadline = Some(Instant::now() + CANCEL_TIMEOUT);
        } else if !self.active {
            for _ in self.queued.drain(..) {
                emit(
                    &self.out,
                    EventKind::TurnDone {
                        summary: Some("cancelled".into()),
                    },
                );
                emit(
                    &self.out,
                    EventKind::State {
                        state: SessionState::Idle,
                        status_line: None,
                    },
                );
            }
        }
        Ok(())
    }
}

fn permission_options(params: &Value) -> Option<Vec<(Decision, String)>> {
    params["toolCall"]["toolCallId"].as_str()?;
    let mut mapped = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for option in params["options"].as_array()? {
        let id = option["optionId"].as_str()?;
        option["name"].as_str()?;
        if !ids.insert(id) {
            return None;
        }
        let decision = match option["kind"].as_str()? {
            "allow_once" => Decision::AllowOnce,
            "allow_always" => Decision::AllowSession,
            "reject_once" => Decision::Deny,
            // reject_always has no faithful Tether decision; never pretend
            // that denying once remembers a decision on the user's behalf.
            _ => continue,
        };
        if !mapped.iter().any(|(d, _)| *d == decision) {
            mapped.push((decision, id.to_owned()));
        }
    }
    Some(mapped)
}

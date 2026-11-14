//! One Tether session = one DSH session on the shared `dsh web`.
//!
//! The DSH session lives in `dsh web`, not in this task: ending the task only
//! stops following it. `Exited` is sent when the connection to `dsh web`
//! drops; the server then resumes with the session id on the next message.

use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{Value, json};
use tether_agent_api::{AgentCommand, AgentOutput, SpawnOptions, with_paths};
use tether_proto::{Decision, EventKind, NoticeLevel, SessionState};
use tokio::sync::{broadcast, mpsc};

use crate::{
    api::{self, Api, rpc_code},
    hub::Frame,
    translate::{self, Translator, str_at},
};

/// Pictures DSH's prompt takes as image parts; anything else goes as a path.
const IMAGE_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
];
const MAX_IMAGE_BYTES: u64 = 20 << 20;

pub(crate) struct Session {
    api: Arc<dyn Api>,
    out: mpsc::UnboundedSender<AgentOutput>,
    id: String,
    tr: Translator,
    /// Pending approvals: approval id → the rpc id that answers it.
    approvals: HashMap<String, String>,
    /// A question DSH is waiting on: its rpc id and questions.
    question: Option<(String, Vec<Value>)>,
}

/// Run until shutdown or until `dsh web` goes away.
pub(crate) async fn run(
    api: Arc<dyn Api>,
    mut frames: broadcast::Receiver<Arc<Frame>>,
    opts: SpawnOptions,
    mut commands: mpsc::UnboundedReceiver<AgentCommand>,
    out: mpsc::UnboundedSender<AgentOutput>,
) {
    let mut session = match Session::open(api, &opts, out.clone()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = out.send(AgentOutput::Exited {
                code: None,
                error: Some(format!("{error:#}")),
            });
            return;
        }
    };
    let error = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(AgentCommand::Shutdown) => break None,
                Some(command) => session.command(command).await,
            },
            frame = frames.recv() => match frame {
                Ok(frame) => {
                    if let Some(reason) = session.frame(&frame) {
                        break Some(reason);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    session.notice(NoticeLevel::Warn, format!("DSH 事件太多，漏掉了 {n} 条；在电脑上的 DSH 里可以看到完整记录"));
                }
                Err(broadcast::error::RecvError::Closed) => break Some("与 DSH 的连接已关闭".into()),
            },
            _ = out.closed() => break None,
        }
    };
    session.api.detach(&session.id);
    if error.is_some() {
        for approval in std::mem::take(&mut session.approvals).into_keys() {
            session.emit(EventKind::ApprovalDone {
                approval,
                decision: Decision::Abort,
            });
        }
    }
    let _ = out.send(AgentOutput::Exited { code: None, error });
}

impl Session {
    async fn open(
        api: Arc<dyn Api>,
        opts: &SpawnOptions,
        out: mpsc::UnboundedSender<AgentOutput>,
    ) -> Result<Self> {
        let cwd = opts
            .cwd
            .to_str()
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        let id = match &opts.resume {
            Some(id) => {
                api.attach(id)
                    .await
                    .with_context(|| format!("打开 DSH 会话 {id} 失败"))?;
                id.clone()
            }
            None => {
                let payload = match &cwd {
                    Some(cwd) => json!({ "cwd": cwd }),
                    None => json!({}),
                };
                let created = api
                    .call("session.create", payload)
                    .await
                    .context("创建 DSH 会话失败")?;
                let id = created
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .context("DSH 没有返回会话 id")?
                    .to_owned();
                // 0.2 sends a session's frames only to those following it.
                api.attach(&id)
                    .await
                    .with_context(|| format!("打开 DSH 会话 {id} 失败"))?;
                id
            }
        };
        let _ = out.send(AgentOutput::NativeId(id.clone()));
        let mut session = Self {
            api,
            out,
            id,
            tr: Translator::live(cwd),
            approvals: HashMap::new(),
            question: None,
        };
        let permission = opts
            .permission
            .clone()
            .or_else(|| (opts.chat && opts.resume.is_none()).then(|| "read-only".to_owned()));
        session
            .configure(opts.model.clone(), opts.effort.clone(), permission)
            .await;
        Ok(session)
    }

    fn emit(&self, kind: EventKind) {
        let _ = self.out.send(AgentOutput::Event(kind));
    }

    fn emit_all(&self, kinds: Vec<EventKind>) {
        for kind in kinds {
            self.emit(kind);
        }
    }

    fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        self.emit(EventKind::Notice {
            level,
            text: text.into(),
        });
    }

    fn state(&mut self, state: SessionState, line: Option<String>) {
        let kinds = self.tr.state(state, line);
        self.emit_all(kinds);
    }

    /// Returns why the session ended, when it did.
    fn frame(&mut self, frame: &Frame) -> Option<String> {
        match frame {
            Frame::Down(reason) => Some(reason.clone()),
            Frame::Host(payload) => {
                if str_at(payload, &["sessionId"]) == Some(self.id.as_str())
                    && str_at(payload, &["type"]) == Some("host/agent-error")
                {
                    self.notice(
                        NoticeLevel::Error,
                        str_at(payload, &["message"])
                            .unwrap_or("DSH 出错")
                            .to_owned(),
                    );
                }
                if str_at(payload, &["sessionId"]) == Some(self.id.as_str())
                    && str_at(payload, &["type"]) == Some("host/session-removed")
                {
                    return Some("这个 DSH 会话已被删除".into());
                }
                None
            }
            Frame::Mux { rpc_id, payload } => {
                if str_at(payload, &["sessionId"]) != Some(self.id.as_str()) {
                    return None;
                }
                match str_at(payload, &["type"]) {
                    Some("approval/requested") => {
                        if let Some(id) = str_at(payload, &["approvalId"]) {
                            // A reconnect replays the same request: show it once.
                            if self
                                .approvals
                                .insert(id.to_owned(), rpc_id.clone())
                                .is_some()
                            {
                                return None;
                            }
                        }
                    }
                    Some("approval/resolved") => {
                        // Already retired here (an answer DSH did not take).
                        let id = str_at(payload, &["approvalId"]).unwrap_or_default();
                        self.approvals.remove(id)?;
                    }
                    Some("question/requested") => {
                        let questions = payload
                            .get("questions")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        if self.question.as_ref().is_some_and(|(r, _)| r == rpc_id) {
                            return None;
                        }
                        self.question = Some((rpc_id.clone(), questions));
                    }
                    Some("question/resolved") => self.question = None,
                    Some("session/event")
                        if str_at(payload, &["event", "type"]) == Some("turn/end") =>
                    {
                        // Anything still asked belonged to the turn that ended.
                        for approval in std::mem::take(&mut self.approvals).into_keys() {
                            self.emit(EventKind::ApprovalDone {
                                approval,
                                decision: Decision::Abort,
                            });
                        }
                        self.question = None;
                    }
                    _ => {}
                }
                let kinds = self.tr.frame(payload);
                self.emit_all(kinds);
                None
            }
        }
    }

    async fn command(&mut self, command: AgentCommand) {
        match command {
            AgentCommand::Send { text, images } => self.send(text, images).await,
            AgentCommand::Answer { approval, decision } => self.answer(approval, decision).await,
            AgentCommand::Interrupt => self.interrupt().await,
            AgentCommand::SetModel {
                model,
                effort,
                speed: _,
                permission,
            } => self.configure(model, effort, permission).await,
            AgentCommand::Shutdown => {}
        }
    }

    async fn send(&mut self, text: String, images: Vec<String>) {
        if let Some((rpc_id, questions)) = self.question.take() {
            let answers = translate::answers(&questions, &text);
            let message = api::question_answer(&rpc_id, &self.id, answers);
            match self.api.respond(message).await {
                Ok(true) => self.state(SessionState::Working, None),
                Ok(false) => {
                    self.notice(
                        NoticeLevel::Warn,
                        "DSH 已不再等这个问题的回答，作为新消息发送",
                    );
                    self.prompt(text, images).await;
                }
                Err(error) => {
                    self.question = Some((rpc_id, questions));
                    self.notice(NoticeLevel::Error, format!("回答问题失败：{error:#}"));
                }
            }
            return;
        }
        if images.is_empty() && text.trim_start().starts_with('/') && self.command_line(&text).await
        {
            return;
        }
        self.prompt(text, images).await;
    }

    /// A DSH slash command (`/compact`, `/permission read-only`…). False when
    /// DSH does not know it: it goes to the model as a message then.
    async fn command_line(&mut self, line: &str) -> bool {
        let payload = json!({ "args": { "agentId": self.id, "line": line.trim(), "images": [] } });
        match self.api.call("commands/execute", payload).await {
            Ok(Value::Null) => false,
            Ok(done) => {
                let text = str_at(&done, &["result", "text"])
                    .unwrap_or("完成")
                    .to_owned();
                let level = if str_at(&done, &["result", "kind"]) == Some("error") {
                    NoticeLevel::Error
                } else {
                    NoticeLevel::Info
                };
                self.notice(level, text);
                if self.tr.current_state() != Some(SessionState::Working) {
                    self.state(SessionState::Idle, None);
                }
                true
            }
            Err(error) => {
                self.notice(NoticeLevel::Error, format!("DSH 命令失败：{error:#}"));
                true
            }
        }
    }

    async fn prompt(&mut self, text: String, images: Vec<String>) {
        self.state(SessionState::Working, None);
        let mut content = Vec::new();
        let mut as_paths = Vec::new();
        for path in images {
            match image_part(Path::new(&path)) {
                Some(part) => content.push(part),
                None => as_paths.push(path),
            }
        }
        content.insert(
            0,
            json!({ "type": "text", "text": with_paths(&text, &as_paths) }),
        );
        let payload = json!({ "sessionId": self.id, "mode": "queue", "content": content });
        if let Err(error) = self.api.call("session.prompt", payload).await {
            let text = match rpc_code(&error) {
                Some("session-not-found") => "DSH 里找不到这个会话了".to_owned(),
                _ => format!("发送给 DSH 失败：{error:#}"),
            };
            self.notice(NoticeLevel::Error, text);
            self.state(SessionState::Idle, None);
        }
    }

    async fn answer(&mut self, approval: String, decision: Decision) {
        let Some(rpc_id) = self.approvals.get(&approval).cloned() else {
            self.notice(NoticeLevel::Warn, "这个审批已经结束了");
            return;
        };
        let outcome = match decision {
            Decision::AllowOnce | Decision::AllowSession => "allowed-once",
            Decision::Deny | Decision::Abort => "rejected",
        };
        let message = api::approval_answer(&rpc_id, &self.id, &approval, outcome);
        match self.api.respond(message).await {
            // `approval/resolved` follows and reports it.
            Ok(true) => {}
            Ok(false) => {
                self.approvals.remove(&approval);
                self.emit(EventKind::ApprovalDone {
                    approval,
                    decision: Decision::Abort,
                });
                self.notice(NoticeLevel::Warn, "DSH 已不再等这个审批");
            }
            Err(error) => {
                self.notice(NoticeLevel::Error, format!("回答审批失败：{error:#}"));
                return;
            }
        }
        if decision == Decision::Abort {
            self.interrupt().await;
        }
    }

    async fn interrupt(&mut self) {
        if let Some((rpc_id, _)) = self.question.take() {
            let _ = self.api.respond(api::question_cancel(&rpc_id)).await;
        }
        if let Err(error) = self
            .api
            .call("session.cancel", json!({ "sessionId": self.id }))
            .await
        {
            self.notice(NoticeLevel::Error, format!("停止失败：{error:#}"));
        }
    }

    /// Model ids are `provider/model` (see `list_models`).
    async fn configure(
        &mut self,
        model: Option<String>,
        effort: Option<String>,
        permission: Option<String>,
    ) {
        if model.is_some() || effort.is_some() {
            let current = match &model {
                Some(_) => None,
                None => self
                    .api
                    .call("session.models", json!({ "sessionId": self.id }))
                    .await
                    .ok()
                    .and_then(|v| v.get("current").cloned()),
            };
            let pick = model
                .as_deref()
                .and_then(|m| m.split_once('/'))
                .map(|(p, m)| (p.to_owned(), m.to_owned()))
                .or_else(|| {
                    let c = current.as_ref()?;
                    Some((
                        str_at(c, &["provider"])?.to_owned(),
                        str_at(c, &["model"])?.to_owned(),
                    ))
                });
            match pick {
                Some((provider, model)) => {
                    let mut payload =
                        json!({ "sessionId": self.id, "provider": provider, "model": model });
                    if let Some(effort) = effort.filter(|e| !e.is_empty()) {
                        payload["reasoningEffort"] = json!(effort);
                    }
                    if let Err(error) = self.api.call("session.selectModel", payload).await {
                        self.notice(NoticeLevel::Warn, format!("切换模型失败：{error:#}"));
                    }
                }
                None => self.notice(
                    NoticeLevel::Warn,
                    "DSH 的模型要写成 提供方/模型，例如 deepseek-official/deepseek-v4-flash",
                ),
            }
        }
        if let Some(preset) = permission.filter(|p| !p.is_empty()) {
            let line = format!("/permission {preset}");
            let payload = json!({ "args": { "agentId": self.id, "line": line, "images": [] } });
            match self.api.call("commands/execute", payload).await {
                Ok(done) if str_at(&done, &["result", "kind"]) == Some("success") => {}
                Ok(done) => self.notice(
                    NoticeLevel::Warn,
                    format!(
                        "切换权限失败：{}",
                        str_at(&done, &["result", "text"]).unwrap_or("DSH 不认识 /permission")
                    ),
                ),
                Err(error) => self.notice(NoticeLevel::Warn, format!("切换权限失败：{error:#}")),
            }
        }
    }
}

fn image_part(path: &Path) -> Option<Value> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let media = IMAGE_TYPES.iter().find(|(e, _)| *e == ext)?.1;
    if std::fs::metadata(path).ok()?.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let mut part = json!({
        "type": "image",
        "mediaType": media,
        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
    });
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        part["name"] = json!(name);
    }
    Some(part)
}

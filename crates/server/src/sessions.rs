//! A single actor serializes commands, adapter output, seq assignment and writes.
use crate::{Fault, Result, backends::StoreBackend, lock, notify};
use anyhow::{Context, bail, ensure};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
};
use tether_agent_api::{AgentAdapter, AgentCommand, AgentOutput, SpawnOptions};
use tether_proto::{
    ErrorCode, Event, EventKind, MAX_FRAME, MAX_TOOL_OUTPUT, NoticeLevel, ServerMsg, SessionInfo,
    SessionState, now_ms, truncate_tail,
};
use tether_store::SessionRecord;
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::Instant,
};

pub(crate) const HISTORY_PAGE: usize = 128;
type HandleMap = Mutex<HashMap<String, Handle>>;

pub(crate) struct Sessions {
    store: Arc<dyn StoreBackend>,
    agents: HashMap<String, Arc<dyn AgentAdapter>>,
    bus: broadcast::Sender<ServerMsg>,
    handles: Arc<HandleMap>,
}

struct Shared {
    record: Mutex<SessionRecord>,
    events: broadcast::Sender<Event>,
    subscribers: AtomicUsize,
}

#[derive(Clone)]
pub(crate) struct Handle {
    shared: Arc<Shared>,
    commands: mpsc::Sender<Action>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

/// What `create_session` asks for; `resume` is the agent's own
/// conversation to continue (`list_history`).
#[derive(Debug, Default)]
pub(crate) struct NewSession {
    pub cwd: PathBuf,
    pub title: Option<String>,
    pub resume: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub chat: bool,
    pub speed: Option<String>,
    pub permission: Option<String>,
    pub launch: Launch,
}

/// How a new conversation starts; not kept across restarts, when it only
/// ever resumes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Launch {
    pub project: Option<String>,
    pub roots: Vec<PathBuf>,
    pub reveal: bool,
    /// Start as a copy of this conversation of the agent's own.
    pub fork: Option<String>,
}

pub(crate) struct Subscription {
    shared: Arc<Shared>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.shared.subscribers.fetch_sub(1, Ordering::SeqCst);
    }
}

enum Action {
    Command(AgentCommand, oneshot::Sender<Result<()>>),
    Close(oneshot::Sender<Result<()>>),
    Shutdown(oneshot::Sender<Result<()>>),
    /// Stop the idle agent so another program may open its conversation.
    Release(oneshot::Sender<Result<()>>),
    #[cfg(test)]
    Inject(Vec<EventKind>, oneshot::Sender<Result<()>>),
}

impl Sessions {
    pub(crate) fn load(
        store: Arc<dyn StoreBackend>,
        agents: HashMap<String, Arc<dyn AgentAdapter>>,
        bus: broadcast::Sender<ServerMsg>,
    ) -> anyhow::Result<Self> {
        let mut records = store.list_sessions()?;
        for record in &mut records {
            // append_event and upsert_session are separate frozen store calls.
            // Recover a durable tail even if the host died between those calls.
            loop {
                let tail =
                    store.events_after(&record.info.id, record.info.last_seq, HISTORY_PAGE)?;
                if tail.is_empty() {
                    break;
                }
                let last = tail.last().context("empty event tail")?.seq;
                ensure!(
                    last > record.info.last_seq,
                    "non-increasing store event tail"
                );
                record.info.last_seq = last;
            }
            let interrupted = record.info.state == SessionState::Working;
            record.info.state = SessionState::Idle;
            record.info.status_line = None;
            record.info.updated_at = now_ms();
            if interrupted {
                record.info.last_seq = record
                    .info
                    .last_seq
                    .checked_add(1)
                    .context("event seq exhausted")?;
                store.append_event(
                    &record.info.id,
                    &Event {
                        seq: record.info.last_seq,
                        at: now_ms(),
                        kind: EventKind::Notice {
                            level: NoticeLevel::Warn,
                            text: "电脑重启，任务已中断".into(),
                        },
                    },
                )?;
            }
            store.upsert_session(record)?;
        }
        let this = Self {
            store,
            agents,
            bus,
            handles: Arc::new(Mutex::new(HashMap::new())),
        };
        for record in records {
            this.install(record, Launch::default());
        }
        Ok(this)
    }

    fn install(&self, record: SessionRecord, launch: Launch) -> Handle {
        let (events, _) = broadcast::channel(64);
        let (commands, rx) = mpsc::channel(32);
        let shared = Arc::new(Shared {
            record: Mutex::new(record.clone()),
            events,
            subscribers: AtomicUsize::new(0),
        });
        let actor = Actor {
            adapter: self.agents.get(&record.info.agent).cloned(),
            record,
            launch,
            shared: shared.clone(),
            store: self.store.clone(),
            bus: self.bus.clone(),
            process: None,
            output: None,
            busy: false,
            release_at: None,
            approvals: HashMap::new(),
            registry: Arc::downgrade(&self.handles),
        };
        let handle = Handle {
            shared,
            commands,
            task: Arc::new(Mutex::new(Some(tokio::spawn(actor.run(rx))))),
        };
        lock(&self.handles).insert(handle.info().id, handle.clone());
        handle
    }

    pub(crate) fn list(&self) -> Vec<SessionInfo> {
        let mut sessions: Vec<_> = lock(&self.handles).values().map(Handle::info).collect();
        sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        sessions
    }

    pub(crate) fn get(&self, id: &str) -> Result<Handle> {
        lock(&self.handles)
            .get(id)
            .cloned()
            .ok_or_else(|| Fault::new(ErrorCode::NotFound, "session not found"))
    }

    /// The session that continues `agent`'s own conversation `native`, if any.
    pub(crate) fn resuming(&self, agent: &str, native: &str) -> Option<Handle> {
        lock(&self.handles)
            .values()
            .find(|h| {
                let record = lock(&h.shared.record);
                record.info.agent == agent && record.native_id.as_deref() == Some(native)
            })
            .cloned()
    }

    pub(crate) fn create(&self, agent: &str, new: NewSession) -> Result<SessionInfo> {
        let NewSession {
            mut cwd,
            title,
            resume,
            model,
            effort,
            chat,
            speed,
            permission,
            launch,
        } = new;
        let adapter = self
            .agents
            .get(agent)
            .ok_or_else(|| Fault::new(ErrorCode::AgentUnavailable, "unknown agent"))?;
        let info = adapter.info();
        if !info.available || !info.chat {
            return Err(Fault::new(
                ErrorCode::AgentUnavailable,
                "agent does not support chat",
            ));
        }
        let title = title.unwrap_or(info.name);
        // A continued (or copied) chat keeps the folder it has; a new one gets its own.
        if chat && resume.is_none() && launch.fork.is_none() {
            match adapter.chat_dir(&title) {
                Ok(Some(dir)) => cwd = dir,
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(error = format!("{error:#}"), "chat folder");
                    return Err(Fault::new(
                        ErrorCode::Internal,
                        format!("无法创建对话文件夹：{error:#}"),
                    ));
                }
            }
        }
        let now = now_ms();
        let record = SessionRecord {
            info: SessionInfo {
                id: uuid::Uuid::new_v4().to_string(),
                agent: agent.into(),
                title,
                cwd: cwd.to_string_lossy().into_owned(),
                state: SessionState::Idle,
                status_line: None,
                created_at: now,
                updated_at: now,
                last_seq: 0,
                model,
                effort,
                chat,
                speed,
                thread: resume.clone(),
                permission,
            },
            native_id: resume,
        };
        let response = ServerMsg::ok(
            u64::MAX,
            &tether_proto::SessionData {
                session: record.info.clone(),
            },
        );
        if serde_json::to_vec(&response)
            .map_err(anyhow::Error::from)?
            .len()
            > MAX_FRAME
        {
            return Err(Fault::new(
                ErrorCode::BadRequest,
                "session metadata exceeds maximum frame size",
            ));
        }
        self.store.upsert_session(&record)?;
        let info = self.install(record, launch).info();
        let _ = self.bus.send(ServerMsg::SessionUpdate {
            session: info.clone(),
        });
        Ok(info)
    }

    pub(crate) async fn close(&self, id: &str) -> Result<()> {
        let handle = self.get(id)?;
        handle.request(Action::Close).await?;
        handle.join().await;
        Ok(())
    }

    pub(crate) async fn shutdown(&self) {
        let handles = std::mem::take(&mut *lock(&self.handles));
        for handle in handles.values() {
            let _ = handle.request(Action::Shutdown).await;
            handle.join().await;
        }
    }
}

impl Handle {
    pub(crate) fn info(&self) -> SessionInfo {
        lock(&self.shared.record).info.clone()
    }

    /// Registration happens BEFORE the first store read, including its count
    /// for turn_done notifications. The guard is owned by the connection.
    pub(crate) fn subscribe(&self) -> (Subscription, broadcast::Receiver<Event>) {
        let receiver = self.shared.events.subscribe();
        self.shared.subscribers.fetch_add(1, Ordering::SeqCst);
        (
            Subscription {
                shared: self.shared.clone(),
            },
            receiver,
        )
    }

    async fn request(
        &self,
        action: impl FnOnce(oneshot::Sender<Result<()>>) -> Action,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(action(tx))
            .await
            .map_err(|_| Fault::new(ErrorCode::NotFound, "session closed"))?;
        rx.await
            .map_err(|_| Fault::new(ErrorCode::Internal, "session actor stopped"))?
    }
    pub(crate) async fn command(&self, command: AgentCommand) -> Result<()> {
        self.request(|reply| Action::Command(command, reply)).await
    }
    pub(crate) async fn release(&self) -> Result<()> {
        self.request(Action::Release).await
    }
    async fn join(&self) {
        let task = lock(&self.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
    #[cfg(test)]
    pub(crate) async fn inject(&self, events: Vec<EventKind>) {
        self.request(|reply| Action::Inject(events, reply))
            .await
            .unwrap();
    }
}

struct Actor {
    record: SessionRecord,
    launch: Launch,
    shared: Arc<Shared>,
    store: Arc<dyn StoreBackend>,
    bus: broadcast::Sender<ServerMsg>,
    adapter: Option<Arc<dyn AgentAdapter>>,
    process: Option<mpsc::UnboundedSender<AgentCommand>>,
    output: Option<mpsc::UnboundedReceiver<AgentOutput>>,
    busy: bool,
    /// When an idle agent lets go of its conversation (`release_after`).
    release_at: Option<Instant>,
    approvals: HashMap<String, Vec<tether_proto::Decision>>,
    registry: Weak<HandleMap>,
}

async fn next_output(
    output: &mut Option<mpsc::UnboundedReceiver<AgentOutput>>,
) -> Option<AgentOutput> {
    match output {
        Some(output) => output.recv().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

impl Actor {
    async fn run(mut self, mut rx: mpsc::Receiver<Action>) {
        loop {
            tokio::select! {
                // Drain already-buffered exits/states before accepting the next send.
                biased;
                output = next_output(&mut self.output) => {
                    let result = match output {
                        Some(output) => self.output(output),
                        None => self.exited(None, Some("agent output channel closed".into())),
                    };
                    if let Err(error) = result {
                        tracing::error!(%error, session = %self.record.info.id, "session output failed");
                        self.stop_process();
                        if self.emit(EventKind::State { state: SessionState::Error, status_line: None }).is_err() {
                            self.record.info.state = SessionState::Error;
                            self.record.info.status_line = None;
                            *lock(&self.shared.record) = self.record.clone();
                            let _ = self.bus.send(ServerMsg::SessionUpdate { session: self.record.info.clone() });
                            let _ = self.bus.send(ServerMsg::Notify {
                                kind: tether_proto::NotifyKind::Failed, session: Some(self.record.info.id.clone()), term: None,
                                title: self.record.info.title.clone(), body: "会话事件保存失败".into(),
                            });
                        }
                    }
                }
                action = rx.recv() => match action {
                    Some(Action::Command(command, reply)) => { let _ = reply.send(self.command(command)); }
                    Some(Action::Close(reply)) => {
                        // Do not delete the actor if persistence fails.
                        let result = self.store.delete_session(&self.record.info.id).map_err(Fault::from);
                        let closed = result.is_ok();
                        if closed {
                            self.stop_process();
                            // The requesting socket may be revoked/cancelled while
                            // awaiting us. The actor, not that future, owns removal.
                            if let Some(registry) = self.registry.upgrade() {
                                lock(&registry).remove(&self.record.info.id);
                            }
                            let _ = self.bus.send(ServerMsg::SessionClosed { session: self.record.info.id.clone() });
                        }
                        let _ = reply.send(result);
                        if closed { break; }
                    }
                    Some(Action::Shutdown(reply)) => { self.stop_process(); let _ = reply.send(Ok(())); break; }
                    Some(Action::Release(reply)) => {
                        let result = if self.busy {
                            Err(Fault::new(ErrorCode::Busy, "手机上的这个对话还在进行，等它完成再交给电脑"))
                        } else {
                            self.stop_process();
                            Ok(())
                        };
                        let _ = reply.send(result);
                    }
                    #[cfg(test)]
                    Some(Action::Inject(events, reply)) => {
                        let result = events.into_iter().try_for_each(|event| self.emit(event));
                        let _ = reply.send(result.map_err(Fault::from));
                    }
                    None => { self.stop_process(); break; }
                },
                () = sleep_until(self.release_at) => {
                    self.release_at = None;
                    if !self.busy && self.process.is_some() {
                        tracing::debug!(session = %self.record.info.id, "idle agent released");
                        self.stop_process();
                    }
                }
            }
        }
    }

    fn stop_process(&mut self) {
        if let Some(process) = self.process.take() {
            let _ = process.send(AgentCommand::Shutdown);
        }
        self.output = None;
        self.busy = false;
        self.release_at = None;
        self.approvals.clear();
    }

    fn command(&mut self, command: AgentCommand) -> Result<()> {
        match &command {
            AgentCommand::SetModel {
                model,
                effort,
                speed,
                permission,
            } => {
                if permission.is_some() {
                    self.record.info.permission.clone_from(permission);
                }
                if model.is_some() {
                    self.record.info.model.clone_from(model);
                }
                if effort.is_some() {
                    self.record.info.effort.clone_from(effort);
                }
                if speed.is_some() {
                    self.record.info.speed.clone_from(speed);
                }
                self.record.info.updated_at = now_ms();
                self.store.upsert_session(&self.record)?;
                *lock(&self.shared.record) = self.record.clone();
                let _ = self.bus.send(ServerMsg::SessionUpdate {
                    session: self.record.info.clone(),
                });
                // A later spawn reads the record; a running agent hears now.
                if self
                    .process
                    .as_ref()
                    .is_none_or(mpsc::UnboundedSender::is_closed)
                {
                    return Ok(());
                }
            }
            AgentCommand::Send { text, images } => {
                if self.busy {
                    return Err(Fault::new(
                        ErrorCode::Busy,
                        "session is working or waiting for approval",
                    ));
                }
                let envelope = ServerMsg::Event {
                    session: self.record.info.id.clone(),
                    event: Event {
                        seq: u64::MAX,
                        at: u64::MAX,
                        kind: EventKind::UserText {
                            text: text.clone(),
                            images: pictures(images),
                        },
                    },
                };
                if serde_json::to_vec(&envelope)
                    .map_err(anyhow::Error::from)?
                    .len()
                    > MAX_FRAME
                {
                    return Err(Fault::new(
                        ErrorCode::BadRequest,
                        "message does not fit an event frame",
                    ));
                }
                if self
                    .process
                    .as_ref()
                    .is_none_or(mpsc::UnboundedSender::is_closed)
                {
                    let adapter = self
                        .adapter
                        .as_ref()
                        .filter(|a| a.info().available)
                        .ok_or_else(|| {
                            Fault::new(ErrorCode::AgentUnavailable, "agent unavailable")
                        })?;
                    let (out, output) = mpsc::unbounded_channel();
                    let process = adapter
                        .spawn(
                            SpawnOptions {
                                cwd: PathBuf::from(&self.record.info.cwd),
                                // A copy not made yet starts from its original.
                                resume: self
                                    .record
                                    .native_id
                                    .clone()
                                    .or_else(|| self.launch.fork.clone()),
                                fork: self.record.native_id.is_none() && self.launch.fork.is_some(),
                                model: self.record.info.model.clone(),
                                effort: self.record.info.effort.clone(),
                                chat: self.record.info.chat,
                                speed: self.record.info.speed.clone(),
                                permission: self.record.info.permission.clone(),
                                project: self.launch.project.clone(),
                                roots: self.launch.roots.clone(),
                                reveal: self.launch.reveal,
                                ..Default::default()
                            },
                            out,
                        )
                        .map_err(|error| {
                            tracing::warn!(%error, "agent spawn failed");
                            Fault::new(ErrorCode::AgentUnavailable, "agent failed to start")
                        })?;
                    self.process = Some(process);
                    self.output = Some(output);
                }
                self.emit(EventKind::UserText {
                    text: text.clone(),
                    images: pictures(images),
                })?;
                self.busy = true;
                self.release_at = None;
            }
            AgentCommand::Answer { approval, decision } => {
                let choices = self
                    .approvals
                    .get(approval)
                    .ok_or_else(|| Fault::new(ErrorCode::NotFound, "approval not pending"))?;
                if !choices.contains(decision) {
                    return Err(Fault::new(ErrorCode::BadRequest, "decision not offered"));
                }
            }
            AgentCommand::Interrupt if self.process.is_none() => return Ok(()),
            _ => {}
        }
        let sent = self
            .process
            .as_ref()
            .is_some_and(|process| process.send(command.clone()).is_ok());
        if !sent {
            self.exited(None, Some("agent command channel closed".into()))?;
            return Err(Fault::new(
                ErrorCode::AgentUnavailable,
                "agent exited before accepting command",
            ));
        }
        if let AgentCommand::Answer { approval, .. } = command {
            self.approvals.remove(&approval);
        }
        Ok(())
    }

    fn output(&mut self, output: AgentOutput) -> anyhow::Result<()> {
        match output {
            AgentOutput::NativeId(id) => {
                // Started: a later spawn resumes it, and shows nothing again.
                self.launch.reveal = false;
                self.launch.fork = None;
                self.record.info.thread = Some(id.clone());
                self.record.native_id = Some(id);
                self.store.upsert_session(&self.record)?;
                *lock(&self.shared.record) = self.record.clone();
                Ok(())
            }
            AgentOutput::Event(kind) => self.emit(kind),
            AgentOutput::Exited { code, error } => self.exited(code, error),
        }
    }

    fn exited(&mut self, code: Option<i32>, error: Option<String>) -> anyhow::Result<()> {
        self.stop_process();
        let failed = error.is_some() || code.is_some_and(|code| code != 0);
        if let Some(text) = error {
            self.emit(EventKind::Notice {
                level: NoticeLevel::Error,
                text,
            })?;
        }
        self.emit(EventKind::State {
            state: if failed {
                SessionState::Error
            } else {
                SessionState::Exited
            },
            status_line: None,
        })
    }

    fn emit(&mut self, mut kind: EventKind) -> anyhow::Result<()> {
        if let EventKind::ToolEnd { output, .. } = &mut kind {
            // proto::truncate_tail adds an ellipsis, so reserve its UTF-8 bytes.
            if output.len() > MAX_TOOL_OUTPUT {
                *output = truncate_tail(output, MAX_TOOL_OUTPUT - '…'.len_utf8());
            }
        }
        let mut record = self.record.clone();
        let previous = record.info.state;
        match &kind {
            EventKind::State { state, status_line } => {
                record.info.state = *state;
                if status_line.is_some()
                    || matches!(
                        state,
                        SessionState::Idle | SessionState::Error | SessionState::Exited
                    )
                {
                    record.info.status_line = status_line.clone();
                }
            }
            EventKind::ToolStart { summary, .. } => record.info.status_line = Some(summary.clone()),
            EventKind::Approval { approval } => {
                record.info.status_line = Some(approval.title.clone())
            }
            _ => {}
        }
        record.info.last_seq = record
            .info
            .last_seq
            .checked_add(1)
            .context("event seq exhausted")?;
        record.info.updated_at = now_ms();
        let event = Event {
            seq: record.info.last_seq,
            at: record.info.updated_at,
            kind,
        };
        let push = ServerMsg::Event {
            session: record.info.id.clone(),
            event: event.clone(),
        };
        if serde_json::to_vec(&push)?.len() > MAX_FRAME {
            bail!("agent event exceeds maximum frame size");
        }
        if !event.kind.is_transient() {
            self.store.append_event(&record.info.id, &event)?;
        }
        // If metadata fails after append, keep the seq reserved in memory too.
        self.record = record;
        *lock(&self.shared.record) = self.record.clone();
        self.store.upsert_session(&self.record)?;
        match &event.kind {
            EventKind::Approval { approval } => {
                self.approvals
                    .insert(approval.id.clone(), approval.choices.clone());
            }
            EventKind::ApprovalDone { approval, .. } => {
                self.approvals.remove(approval);
            }
            EventKind::State { state, .. } => {
                self.busy = matches!(state, SessionState::Working | SessionState::NeedsYou);
                if !self.busy {
                    self.approvals.clear();
                }
                self.release_at = match (self.busy, &self.process, &self.adapter) {
                    (false, Some(_), Some(adapter)) => {
                        adapter.release_after().map(|after| Instant::now() + after)
                    }
                    _ => None,
                };
            }
            _ => {}
        }
        let _ = self.shared.events.send(event.clone());
        let _ = self.bus.send(ServerMsg::SessionUpdate {
            session: self.record.info.clone(),
        });
        if let Some(notification) = notify::session(
            &self.record.info,
            previous,
            &event.kind,
            self.shared.subscribers.load(Ordering::SeqCst),
        ) {
            let _ = self.bus.send(notification);
        }
        Ok(())
    }
}

/// `user_text.images`: none is left out.
fn pictures(images: &[String]) -> Option<Vec<String>> {
    (!images.is_empty()).then(|| images.to_vec())
}

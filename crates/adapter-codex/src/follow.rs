//! Follow an existing owner on the desktop IPC bus, without opening a window
//! or taking its writer lock. The transport is injectable (including duplex).

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tether_agent_api::{AgentCommand, AgentOutput, SpawnOptions};
use tether_proto::{Decision, EventKind, NoticeLevel, SessionState};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc::{Receiver, UnboundedReceiver, UnboundedSender},
    time::{Instant, timeout, timeout_at},
};

use crate::{
    bus::{self, version},
    mirror::{self, Applied, Mirror},
    translate,
};

const TIMEOUT: Duration = Duration::from_secs(15);
const TIMEOUT_MS: u64 = 15_000;
/// A running app answers its bus at once; longer means it is not there.
const PROBE: Duration = Duration::from_secs(3);

pub(crate) struct Link<W> {
    frames: Receiver<anyhow::Result<Value>>,
    writer: W,
    client: String,
    owner: String,
    held: VecDeque<Value>,
}

impl<W: AsyncWrite + Unpin> Link<W> {
    async fn send(&mut self, frame: &Value) -> anyhow::Result<()> {
        timeout(TIMEOUT, bus::write(&mut self.writer, frame))
            .await
            .context("Codex desktop IPC write timed out")?
            .context(bus::Closed)
    }

    async fn request(
        &mut self,
        method: &str,
        version: u32,
        params: Value,
    ) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.send(&bus::request(
            &id,
            &self.client,
            method,
            version,
            params,
            TIMEOUT_MS,
        ))
        .await?;
        Ok(id)
    }

    /// Only used before following. Never discard broadcasts while awaiting a reply.
    async fn call(&mut self, method: &str, version: u32, params: Value) -> anyhow::Result<Value> {
        let id = self.request(method, version, params).await?;
        let deadline = Instant::now() + PROBE;
        loop {
            let frame = timeout_at(deadline, self.frames.recv())
                .await
                .with_context(|| format!("Codex desktop {method} timed out"))?
                .ok_or(bus::Closed)??;
            if frame["type"] == "client-discovery-request" {
                self.send(&bus::decline(&frame)).await?;
            } else if frame["type"] == "response" && frame["requestId"] == id {
                return Ok(frame);
            } else {
                self.held.push_back(frame);
            }
        }
    }

    async fn next(&mut self) -> anyhow::Result<Value> {
        if let Some(frame) = self.held.pop_front() {
            return Ok(frame);
        }
        self.frames
            .recv()
            .await
            .ok_or(bus::Closed)?
            .context(bus::Closed)
    }

    async fn following(&mut self, thread: &str, following: bool) -> anyhow::Result<()> {
        self.send(&bus::broadcast(
            &self.client,
            "thread-stream-following-changed",
            version::FOLLOWING_CHANGED,
            json!({"conversationId":thread,"hostId":"local","following":following}),
        ))
        .await
    }

    fn disconnected(&self, frame: &Value) -> bool {
        let status = if frame["type"] == "client-status-changed" {
            frame
        } else if frame["type"] == "broadcast" && frame["method"] == "client-status-changed" {
            &frame["params"]
        } else {
            return false;
        };
        status["clientId"] == self.owner && status["status"] == "disconnected"
    }
}

/// Connected and the app owns `thread`: ready to follow. None: no owner.
pub(crate) async fn owned<R, W>(
    reader: R,
    writer: W,
    thread: &str,
) -> anyhow::Result<Option<Link<W>>>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut link = initialized(reader, writer).await?;
    // This discovery method has been verified against the Windows desktop app.
    let reply = link
        .call(
            "thread-follower-read-model-settings",
            version::READ_MODEL_SETTINGS,
            json!({"conversationId":thread}),
        )
        .await?;
    if reply["resultType"] == "error" && reply["error"] == "no-client-found" {
        return Ok(None);
    }
    refused(&reply, "owner query")?;
    link.owner = reply["handledByClientId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Codex desktop owner response returned no handledByClientId")?
        .to_owned();
    Ok(Some(link))
}

/// Puts `thread` in the app's sidebar without opening it. The app lists threads
/// other programs start only when it restarts; a window that unarchives one
/// tells the others "thread-unarchived", and each then reads that thread and
/// adds it to its recent conversations.
pub(crate) async fn announce<R, W>(reader: R, writer: W, thread: &str) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut link = initialized(reader, writer).await?;
    let frame = bus::broadcast(
        &link.client,
        "thread-unarchived",
        version::UNARCHIVED,
        json!({"hostId":"local","conversationId":thread}),
    );
    link.send(&frame).await
}

async fn initialized<R, W>(reader: R, writer: W) -> anyhow::Result<Link<W>>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut link = Link {
        frames: bus::frames(reader),
        writer,
        client: bus::UNINITIALIZED.into(),
        owner: String::new(),
        held: VecDeque::new(),
    };
    let reply = link
        .call(
            "initialize",
            version::INITIALIZE,
            json!({"clientType":"tether"}),
        )
        .await?;
    refused(&reply, "initialize")?;
    link.client = reply["result"]["clientId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Codex desktop initialize returned no clientId")?
        .to_owned();
    Ok(link)
}

/// An error answer where this Tether expects success: the app has changed.
fn refused(reply: &Value, what: &str) -> anyhow::Result<()> {
    if reply["resultType"] == "success" {
        return Ok(());
    }
    tracing::warn!(what, error = %reply["error"], "Codex desktop refused");
    Err(bus::Updated.into())
}

enum Pending {
    Start(String),
    Interrupt,
    Answer,
}

struct Session {
    mirror: Mirror,
    queue: VecDeque<(String, Vec<String>)>,
    pending: HashMap<String, Pending>,
    starting: Option<String>,
    /// A successful reply alone is not permission to start another turn: wait
    /// until its id is also in the mirrored state (possibly already completed).
    started_turn: Option<String>,
    interrupt_pending: bool,
    ready: bool,
    synced: bool,
    model: Option<String>,
    effort: Option<String>,
    speed: Option<String>,
    permission: Option<String>,
}

fn emit(out: &UnboundedSender<AgentOutput>, event: EventKind) {
    let _ = out.send(AgentOutput::Event(event));
}

pub(crate) async fn run<W: AsyncWrite + Unpin>(
    mut link: Link<W>,
    thread: &str,
    opts: &SpawnOptions,
    mut commands: UnboundedReceiver<AgentCommand>,
    out: &UnboundedSender<AgentOutput>,
) -> anyhow::Result<()> {
    link.following(thread, true).await?;
    let mut session = Session {
        mirror: Mirror::new(thread),
        queue: VecDeque::new(),
        pending: HashMap::new(),
        starting: None,
        started_turn: None,
        interrupt_pending: false,
        ready: false,
        synced: false,
        model: opts.model.clone(),
        effort: opts.effort.clone(),
        speed: opts.speed.clone(),
        permission: opts.permission.clone(),
    };
    let deadline = tokio::time::sleep(TIMEOUT);
    tokio::pin!(deadline);
    let result = async {
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(AgentCommand::Shutdown) | None => return Ok(()),
                    Some(command) => session.command(command, &mut link, thread, out).await?,
                },
                frame = link.next() => {
                    let frame = frame?;
                    if link.disconnected(&frame) { return Err(bus::Closed.into()); }
                    session.frame(frame, &mut link, thread, out).await?;
                },
                _ = &mut deadline, if !session.ready => bail!("等待 Codex 桌面版对话快照超时"),
                _ = out.closed() => return Ok(()),
            }
            session.drain(&mut link, thread, out).await?;
        }
    }
    .await;
    // Closing a follower only releases its subscription; never stops the owner.
    let _ = link.following(thread, false).await;
    result
}

impl Session {
    fn working(&mut self, out: &UnboundedSender<AgentOutput>) {
        self.mirror.set_working(true);
        emit(out, translate::state(SessionState::Working));
    }

    fn idle(&mut self, out: &UnboundedSender<AgentOutput>) {
        self.mirror.set_working(false);
        emit(out, EventKind::TurnDone { summary: None });
        emit(out, translate::state(SessionState::Idle));
    }

    async fn command<W: AsyncWrite + Unpin>(
        &mut self,
        command: AgentCommand,
        link: &mut Link<W>,
        thread: &str,
        out: &UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<()> {
        match command {
            AgentCommand::Send { text, images } => {
                self.working(out);
                self.queue.push_back((text, images));
            }
            AgentCommand::Interrupt => {
                let queued = !self.queue.is_empty();
                self.queue.clear();
                for approval in self.mirror.approval_ids() {
                    self.mirror.answer(&approval);
                    emit(
                        out,
                        EventKind::ApprovalDone {
                            approval,
                            decision: Decision::Abort,
                        },
                    );
                }
                self.interrupt_pending =
                    !self.ready || self.mirror.busy() || self.starting.is_some();
                if self.mirror.busy() {
                    self.working(out);
                }
                if queued && !self.interrupt_pending {
                    self.idle(out);
                }
            }
            AgentCommand::Answer { approval, decision } => {
                if let Some(asked) = self.mirror.answer(&approval) {
                    let method = if asked.method == mirror::FILE_APPROVAL {
                        "thread-follower-file-approval-decision"
                    } else {
                        "thread-follower-command-approval-decision"
                    };
                    let id = link.request(method, version::APPROVAL_DECISION,
                        json!({"conversationId":thread,"requestId":asked.id,"decision":translate::decision(decision)})).await?;
                    self.pending.insert(id, Pending::Answer);
                    emit(out, EventKind::ApprovalDone { approval, decision });
                    self.working(out);
                    if self.mirror.waiting() {
                        emit(out, translate::state(SessionState::NeedsYou));
                    }
                } else {
                    emit(
                        out,
                        translate::notice(NoticeLevel::Warn, "审批已结束或不存在"),
                    );
                }
            }
            AgentCommand::SetModel {
                model,
                effort,
                speed,
                permission,
            } => {
                if permission.is_some() {
                    self.permission = permission;
                }
                if model.is_some() {
                    self.model = model;
                }
                if effort.is_some() {
                    self.effort = effort;
                }
                if speed.is_some() {
                    self.speed = speed;
                }
                emit(
                    out,
                    translate::notice(
                        NoticeLevel::Info,
                        format!(
                            "下一条消息起使用 {}（思考强度：{}，速度：{}{}）",
                            self.model.as_deref().unwrap_or("默认模型"),
                            self.effort.as_deref().unwrap_or("默认"),
                            translate::speed_word(self.speed.as_deref()),
                            translate::permission_part(self.permission.as_deref()),
                        ),
                    ),
                );
            }
            AgentCommand::Shutdown => {}
        }
        Ok(())
    }

    async fn drain<W: AsyncWrite + Unpin>(
        &mut self,
        link: &mut Link<W>,
        thread: &str,
        out: &UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<()> {
        if !self.ready || !self.synced {
            return Ok(());
        }
        if self
            .started_turn
            .as_deref()
            .is_some_and(|turn| self.mirror.has_turn(turn))
        {
            self.starting = None;
            self.started_turn = None;
        }
        if self.interrupt_pending {
            if let Some(turn) = self
                .mirror
                .active_turn()
                .or_else(|| self.started_turn.clone())
            {
                let id = link
                    .request(
                        "thread-follower-interrupt-turn",
                        version::INTERRUPT_TURN,
                        json!({"conversationId":thread,"mode":"user-stop","expectedTurnId":turn}),
                    )
                    .await?;
                self.pending.insert(id, Pending::Interrupt);
                self.interrupt_pending = false;
            } else if self.starting.is_none() && !self.mirror.busy() {
                self.interrupt_pending = false;
                if self.queue.is_empty() && self.mirror.working() {
                    self.idle(out);
                }
            }
        }
        if self.starting.is_none()
            && !self.mirror.busy()
            && !self.interrupt_pending
            && let Some((text, images)) = self.queue.pop_front()
        {
            // A previous completion can have emitted Idle while this was queued.
            if !self.mirror.working() {
                self.working(out);
            }
            self.mirror.sent(&text);
            let mut request = json!({"threadId":thread,"input":translate::input(&text, &images)});
            if let Some(model) = &self.model {
                request["model"] = json!(model);
            }
            if let Some(effort) = &self.effort {
                request["effort"] = json!(effort);
            }
            if let Some(speed) = &self.speed {
                request["serviceTier"] = json!(speed);
            }
            if let Some(permission) = &self.permission {
                request["permissions"] = json!(permission);
            }
            let id = link
                .request(
                    "thread-follower-start-turn",
                    version::START_TURN,
                    json!({"conversationId":thread,"turnStart":{"request":request,"context":{}}}),
                )
                .await?;
            self.starting = Some(id.clone());
            self.pending.insert(id, Pending::Start(text));
        }
        Ok(())
    }

    async fn frame<W: AsyncWrite + Unpin>(
        &mut self,
        frame: Value,
        link: &mut Link<W>,
        thread: &str,
        out: &UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<()> {
        if frame["type"] == "client-discovery-request" {
            return link.send(&bus::decline(&frame)).await;
        }
        if frame["type"] == "response" {
            if let Some(pending) = frame["requestId"]
                .as_str()
                .and_then(|id| self.pending.remove(id))
            {
                if frame["resultType"] == "error" && frame["error"] == "client-disconnected" {
                    return Err(bus::Closed.into());
                }
                match pending {
                    Pending::Start(text) => {
                        if frame["resultType"] == "success" {
                            self.started_turn = Some(
                                frame["result"]["result"]["turn"]["id"]
                                    .as_str()
                                    .filter(|id| !id.is_empty())
                                    .context("Codex desktop start-turn returned no turn id")?
                                    .to_owned(),
                            );
                        } else {
                            self.starting = None;
                            self.started_turn = None;
                            self.mirror.unsent(&text);
                            emit(
                                out,
                                translate::notice(
                                    NoticeLevel::Error,
                                    translate::error_text(&frame["error"]),
                                ),
                            );
                            if !self.mirror.busy() {
                                self.idle(out);
                            }
                        }
                    }
                    Pending::Interrupt | Pending::Answer => {
                        if frame["resultType"] != "success" {
                            emit(
                                out,
                                translate::notice(
                                    NoticeLevel::Error,
                                    translate::error_text(&frame["error"]),
                                ),
                            );
                        }
                    }
                }
            }
            return Ok(());
        }
        if frame["type"] != "broadcast"
            || frame["method"] != "thread-stream-state-changed"
            || frame["params"]["conversationId"] != thread
            || frame["params"]["hostId"] != "local"
        {
            return Ok(());
        }
        if frame["version"] != version::STATE_CHANGED {
            tracing::warn!(version = %frame["version"], "Codex desktop state version is not supported");
            return Err(bus::Updated.into());
        }
        let change = &frame["params"]["change"];
        let events = match change["type"].as_str() {
            Some("snapshot") => {
                let revision = change["revision"]
                    .as_u64()
                    .context("snapshot missing revision")?;
                let state = change
                    .get("conversationState")
                    .filter(|s| s.is_object())
                    .context("snapshot missing conversationState")?;
                if !self.ready {
                    let _ = out.send(AgentOutput::NativeId(thread.to_owned()));
                }
                let events = self.mirror.snapshot(state.clone(), revision, !self.ready);
                self.ready = true;
                self.synced = true;
                events
            }
            Some("patches") if self.ready && self.synced => {
                let base = change["baseRevision"]
                    .as_u64()
                    .context("patch missing baseRevision")?;
                let revision = change["revision"]
                    .as_u64()
                    .context("patch missing revision")?;
                let patches = change["patches"]
                    .as_array()
                    .context("patch missing patches")?;
                match self.mirror.patch(base, revision, patches) {
                    Applied::Events(events) => events,
                    Applied::Stale => {
                        self.synced = false;
                        link.following(thread, true).await?;
                        Vec::new()
                    }
                }
            }
            _ => Vec::new(), // Patches before the initial/recovery snapshot have no base.
        };
        for event in events {
            emit(out, event);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "follow_tests.rs"]
mod tests;

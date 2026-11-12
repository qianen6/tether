use crate::{
    Fault, Result, State, lock, notify,
    sessions::{HISTORY_PAGE, Launch, NewSession, Subscription},
};
use axum::{
    Router,
    extract::{
        State as AxumState, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    response::Response,
    routing::get,
};
use futures::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tether_agent_api::{
    AgentAdapter, AgentCommand, HistoryQuery, HistoryRead, ModelQuery, NoHistory,
};
use tether_proto::*;
use tether_store::Device;
use tether_terminals::TermChunk;
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinHandle,
    time::{Instant, timeout},
};
use uuid::Uuid;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an agent that let go of a conversation gets to exit before its
/// desktop app opens it.
const HANDOVER: Duration = Duration::from_millis(2500);

type TokenHash = [u8; 32];
fn token_hash(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

#[derive(Default)]
pub(crate) struct Connections {
    entries: HashMap<Uuid, Entry>,
    tokens: HashMap<String, HashSet<TokenHash>>,
    revoked: HashSet<TokenHash>,
    stopping: bool,
}
struct Entry {
    device: Option<String>,
    close: watch::Sender<Option<u16>>,
}

impl Connections {
    pub(crate) fn devices(&self) -> Vec<String> {
        let mut ids: Vec<_> = self
            .entries
            .values()
            .filter_map(|e| e.device.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        ids.sort();
        ids
    }
    pub(crate) fn contains(&self, device: Option<&str>) -> bool {
        self.entries
            .values()
            .any(|e| device.is_none_or(|id| e.device.as_deref() == Some(id)))
    }
    pub(crate) fn revoke(&mut self, id: &str) {
        if let Some(tokens) = self.tokens.remove(id) {
            self.revoked.extend(tokens);
        }
        for entry in self
            .entries
            .values()
            .filter(|e| e.device.as_deref() == Some(id))
        {
            entry.close.send_replace(Some(close::UNAUTHORIZED));
        }
    }
    pub(crate) fn shutdown(&mut self) {
        self.stopping = true;
        for entry in self.entries.values() {
            entry.close.send_replace(Some(1001));
        }
    }
}

struct Registration {
    state: Arc<State>,
    id: Uuid,
}
impl Drop for Registration {
    fn drop(&mut self) {
        lock(&self.state.connections).entries.remove(&self.id);
        self.state.connections_changed.notify_waiters();
    }
}

pub(crate) fn router(state: Arc<State>) -> Router {
    Router::new()
        .route("/v1/ws", get(upgrade))
        .with_state(state)
}

async fn upgrade(AxumState(state): AxumState<Arc<State>>, ws: WebSocketUpgrade) -> Response {
    // The app must consume and discard oversized messages and keep the socket
    // usable. A tungstenite size cap closes the stream before we can do that.
    ws.max_frame_size(usize::MAX)
        .max_message_size(usize::MAX)
        .on_upgrade(move |socket| connection(state, socket))
}

async fn direct(socket: &mut WebSocket, message: Message) -> bool {
    matches!(
        timeout(WRITE_TIMEOUT, socket.send(message)).await,
        Ok(Ok(()))
    )
}
fn json(message: &ServerMsg) -> Message {
    // Every ServerMsg variant contains JSON-serializable fields only.
    let text = serde_json::to_string(message).unwrap_or_else(|_| "{\"t\":\"error\",\"req\":null,\"code\":\"internal\",\"message\":\"serialization failed\"}".into());
    Message::Text(text.into())
}
fn closing(code: u16) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: "".into(),
    }))
}
fn frame_len(frame: &Message) -> usize {
    match frame {
        Message::Text(text) => text.len(),
        Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes) => bytes.len(),
        Message::Close(_) => 0,
    }
}

enum AuthError {
    Reject(RejectReason),
    Internal,
}

/// Synchronous so the registry lock protects lookup AND registration against
/// revoke. No raw device token is retained by the server.
fn authenticate(
    state: &State,
    id: Uuid,
    first: ClientMsg,
) -> std::result::Result<(Device, Option<String>), AuthError> {
    let mut connections = lock(&state.connections);
    if connections.stopping {
        return Err(AuthError::Internal);
    }
    let (device, token) = match first {
        ClientMsg::Pair {
            version,
            secret,
            device_name,
        } => {
            if version != PROTOCOL_VERSION {
                return Err(AuthError::Reject(RejectReason::VersionMismatch));
            }
            lock(&state.pairing)
                .claim(&secret)
                .map_err(AuthError::Reject)?;
            let name: String = device_name.chars().take(64).collect();
            let (device, token) = state
                .store
                .issue_device(if name.is_empty() { "phone" } else { &name })
                .map_err(|e| {
                    tracing::error!(error = %e, "device issuance failed");
                    AuthError::Internal
                })?;
            connections
                .tokens
                .entry(device.id.clone())
                .or_default()
                .insert(token_hash(&token));
            (device, Some(token))
        }
        ClientMsg::Hello { version, token, .. } => {
            if version != PROTOCOL_VERSION {
                return Err(AuthError::Reject(RejectReason::VersionMismatch));
            }
            let hash = token_hash(&token);
            if connections.revoked.contains(&hash) {
                return Err(AuthError::Reject(RejectReason::Revoked));
            }
            let device = state
                .store
                .device_by_token(&token)
                .map_err(|e| {
                    tracing::error!(error = %e, "device lookup failed");
                    AuthError::Internal
                })?
                .ok_or(AuthError::Reject(RejectReason::InvalidToken))?;
            connections
                .tokens
                .entry(device.id.clone())
                .or_default()
                .insert(hash);
            (device, None)
        }
        _ => return Err(AuthError::Internal),
    };
    state.store.touch_device(&device.id).map_err(|e| {
        tracing::error!(error = %e, "device touch failed");
        AuthError::Internal
    })?;
    let entry = connections
        .entries
        .get_mut(&id)
        .ok_or(AuthError::Internal)?;
    entry.device = Some(device.id.clone());
    if token.is_some() {
        let _ = state.paired.send(device.clone());
    }
    Ok((device, token))
}

async fn handshake(
    state: &State,
    id: Uuid,
    socket: &mut WebSocket,
    control: &mut watch::Receiver<Option<u16>>,
) -> bool {
    let first = tokio::select! {
        biased;
        _ = control.changed() => { let _ = direct(socket, closing(1001)).await; return false; }
        first = timeout(HANDSHAKE_TIMEOUT, socket.next()) => first,
    };
    let first = match first {
        Ok(Some(Ok(Message::Text(text)))) if text.len() <= MAX_FRAME => {
            match serde_json::from_str::<ClientMsg>(&text) {
                Ok(first @ (ClientMsg::Pair { .. } | ClientMsg::Hello { .. })) => first,
                Ok(request) => {
                    direct(
                        socket,
                        json(&ServerMsg::error(
                            request.req(),
                            ErrorCode::Unauthorized,
                            "first frame must be pair or hello",
                        )),
                    )
                    .await;
                    direct(socket, closing(close::HANDSHAKE)).await;
                    return false;
                }
                Err(_) => {
                    direct(
                        socket,
                        json(&ServerMsg::error(
                            request_number(&text),
                            ErrorCode::BadRequest,
                            "invalid handshake",
                        )),
                    )
                    .await;
                    direct(socket, closing(close::HANDSHAKE)).await;
                    return false;
                }
            }
        }
        Ok(Some(Ok(_))) => {
            direct(
                socket,
                json(&ServerMsg::error(
                    None,
                    ErrorCode::BadRequest,
                    "invalid handshake frame",
                )),
            )
            .await;
            direct(socket, closing(close::HANDSHAKE)).await;
            return false;
        }
        _ => {
            direct(socket, closing(close::HANDSHAKE)).await;
            return false;
        }
    };
    match authenticate(state, id, first) {
        Ok((device, token)) => {
            if let Some(token) = token
                && !direct(
                    socket,
                    json(&ServerMsg::Paired {
                        device_id: device.id.clone(),
                        token,
                    }),
                )
                .await
            {
                return false;
            }
            direct(
                socket,
                json(&ServerMsg::Welcome {
                    version: PROTOCOL_VERSION,
                    host_name: state.host_name.clone(),
                    device_id: device.id,
                    server_time: now_ms(),
                }),
            )
            .await
        }
        Err(AuthError::Reject(reason)) => {
            direct(socket, json(&ServerMsg::Rejected { reason })).await;
            direct(
                socket,
                closing(if reason == RejectReason::VersionMismatch {
                    close::VERSION_MISMATCH
                } else {
                    close::UNAUTHORIZED
                }),
            )
            .await;
            false
        }
        Err(AuthError::Internal) => {
            direct(
                socket,
                json(&ServerMsg::error(
                    None,
                    ErrorCode::Internal,
                    "authentication backend failed",
                )),
            )
            .await;
            direct(socket, closing(1011)).await;
            false
        }
    }
}

#[derive(Clone)]
struct Outbox {
    queue: mpsc::Sender<Message>,
    control: watch::Sender<Option<u16>>,
}
impl Outbox {
    async fn frame(&self, message: Message) -> bool {
        let mut control = self.control.subscribe();
        if control.borrow().is_some() {
            return false;
        }
        tokio::select! {
            biased;
            _ = control.changed() => false,
            result = timeout(WRITE_TIMEOUT, self.queue.send(message)) => match result {
                Ok(Ok(())) => true,
                _ => { self.control.send_replace(Some(1013)); false }
            }
        }
    }
    async fn send(&self, message: &ServerMsg) -> bool {
        let frame = json(message);
        if frame_len(&frame) > MAX_FRAME {
            let req = match message {
                ServerMsg::Ok { req, .. } => Some(*req),
                ServerMsg::Error { req, .. } => *req,
                _ => None,
            };
            return self
                .frame(json(&ServerMsg::error(
                    req,
                    ErrorCode::BadRequest,
                    "outgoing message exceeds 8 MiB",
                )))
                .await;
        }
        self.frame(frame).await
    }
    async fn bytes(&self, term: &str, bytes: &[u8]) -> bool {
        if term.is_empty() || term.len() > u8::MAX as usize {
            return false;
        }
        for chunk in bytes.chunks(MAX_FRAME - 2 - term.len()) {
            if !self
                .frame(Message::Binary(
                    binary::encode(binary::TERM_OUTPUT, term, chunk).into(),
                ))
                .await
            {
                return false;
            }
        }
        true
    }
}

struct Worker {
    task: JoinHandle<()>,
    _subscription: Option<Subscription>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn connection(state: Arc<State>, mut socket: WebSocket) {
    let id = Uuid::new_v4();
    let (control, mut closing_rx) = watch::channel(None);
    {
        let mut connections = lock(&state.connections);
        if connections.stopping {
            return;
        }
        connections.entries.insert(
            id,
            Entry {
                device: None,
                close: control.clone(),
            },
        );
    }
    let _registration = Registration {
        state: state.clone(),
        id,
    };
    // Register for host-wide pushes before authentication as well.
    let pushes = state.bus.subscribe();
    if !handshake(&state, id, &mut socket, &mut closing_rx).await {
        return;
    }
    let (mut sink, mut stream) = socket.split();
    let (queue, mut outgoing) = mpsc::channel::<Message>(64);
    let out = Outbox {
        queue,
        control: control.clone(),
    };
    let mut writer_control = control.subscribe();
    let writer = tokio::spawn(async move {
        loop {
            let code = *writer_control.borrow();
            if let Some(code) = code {
                let _ = timeout(Duration::from_secs(1), sink.send(closing(code))).await;
                break;
            }
            let frame = tokio::select! {
                biased;
                _ = writer_control.changed() => continue,
                frame = outgoing.recv() => match frame { Some(frame) => frame, None => break },
            };
            let sent = tokio::select! {
                biased;
                _ = writer_control.changed() => continue,
                sent = timeout(WRITE_TIMEOUT, sink.send(frame)) => sent,
            };
            if !matches!(sent, Ok(Ok(()))) {
                break;
            }
        }
    });
    let push_state = state.clone();
    let push_out = out.clone();
    let push_task = Worker {
        task: tokio::spawn(async move {
            forward_pushes(push_state, pushes, push_out).await;
        }),
        _subscription: None,
    };
    let mut connection = Client {
        state,
        out,
        sessions: HashMap::new(),
        terms: HashMap::new(),
        last_req: None,
    };
    let idle = tokio::time::sleep(IDLE_TIMEOUT);
    tokio::pin!(idle);
    loop {
        if closing_rx.borrow().is_some() {
            break;
        }
        tokio::select! {
            biased;
            _ = closing_rx.changed() => break,
            _ = &mut idle => break,
            _ = connection.out.queue.closed() => break,
            frame = stream.next() => {
                idle.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
                let Some(Ok(frame)) = frame else { break; };
                if matches!(frame, Message::Close(_)) { break; }
                let keep = tokio::select! {
                    biased;
                    _ = closing_rx.changed() => false,
                    keep = connection.frame(frame) => keep,
                };
                if !keep { break; }
            }
        }
    }
    if control.borrow().is_none() {
        control.send_replace(Some(1000));
    }
    drop(connection);
    drop(push_task);
    let _ = writer.await;
}

fn request_number(text: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()?
        .get("req")?
        .as_u64()
}

struct Client {
    state: Arc<State>,
    out: Outbox,
    sessions: HashMap<String, Worker>,
    terms: HashMap<String, Worker>,
    last_req: Option<u64>,
}

impl Client {
    async fn frame(&mut self, frame: Message) -> bool {
        if frame_len(&frame) > MAX_FRAME {
            return self
                .out
                .send(&ServerMsg::error(
                    None,
                    ErrorCode::BadRequest,
                    "frame exceeds 8 MiB",
                ))
                .await;
        }
        match frame {
            Message::Text(text) => {
                let request = match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(request) => request,
                    Err(_) => {
                        return self
                            .out
                            .send(&ServerMsg::error(
                                request_number(&text),
                                ErrorCode::BadRequest,
                                "invalid request",
                            ))
                            .await;
                    }
                };
                let req = request.req();
                if let Some(req) = req {
                    if self.last_req.is_some_and(|last| req <= last) {
                        return self
                            .out
                            .send(&ServerMsg::error(
                                Some(req),
                                ErrorCode::BadRequest,
                                "req must increase within a connection",
                            ))
                            .await;
                    }
                    self.last_req = Some(req);
                }
                if let Err(error) = self.request(request).await {
                    return self.out.send(&error.response(req)).await;
                }
                true
            }
            Message::Binary(bytes) => {
                let result = (|| -> Result<()> {
                    let (kind, term, data) = binary::decode(&bytes)
                        .map_err(|_| Fault::new(ErrorCode::BadRequest, "invalid terminal frame"))?;
                    if kind != binary::TERM_INPUT {
                        return Err(Fault::new(ErrorCode::BadRequest, "expected terminal input"));
                    }
                    self.term_info(term)?;
                    if !self.terms.contains_key(term) {
                        return Err(Fault::new(
                            ErrorCode::Unauthorized,
                            "terminal is not attached",
                        ));
                    }
                    self.state.terms.input(term, data)?;
                    Ok(())
                })();
                match result {
                    Ok(()) => true,
                    Err(error) => self.out.send(&error.response(None)).await,
                }
            }
            Message::Ping(bytes) => self.out.frame(Message::Pong(bytes)).await,
            Message::Pong(_) => true,
            Message::Close(_) => false,
        }
    }

    fn history_agent(&self, agent: &str) -> Result<Arc<dyn AgentAdapter>> {
        let adapter = self
            .state
            .agents
            .get(agent)
            .cloned()
            .ok_or_else(|| Fault::new(ErrorCode::AgentUnavailable, "unknown agent"))?;
        if !adapter.info().available {
            return Err(Fault::new(ErrorCode::AgentUnavailable, "agent unavailable"));
        }
        Ok(adapter)
    }

    async fn ok<T: serde::Serialize>(&self, req: u64, data: &T) {
        self.out.send(&ServerMsg::ok(req, data)).await;
    }

    async fn request(&mut self, request: ClientMsg) -> Result<()> {
        match request {
            ClientMsg::Pair { .. } | ClientMsg::Hello { .. } => {
                return Err(Fault::new(
                    ErrorCode::BadRequest,
                    "connection already authenticated",
                ));
            }
            ClientMsg::ListAgents { req } => {
                let mut agents: Vec<_> = self.state.agents.values().map(|a| a.info()).collect();
                agents.sort_by(|a, b| a.id.cmp(&b.id));
                self.ok(req, &AgentsData { agents }).await;
            }
            ClientMsg::ListSessions { req } => {
                self.ok(
                    req,
                    &SessionsData {
                        sessions: self.state.sessions.list(),
                    },
                )
                .await
            }
            ClientMsg::CreateSession {
                req,
                agent,
                cwd,
                title,
                resume,
                model,
                effort,
                chat,
                project,
                roots,
                speed,
                permission,
                reveal,
                fork,
            } => {
                check_name(model.as_deref(), "model")?;
                check_name(effort.as_deref(), "effort")?;
                check_name(speed.as_deref(), "speed")?;
                check_name(permission.as_deref(), "permission")?;
                check_name(project.as_deref(), "project")?;
                let fork = fork.unwrap_or(false);
                if fork && resume.is_none() {
                    return Err(Fault::new(ErrorCode::BadRequest, "fork needs resume"));
                }
                if let Some(thread) = &resume {
                    check_thread(thread)?;
                }
                // A copy leaves the original, and whoever has it, alone.
                let (resume, fork) = if fork { (None, resume) } else { (resume, None) };
                if let Some(existing) = resume
                    .as_ref()
                    .and_then(|thread| self.state.sessions.resuming(&agent, thread))
                {
                    let info = existing.info();
                    // Mid-turn: keep it. Otherwise start again from the
                    // record on disk, which may have moved on elsewhere.
                    if matches!(info.state, SessionState::Working | SessionState::NeedsYou) {
                        self.ok(req, &SessionData { session: info }).await;
                        return Ok(());
                    }
                    self.state.sessions.close(&info.id).await?;
                }
                let chat = chat.unwrap_or(false);
                // A new chat gets a folder of its own; the phone's is ignored.
                let cwd = if chat && resume.is_none() && fork.is_none() {
                    None
                } else {
                    cwd
                };
                let roots = roots
                    .unwrap_or_default()
                    .into_iter()
                    .take(32)
                    // A folder gone missing is left out, not an error.
                    .filter_map(|root| cwd_path(Some(root)).ok())
                    .collect::<Vec<_>>();
                let new = NewSession {
                    cwd: cwd_path(cwd)?,
                    title,
                    resume,
                    model,
                    effort,
                    chat,
                    speed,
                    permission,
                    launch: Launch {
                        project: if chat { None } else { project },
                        roots: if chat { Vec::new() } else { roots },
                        reveal: reveal.unwrap_or(false),
                        fork,
                    },
                };
                let session = self.state.sessions.create(&agent, new)?;
                self.ok(req, &SessionData { session }).await;
            }
            ClientMsg::ListModels { req, agent } => {
                let adapter = self.history_agent(&agent)?;
                let out = self.out.clone();
                tokio::spawn(async move {
                    let (models, permissions) = tokio::join!(
                        adapter.list_models(ModelQuery::default()),
                        adapter.list_permissions(ModelQuery::default())
                    );
                    // No permissions to offer is no reason to offer no models.
                    let permissions = permissions.unwrap_or_else(|error| {
                        tracing::warn!(error = %format!("{error:#}"), "listing permissions failed");
                        Default::default()
                    });
                    let reply = match models {
                        Ok(models) => ServerMsg::ok(
                            req,
                            &ModelsData {
                                models,
                                permissions: permissions.list,
                                default_permission: permissions.default,
                            },
                        ),
                        Err(error) => {
                            tracing::warn!(error = %format!("{error:#}"), "listing models failed");
                            Fault::new(ErrorCode::Internal, format!("读取模型列表失败：{error:#}"))
                                .response(Some(req))
                        }
                    };
                    out.send(&reply).await;
                });
            }
            ClientMsg::SetModel {
                req,
                session,
                model,
                effort,
                speed,
                permission,
            } => {
                check_name(model.as_deref(), "model")?;
                check_name(effort.as_deref(), "effort")?;
                check_name(speed.as_deref(), "speed")?;
                check_name(permission.as_deref(), "permission")?;
                self.state
                    .sessions
                    .get(&session)?
                    .command(AgentCommand::SetModel {
                        model,
                        effort,
                        speed,
                        permission,
                    })
                    .await?;
                self.ok(req, &Empty {}).await;
            }
            ClientMsg::Reveal { req, agent, thread } => {
                check_thread(&thread)?;
                let adapter = self.history_agent(&agent)?;
                // Only one program may continue a conversation: a session of
                // ours on it lets go first, so the app can take it over.
                let Some(session) = self.state.sessions.resuming(&agent, &thread) else {
                    adapter
                        .reveal(&thread)
                        .map_err(|error| Fault::new(ErrorCode::BadRequest, format!("{error:#}")))?;
                    self.ok(req, &Empty {}).await;
                    return Ok(());
                };
                session.release().await?;
                let out = self.out.clone();
                tokio::spawn(async move {
                    // Its agent exits on its own time; open the app after that.
                    tokio::time::sleep(HANDOVER).await;
                    let reply = match adapter.reveal(&thread) {
                        Ok(()) => ServerMsg::ok(req, &Empty {}),
                        Err(error) => Fault::new(ErrorCode::BadRequest, format!("{error:#}"))
                            .response(Some(req)),
                    };
                    out.send(&reply).await;
                });
            }
            ClientMsg::ListHistory {
                req,
                agent,
                search,
                cursor,
            } => {
                let adapter = self.history_agent(&agent)?;
                let out = self.out.clone();
                // Reading starts a short-lived agent process: off the connection loop.
                tokio::spawn(async move {
                    let query = HistoryQuery {
                        search,
                        cursor,
                        limit: THREADS_PAGE,
                        program: None,
                    };
                    let reply = match adapter.list_history(query).await {
                        Ok(data) => ServerMsg::ok(req, &data),
                        Err(error) => history_fault(error).response(Some(req)),
                    };
                    out.send(&reply).await;
                });
            }
            ClientMsg::ReadHistory { req, agent, thread } => {
                check_thread(&thread)?;
                let adapter = self.history_agent(&agent)?;
                let out = self.out.clone();
                tokio::spawn(async move {
                    let read = HistoryRead {
                        thread,
                        program: None,
                    };
                    let reply = match adapter.read_history(read).await {
                        Ok(data) => ServerMsg::ok(req, &data),
                        Err(error) => history_fault(error).response(Some(req)),
                    };
                    out.send(&reply).await;
                });
            }
            ClientMsg::Subscribe {
                req,
                session,
                after,
            } => {
                let handle = self.state.sessions.get(&session)?;
                let (subscription, events) = handle.subscribe();
                self.sessions.remove(&session);
                self.ok(
                    req,
                    &SessionData {
                        session: handle.info(),
                    },
                )
                .await;
                let state = self.state.clone();
                let out = self.out.clone();
                let id = session.clone();
                let task = tokio::spawn(async move {
                    forward_session(state, id, after, events, out).await;
                });
                self.sessions.insert(
                    session,
                    Worker {
                        task,
                        _subscription: Some(subscription),
                    },
                );
            }
            ClientMsg::Unsubscribe { req, session } => {
                self.state.sessions.get(&session)?;
                self.sessions.remove(&session);
                self.ok(req, &Empty {}).await;
            }
            ClientMsg::SendMessage {
                req,
                session,
                text,
                images,
            } => {
                self.state
                    .sessions
                    .get(&session)?
                    .command(AgentCommand::Send {
                        text,
                        images: images.unwrap_or_default(),
                    })
                    .await?;
                self.ok(req, &Empty {}).await;
            }
            ClientMsg::Answer {
                req,
                session,
                approval,
                decision,
            } => {
                self.state
                    .sessions
                    .get(&session)?
                    .command(AgentCommand::Answer { approval, decision })
                    .await?;
                self.ok(req, &Empty {}).await;
            }
            ClientMsg::Interrupt { req, session } => {
                self.state
                    .sessions
                    .get(&session)?
                    .command(AgentCommand::Interrupt)
                    .await?;
                self.ok(req, &Empty {}).await;
            }
            ClientMsg::CloseSession { req, session } => {
                self.state.sessions.close(&session).await?;
                self.sessions.remove(&session);
                self.ok(req, &Empty {}).await;
            }
            ClientMsg::TermOpen {
                req,
                command,
                agent,
                cwd,
                cols,
                rows,
            } => {
                dimensions(cols, rows)?;
                let command = match (command, agent) {
                    (Some(command), None) if command.first().is_some_and(|s| !s.is_empty()) => {
                        command
                    }
                    (None, Some(agent)) => {
                        let info = self
                            .state
                            .agents
                            .get(&agent)
                            .map(|a| a.info())
                            .filter(|a| a.available && !a.terminal_command.is_empty())
                            .ok_or_else(|| {
                                Fault::new(
                                    ErrorCode::AgentUnavailable,
                                    "agent has no terminal command",
                                )
                            })?;
                        info.terminal_command
                    }
                    _ => {
                        return Err(Fault::new(
                            ErrorCode::BadRequest,
                            "specify exactly one nonempty command or agent",
                        ));
                    }
                };
                let term = self.state.terms.open(command, cwd_path(cwd)?, cols, rows)?;
                if let Err(error) = notify::monitor(&self.state, &term.id) {
                    let _ = self.state.terms.kill(&term.id);
                    return Err(error.into());
                }
                self.attach(req, term).await?;
            }
            ClientMsg::TermAttach { req, term } => {
                let info = self.term_info(&term)?;
                self.attach(req, info).await?;
            }
            ClientMsg::TermDetach { req, term } => {
                self.term_info(&term)?;
                self.terms.remove(&term);
                self.ok(req, &Empty {}).await;
            }
            ClientMsg::TermResize {
                req,
                term,
                cols,
                rows,
            } => {
                dimensions(cols, rows)?;
                self.term_info(&term)?;
                self.state.terms.resize(&term, cols, rows)?;
                self.ok(req, &Empty {}).await;
            }
            ClientMsg::TermKill { req, term } => {
                self.term_info(&term)?;
                self.state.terms.kill(&term)?;
                self.ok(req, &Empty {}).await;
            }
            ClientMsg::ListTerms { req } => {
                self.ok(
                    req,
                    &TermsData {
                        terms: self.state.terms.list(),
                    },
                )
                .await
            }
            ClientMsg::PutFile { req, name, data } => {
                let saved = crate::files::put(&crate::files::uploads(), &name, &data).await?;
                self.ok(req, &saved).await
            }
            ClientMsg::GetFile { req, path } => {
                let file = crate::files::get(&path).await?;
                self.ok(req, &file).await
            }
            ClientMsg::Ping { n } => {
                self.out.send(&ServerMsg::Pong { n }).await;
            }
        }
        Ok(())
    }

    fn term_info(&self, term: &str) -> Result<TermInfo> {
        self.state
            .terms
            .list()
            .into_iter()
            .find(|t| t.id == term)
            .ok_or_else(|| Fault::new(ErrorCode::NotFound, "terminal not found"))
    }

    async fn attach(&mut self, req: u64, info: TermInfo) -> Result<()> {
        if info.id.is_empty() || info.id.len() > u8::MAX as usize {
            return Err(Fault::new(
                ErrorCode::Internal,
                "invalid terminal id from backend",
            ));
        }
        notify::monitor(&self.state, &info.id)?;
        let (scrollback, receiver) = self.state.terms.attach(&info.id)?;
        self.terms.remove(&info.id);
        self.ok(req, &TermData { term: info.clone() }).await;
        let term = info.id.clone();
        let out = self.out.clone();
        let task = tokio::spawn(async move {
            forward_term(info, scrollback, receiver, out).await;
        });
        self.terms.insert(
            term,
            Worker {
                task,
                _subscription: None,
            },
        );
        Ok(())
    }
}

/// The user's home directory: the default working directory. The server's own
/// is no use (a desktop app started by Windows runs in `C:\Windows`).
fn home() -> Option<PathBuf> {
    std::env::home_dir().filter(|p| p.is_dir())
}

/// No cwd, or `~`, means home; `~/x` is under it.
pub(crate) fn cwd_path(cwd: Option<String>) -> Result<PathBuf> {
    let cwd = cwd.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
    let path = match cwd.as_deref() {
        None | Some("~") => home()
            .map(Ok)
            .unwrap_or_else(|| std::env::current_dir().map_err(anyhow::Error::from))?,
        Some(c) => match c.strip_prefix("~/").or_else(|| c.strip_prefix("~\\")) {
            Some(rest) => home()
                .ok_or_else(|| Fault::new(ErrorCode::BadRequest, "no home directory"))?
                .join(rest),
            None => PathBuf::from(c),
        },
    };
    if !path.is_dir() {
        return Err(Fault::new(
            ErrorCode::BadRequest,
            "cwd must be an existing directory",
        ));
    }
    std::fs::canonicalize(path)
        .map(simplify)
        .map_err(anyhow::Error::from)
        .map_err(Fault::from)
}

/// `\\?\C:\x` → `C:\x` (Windows canonical paths), when that means the same.
fn simplify(path: PathBuf) -> PathBuf {
    let Some(s) = path.to_str() else { return path };
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.len() >= 3 && rest.as_bytes()[1] == b':' && !rest.contains("..") => {
            PathBuf::from(rest)
        }
        Some(rest) if rest.starts_with(r"UNC\") => PathBuf::from(format!(r"\\{}", &rest[4..])),
        _ => path,
    }
}
fn dimensions(cols: u16, rows: u16) -> Result<()> {
    if cols == 0 || rows == 0 {
        return Err(Fault::new(
            ErrorCode::BadRequest,
            "terminal dimensions must be positive",
        ));
    }
    Ok(())
}

async fn replay(state: &State, session: &str, last: &mut u64, out: &Outbox) -> Result<bool> {
    loop {
        let history = state.store.events_after(session, *last, HISTORY_PAGE)?;
        if history.is_empty() {
            return Ok(true);
        }
        for event in history {
            if event.seq <= *last || event.kind.is_transient() {
                return Err(Fault::new(
                    ErrorCode::Internal,
                    "invalid stored event order",
                ));
            }
            let seq = event.seq;
            if !out
                .send(&ServerMsg::Event {
                    session: session.into(),
                    event,
                })
                .await
            {
                return Ok(false);
            }
            *last = seq;
        }
        tokio::task::yield_now().await;
    }
}

async fn forward_session(
    state: Arc<State>,
    session: String,
    mut last: u64,
    mut receiver: broadcast::Receiver<Event>,
    out: Outbox,
) {
    loop {
        match replay(&state, &session, &mut last, &out).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                out.send(&error.response(None)).await;
                out.control.send_replace(Some(1011));
                return;
            }
        }
        loop {
            match receiver.recv().await {
                Ok(event) if event.seq > last => {
                    let seq = event.seq;
                    if !out
                        .send(&ServerMsg::Event {
                            session: session.clone(),
                            event,
                        })
                        .await
                    {
                        return;
                    }
                    last = seq;
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => break, // durable recovery, never silently drop
                Err(broadcast::error::RecvError::Closed) => {
                    let _ = replay(&state, &session, &mut last, &out).await;
                    return;
                }
            }
        }
    }
}

async fn forward_term(
    info: TermInfo,
    scrollback: Vec<u8>,
    mut receiver: broadcast::Receiver<TermChunk>,
    out: Outbox,
) {
    if !out.bytes(&info.id, &scrollback).await {
        return;
    }
    if !info.running {
        out.send(&ServerMsg::TermExit {
            term: info.id,
            code: None,
        })
        .await;
        return;
    }
    loop {
        match receiver.recv().await {
            Ok(TermChunk::Data(bytes)) => {
                if !out.bytes(&info.id, &bytes).await {
                    return;
                }
            }
            Ok(TermChunk::Exit(code)) => {
                out.send(&ServerMsg::TermExit {
                    term: info.id,
                    code,
                })
                .await;
                return;
            }
            Ok(TermChunk::Attention) => {} // host-wide monitor owns the shared cooldown
            Err(broadcast::error::RecvError::Closed) => {
                out.send(&ServerMsg::TermExit {
                    term: info.id,
                    code: None,
                })
                .await;
                return;
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // The frozen terminal API has no offset/cursor. Replaying the
                // whole scrollback here would duplicate bytes; require reattach.
                out.send(&ServerMsg::error(
                    None,
                    ErrorCode::Busy,
                    "terminal output lagged; detach and attach again",
                ))
                .await;
                return;
            }
        }
    }
}

async fn forward_pushes(
    state: Arc<State>,
    mut receiver: broadcast::Receiver<ServerMsg>,
    out: Outbox,
) {
    let mut known: HashSet<_> = state.sessions.list().into_iter().map(|s| s.id).collect();
    loop {
        match receiver.recv().await {
            Ok(message) => {
                match &message {
                    ServerMsg::SessionUpdate { session } => {
                        known.insert(session.id.clone());
                    }
                    ServerMsg::SessionClosed { session } => {
                        known.remove(session);
                    }
                    _ => {}
                }
                if !out.send(&message).await {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let sessions = state.sessions.list();
                let current: HashSet<_> = sessions.iter().map(|s| s.id.clone()).collect();
                for session in known.difference(&current) {
                    if !out
                        .send(&ServerMsg::SessionClosed {
                            session: session.clone(),
                        })
                        .await
                    {
                        return;
                    }
                }
                for session in sessions {
                    if !out.send(&ServerMsg::SessionUpdate { session }).await {
                        return;
                    }
                }
                known = current;
            }
        }
    }
}

/// Threads per `list_history` page.
const THREADS_PAGE: u32 = 50;

/// A model or effort name: short, printable.
fn check_name(name: Option<&str>, what: &str) -> Result<()> {
    if let Some(name) = name
        && (name.trim().is_empty() || name.len() > 100 || name.chars().any(char::is_control))
    {
        return Err(Fault::new(ErrorCode::BadRequest, format!("invalid {what}")));
    }
    Ok(())
}

fn check_thread(thread: &str) -> Result<()> {
    if thread.is_empty() || thread.len() > 200 || thread.chars().any(char::is_control) {
        return Err(Fault::new(ErrorCode::BadRequest, "invalid thread id"));
    }
    Ok(())
}

/// An agent without history is the phone's mistake; anything else is the
/// agent failing, said in its own words (no secrets in Codex's errors).
fn history_fault(error: anyhow::Error) -> Fault {
    if error.is::<NoHistory>() {
        return Fault::new(ErrorCode::BadRequest, error.to_string());
    }
    tracing::warn!(error = %format!("{error:#}"), "reading agent history failed");
    Fault::new(ErrorCode::Internal, format!("读取历史失败：{error:#}"))
}

//! Process lifecycle and a transport-independent app-server state machine.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use serde_json::{Value, json};
use tether_agent_api::{AgentCommand, AgentOutput, SpawnOptions};
use tether_proto::{Decision, EventKind, NoticeLevel, SessionState};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
        BufReader,
    },
    process::{Child, Command},
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    time::{Instant, MissedTickBehavior},
};

use crate::{
    COMMAND,
    protocol::{Id, Message, Recording, Rpc},
    translate,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_GRACE: Duration = Duration::from_secs(2);

pub(crate) fn spawn(
    opts: SpawnOptions,
    out: UnboundedSender<AgentOutput>,
    via_desktop: fn() -> bool,
) -> anyhow::Result<UnboundedSender<AgentCommand>> {
    let runtime = tokio::runtime::Handle::try_current()
        .context("CodexAdapter::spawn needs a Tokio runtime")?;
    let (tx, rx) = mpsc::unbounded_channel();
    runtime.spawn(async move {
        let (code, error) = match process(opts, rx, &out, via_desktop).await {
            Ok(result) => result,
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let _ = out.send(AgentOutput::Exited { code, error });
    });
    Ok(tx)
}

pub(crate) fn command(opts: &SpawnOptions) -> anyhow::Result<Command> {
    command_with(opts, None)
}

/// `provider`: the model provider to use instead of the one `config.toml`
/// names, for a thread started with it.
fn command_with(opts: &SpawnOptions, provider: Option<&str>) -> anyhow::Result<Command> {
    let cwd = std::env::current_dir()?;
    let path = opts
        .env
        .iter()
        .rev()
        .find(|(k, _)| {
            if cfg!(windows) {
                k.eq_ignore_ascii_case("PATH")
            } else {
                k == "PATH"
            }
        })
        .map(|(_, v)| v.into())
        .or_else(|| std::env::var_os("PATH"));
    let program = which::which_in(
        opts.program
            .as_deref()
            .unwrap_or_else(|| Path::new(COMMAND)),
        path,
        &cwd,
    )
    .context("find codex executable")?;
    let mut cmd = Command::new(program);
    if let Some(provider) = provider {
        cmd.arg("-c").arg(format!("model_provider=\"{provider}\""));
    }
    cmd.arg("app-server")
        .args(&opts.args)
        .envs(opts.env.iter().cloned());
    if !opts.cwd.as_os_str().is_empty() {
        cmd.current_dir(&opts.cwd);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // std::process resolves .cmd/.bat through cmd.exe on Windows; which_in
    // handles PATHEXT and explicit .exe overrides before changing the cwd.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    Ok(cmd)
}

/// `thread/start`, `thread/resume` or `thread/fork`, as `opts` asks.
pub(crate) fn thread_request(opts: &SpawnOptions) -> anyhow::Result<(&'static str, Value)> {
    // A chat may read; anything else waits for the phone.
    let sandbox = if opts.chat {
        "read-only"
    } else {
        "workspace-write"
    };
    let mut params = json!({"approvalPolicy":"on-request","sandbox":sandbox});
    // A profile chosen on the phone replaces the sandbox (the two do not combine).
    if let Some(permission) = &opts.permission
        && let Some(params) = params.as_object_mut()
    {
        params.remove("sandbox");
        params.insert("permissions".into(), json!(permission));
    }
    if !opts.cwd.as_os_str().is_empty() {
        params["cwd"] = serde_json::to_value(&opts.cwd).context("encode codex cwd")?;
    }
    if opts.roots.len() > 1 {
        params["runtimeWorkspaceRoots"] =
            serde_json::to_value(&opts.roots).context("encode codex roots")?;
    }
    if let Some(speed) = &opts.speed {
        params["serviceTier"] = json!(speed);
    }
    let method = if let Some(resume) = &opts.resume {
        params["threadId"] = json!(resume);
        if opts.fork {
            // A copy has no project of its own to widen into.
            if let Some(params) = params.as_object_mut() {
                params.remove("runtimeWorkspaceRoots");
            }
            if let Some(model) = &opts.model {
                params["model"] = json!(model);
            }
            "thread/fork"
        } else {
            "thread/resume"
        }
    } else {
        if let Some(model) = &opts.model {
            params["model"] = json!(model);
        }
        if let Some(project) = &opts.project {
            params["projectId"] = json!(project);
        }
        "thread/start"
    };
    Ok((method, params))
}

fn record_dir(opts: &SpawnOptions) -> Option<PathBuf> {
    opts.env
        .iter()
        .rev()
        .find(|(key, _)| key == "TETHER_RECORD_DIR")
        .map(|(_, value)| PathBuf::from(value))
        .or_else(|| std::env::var_os("TETHER_RECORD_DIR").map(PathBuf::from))
        .filter(|p| !p.as_os_str().is_empty())
}

async fn process(
    opts: SpawnOptions,
    rx: UnboundedReceiver<AgentCommand>,
    out: &UnboundedSender<AgentOutput>,
    via_desktop: fn() -> bool,
) -> anyhow::Result<(Option<i32>, Option<String>)> {
    process_with(opts, rx, out, via_desktop, crate::bus::connect).await
}

/// Inject only the connection boundary; tests never touch a user's desktop bus.
async fn process_with<F>(
    mut opts: SpawnOptions,
    rx: UnboundedReceiver<AgentCommand>,
    out: &UnboundedSender<AgentOutput>,
    via_desktop: fn() -> bool,
    connect: impl FnOnce() -> F,
) -> anyhow::Result<(Option<i32>, Option<String>)>
where
    F: std::future::Future<Output = anyhow::Result<(crate::bus::Reader, crate::bus::Writer)>>,
{
    // Older phones default to reveal=true for new threads/forks. Automatic
    // routing must never navigate the desktop, even on the direct path. The
    // explicit AgentAdapter::reveal operation is unchanged.
    opts.reveal = false;
    if via_desktop() {
        // The user keeps the Codex app open; without it, say so instead of
        // quietly running elsewhere.
        let (reader, writer) = match connect().await {
            Ok(connection) => connection,
            Err(error) => {
                tracing::debug!(%error, "Codex desktop not reachable");
                return Ok((None, Some(crate::bus::NOT_OPEN_TEXT.into())));
            }
        };
        if let Some(thread) = desktop_thread(&opts) {
            match crate::follow::owned(reader, writer, thread).await {
                Ok(Some(link)) => {
                    let error = crate::follow::run(link, thread, &opts, rx, out)
                        .await
                        .err()
                        .map(|error| error.to_string());
                    return Ok((None, error));
                }
                Ok(None) => {
                    tracing::debug!(thread, "Codex desktop has no owner; using app-server")
                }
                Err(error) => {
                    tracing::debug!(
                        thread,
                        error = format!("{error:#}"),
                        "Codex desktop probe failed"
                    );
                    let text = if error.downcast_ref::<crate::bus::Closed>().is_some() {
                        crate::bus::NOT_OPEN_TEXT
                    } else if error.downcast_ref::<crate::bus::Updated>().is_some() {
                        crate::bus::UPDATED_TEXT
                    } else {
                        crate::bus::NO_ANSWER_TEXT
                    };
                    return Ok((None, Some(text.into())));
                }
            }
        }
    }
    let (mut recording, mut stderr_record) = match record_dir(&opts) {
        Some(dir) => {
            let (record, stderr) = Recording::open(&dir).await?;
            (Some(record), Some(stderr))
        }
        None => (None, None),
    };
    let mut rx = rx;
    let mut queued = VecDeque::new();
    let mut provider: Option<String> = None;
    loop {
        let child = command_with(&opts, provider.as_deref())?
            .spawn()
            .context("start codex app-server")?;
        let inbox = Inbox {
            commands: &mut rx,
            queued: std::mem::take(&mut queued),
        };
        let error = match drive_child(child, &opts, inbox, out, recording, stderr_record).await {
            Ok(done) => return Ok(done),
            Err(error) => error,
        };
        let mismatch = match error.downcast::<ProviderMismatch>() {
            Ok(mismatch) => mismatch,
            Err(error) => return Err(error),
        };
        // Once: with the thread's own provider there is nothing left to switch.
        if provider.is_some() || !provider_name(&mismatch.provider) {
            return Ok((None, Some(mismatch.to_string())));
        }
        emit(
            out,
            translate::notice(
                NoticeLevel::Info,
                format!(
                    "这个对话是用 {} 开始的，电脑上的 Codex 现在用的是 {}。Codex 只能用原来的 {} 继续它，已自动切换；想用现在的设置，请新建对话。",
                    mismatch.provider, mismatch.current, mismatch.provider
                ),
            ),
        );
        provider = Some(mismatch.provider);
        queued = mismatch.queued;
        (recording, stderr_record) = match record_dir(&opts) {
            Some(dir) => {
                let (record, stderr) = Recording::open(&dir).await?;
                (Some(record), Some(stderr))
            }
            None => (None, None),
        };
    }
}

/// A name `-c model_provider="…"` can carry as it is.
fn provider_name(provider: &str) -> bool {
    !provider.is_empty()
        && provider
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// What the phone asks for, after what it asked before a restart.
pub(crate) struct Inbox<'a> {
    pub(crate) commands: &'a mut UnboundedReceiver<AgentCommand>,
    pub(crate) queued: VecDeque<(String, Vec<String>)>,
}

#[cfg(test)]
impl<'a> Inbox<'a> {
    pub(crate) fn new(commands: &'a mut UnboundedReceiver<AgentCommand>) -> Self {
        Self {
            commands,
            queued: VecDeque::new(),
        }
    }
}

/// The thread was started with another model provider than `config.toml`
/// names now (a ChatGPT sign-in, a relay): Codex continues it only with that one.
#[derive(Debug)]
pub(crate) struct ProviderMismatch {
    pub(crate) provider: String,
    pub(crate) current: String,
    queued: VecDeque<(String, Vec<String>)>,
}

impl std::fmt::Display for ProviderMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "这个对话是用 {} 开始的，电脑上的 Codex 现在用的是 {}，没法接着用。请新建对话。",
            self.provider, self.current
        )
    }
}

impl std::error::Error for ProviderMismatch {}

/// `thread/resume`'s `modelProviderMismatch`: the thread's provider and the current one.
fn mismatch(error: &Value) -> Option<(String, String)> {
    let data = &error["data"];
    if data["type"] != "modelProviderMismatch" {
        return None;
    }
    Some((
        data["modelProvider"].as_str()?.to_owned(),
        data["currentModelProvider"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    ))
}

/// Only an existing thread can already be open in the app; a copy is new.
fn desktop_thread(opts: &SpawnOptions) -> Option<&str> {
    opts.resume.as_deref().filter(|_| !opts.fork)
}

async fn drain_stderr<R: AsyncRead + Unpin>(
    mut stderr: R,
    mut recording: Option<tokio::fs::File>,
) -> anyhow::Result<String> {
    let mut tail = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let n = stderr.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if let Some(file) = &mut recording {
            file.write_all(&buf[..n]).await?;
            file.flush().await?;
        }
        tail.extend_from_slice(&buf[..n]);
        if tail.len() > tether_proto::MAX_TOOL_OUTPUT {
            tail.drain(..tail.len() - tether_proto::MAX_TOOL_OUTPUT);
        }
        tracing::debug!(bytes = n, "codex stderr");
    }
    Ok(translate::tail(&String::from_utf8_lossy(&tail)))
}

async fn kill_tree(child: &mut Child) {
    // Killing cmd.exe alone can leave npm's node/native child alive. First let
    // stdin EOF close the server gracefully; on timeout terminate its whole tree.
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let mut kill = Command::new("taskkill.exe");
        kill.args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let _ = tokio::time::timeout(EXIT_GRACE, kill.status()).await;
    }
    let _ = child.start_kill();
}

async fn drive_child(
    mut child: Child,
    opts: &SpawnOptions,
    rx: Inbox<'_>,
    out: &UnboundedSender<AgentOutput>,
    recording: Option<Recording>,
    stderr_record: Option<tokio::fs::File>,
) -> anyhow::Result<(Option<i32>, Option<String>)> {
    let input = child.stdin.take().context("missing codex stdin")?;
    let output = child.stdout.take().context("missing codex stdout")?;
    let stderr = child.stderr.take().context("missing codex stderr")?;
    let mut stderr_task = tokio::spawn(drain_stderr(stderr, stderr_record));
    // run_stream owns stdin. Every exit path drops it before waiting on child.
    let result = run_stream(
        BufReader::new(output),
        input,
        opts,
        rx,
        out,
        recording,
        REQUEST_TIMEOUT,
    )
    .await;
    let requested = matches!(result, Ok(Stop::Shutdown));
    let status = match tokio::time::timeout(EXIT_GRACE, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            kill_tree(&mut child).await;
            child.wait().await
        }
    };
    let stderr = match tokio::time::timeout(EXIT_GRACE, &mut stderr_task).await {
        Ok(Ok(Ok(tail))) => tail,
        Ok(Ok(Err(e))) => format!("stderr IO: {e}"),
        Ok(Err(e)) => format!("stderr task: {e}"),
        Err(_) => {
            stderr_task.abort();
            "stderr stream did not close".into()
        }
    };
    let status = status.context("wait for codex app-server")?;
    let error = match result {
        // The caller starts it again with the thread's provider.
        Err(e) if e.is::<ProviderMismatch>() => return Err(e),
        // Said in full; Codex's log lines add nothing.
        Err(e) if e.is::<ThreadBusy>() => Some(e.to_string()),
        Err(e) => Some(format!(
            "{e:#}{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!("\n{stderr}")
            }
        )),
        Ok(_) if !status.success() && !requested => {
            Some(format!("codex app-server exited with {status}: {stderr}"))
        }
        _ => None,
    };
    Ok((status.code(), error))
}

/// Another app-server has the thread loaded (`thread-writer-locks`), most
/// often the Codex app's: only one may continue it.
#[derive(Debug)]
pub(crate) struct ThreadBusy;

impl std::fmt::Display for ThreadBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(BUSY_TEXT)
    }
}

impl std::error::Error for ThreadBusy {}

/// The phone offers a fork when it sees "分叉成新对话".
pub(crate) const BUSY_TEXT: &str = "电脑上的 Codex 桌面版（或另一个程序）正开着这个对话，同一个对话同一时间只能在一边继续。可以分叉成新对话继续，或者在电脑上退出 Codex 桌面版后再发。";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Shutdown,
    Eof,
}

enum Request {
    Initialize,
    Thread,
    Start(u64),
    Steer,
    Interrupt,
}

struct ApprovalWait {
    rpc_id: Id,
    turn_id: String,
}

#[derive(Default)]
struct Session {
    /// For `turn/start`; `None` leaves the thread's own.
    model: Option<String>,
    effort: Option<String>,
    /// `serviceTier`: a tier id or "default".
    speed: Option<String>,
    /// `permissions`: a profile id.
    permission: Option<String>,
    /// Show the thread in the desktop app once its first turn has started.
    reveal: bool,
    /// List a new thread in the desktop app's sidebar once its first turn has
    /// started, without opening it.
    announce: bool,
    thread: Option<String>,
    ready: bool,
    turn: Option<String>,
    starting: Option<u64>,
    generation: u64,
    finished: VecDeque<String>,
    /// Messages not sent yet: text and pictures.
    queue: VecDeque<(String, Vec<String>)>,
    interrupt_pending: bool,
    items: HashMap<String, Value>,
    approvals: HashMap<String, ApprovalWait>,
}

fn emit(out: &UnboundedSender<AgentOutput>, event: EventKind) {
    let _ = out.send(AgentOutput::Event(event));
}

async fn run_stream<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(
    reader: R,
    writer: W,
    opts: &SpawnOptions,
    inbox: Inbox<'_>,
    out: &UnboundedSender<AgentOutput>,
    recording: Option<Recording>,
    timeout: Duration,
) -> anyhow::Result<Stop> {
    let mut rpc = Rpc::new(writer, recording, timeout);
    // Projects (`projectId`) are still experimental in app-server.
    rpc.request("initialize", json!({"clientInfo":{"name":"tether","title":"Tether","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}), Request::Initialize).await?;
    let mut session = Session {
        thread: opts.resume.clone().filter(|_| !opts.fork),
        model: opts.model.clone(),
        effort: opts.effort.clone(),
        speed: opts.speed.clone(),
        permission: opts.permission.clone(),
        reveal: opts.reveal && (opts.resume.is_none() || opts.fork),
        announce: opts.resume.is_none() || opts.fork,
        queue: inbox.queued,
        ..Session::default()
    };
    let commands = inbox.commands;
    let mut lines = reader.lines();
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(AgentCommand::Shutdown) | None => return Ok(Stop::Shutdown),
                Some(AgentCommand::Send { text, images }) => {
                    emit(out, translate::state(SessionState::Working));
                    session.queue.push_back((text, images));
                    session.flush(&mut rpc).await?;
                }
                Some(AgentCommand::Answer { approval, decision }) => {
                    if let Some(wait) = session.approvals.remove(&approval) {
                        rpc.send(json!({"id":wait.rpc_id,"result":{"decision":translate::decision(decision)}})).await?;
                        emit(out, EventKind::ApprovalDone { approval, decision });
                        emit(out, translate::state(SessionState::Working));
                        if !session.approvals.is_empty() { emit(out, translate::state(SessionState::NeedsYou)); }
                    } else { emit(out, translate::notice(NoticeLevel::Warn, "审批已结束或不存在")); }
                }
                Some(AgentCommand::SetModel { model, effort, speed, permission }) => {
                    if permission.is_some() {
                        session.permission = permission;
                    }
                    if model.is_some() {
                        session.model = model;
                    }
                    if effort.is_some() {
                        session.effort = effort;
                    }
                    if speed.is_some() {
                        session.speed = speed;
                    }
                    emit(out, translate::notice(NoticeLevel::Info, format!(
                        "下一条消息起使用 {}（思考强度：{}，速度：{}{}）",
                        session.model.as_deref().unwrap_or("默认模型"),
                        session.effort.as_deref().unwrap_or("默认"),
                        translate::speed_word(session.speed.as_deref()),
                        translate::permission_part(session.permission.as_deref()),
                    )));
                }
                Some(AgentCommand::Interrupt) => {
                    session.cancel_approvals(&mut rpc, out, None).await?;
                    if session.turn.is_some() || session.starting.is_some() || !session.queue.is_empty() {
                        session.interrupt_pending = true;
                        session.interrupt(&mut rpc).await?;
                    }
                }
            },
            line = lines.next_line() => {
                let Some(line) = line.context("read codex stdout")? else {
                    ensure!(session.ready, "codex stdout closed before thread initialization");
                    ensure!(session.turn.is_none() && session.starting.is_none(), "codex stdout closed during a turn");
                    return Ok(Stop::Eof);
                };
                ensure!(line.len() <= 16 << 20, "codex line exceeds 16 MiB");
                rpc.record_incoming(&line).await?;
                match crate::protocol::decode(&line) {
                    Ok(Message::Response { id, result }) => {
                        if let Some(pending) = rpc.resolve(&id) {
                            match result {
                                Ok(result) => session.response(pending.context, &result, opts, &mut rpc, out).await?,
                                Err(error) => {
                                    if matches!(pending.context, Request::Thread) && let Some((provider, current)) = mismatch(&error) {
                                        bail!(ProviderMismatch { provider, current, queued: std::mem::take(&mut session.queue) });
                                    }
                                    session.failure(pending.context, &format!("{}: {}", pending.method, translate::error_text(&error)), out)?
                                }
                            }
                        } else { emit(out, translate::notice(NoticeLevel::Warn, "忽略未知或已过期的 Codex 响应")); }
                    }
                    Ok(Message::Notification { method, params }) => session.notification(&method, &params, &mut rpc, out).await?,
                    Ok(Message::Request { id, method, params }) => session.request(id, &method, &params, &mut rpc, out).await?,
                    Err(error) => emit(out, translate::notice(NoticeLevel::Warn, format!("忽略无效的 Codex 消息：{error}"))),
                }
                session.flush(&mut rpc).await?;
            }
            _ = tick.tick() => {
                for pending in rpc.expired(Instant::now()) {
                    session.failure(pending.context, &format!("{}: response timed out", pending.method), out)?;
                }
                session.flush(&mut rpc).await?;
            }
            _ = out.closed() => return Ok(Stop::Shutdown),
        }
    }
}

impl Session {
    async fn flush<W: AsyncWrite + Unpin>(
        &mut self,
        rpc: &mut Rpc<W, Request>,
    ) -> anyhow::Result<()> {
        if !self.ready {
            return Ok(());
        }
        let thread = self.thread.as_deref().context("ready thread has no id")?;
        while let Some((text, images)) = self.queue.front() {
            let input = translate::input(text, images);
            if let Some(turn) = &self.turn {
                rpc.request(
                    "turn/steer",
                    json!({"threadId":thread,"expectedTurnId":turn,"input":input}),
                    Request::Steer,
                )
                .await?;
                self.queue.pop_front();
            } else if self.starting.is_none() {
                self.generation = self
                    .generation
                    .checked_add(1)
                    .context("turn generation exhausted")?;
                let mut params = json!({"threadId":thread,"input":input});
                if let Some(model) = &self.model {
                    params["model"] = json!(model);
                }
                if let Some(effort) = &self.effort {
                    params["effort"] = json!(effort);
                }
                if let Some(speed) = &self.speed {
                    params["serviceTier"] = json!(speed);
                }
                if let Some(permission) = &self.permission {
                    params["permissions"] = json!(permission);
                }
                rpc.request("turn/start", params, Request::Start(self.generation))
                    .await?;
                self.starting = Some(self.generation);
                self.queue.pop_front();
            } else {
                break;
            }
        }
        self.interrupt(rpc).await
    }

    async fn interrupt<W: AsyncWrite + Unpin>(
        &mut self,
        rpc: &mut Rpc<W, Request>,
    ) -> anyhow::Result<()> {
        if self.interrupt_pending
            && let (Some(thread), Some(turn)) = (&self.thread, &self.turn)
        {
            rpc.request(
                "turn/interrupt",
                json!({"threadId":thread,"turnId":turn}),
                Request::Interrupt,
            )
            .await?;
            self.interrupt_pending = false;
        }
        Ok(())
    }

    async fn response<W: AsyncWrite + Unpin>(
        &mut self,
        context: Request,
        result: &Value,
        opts: &SpawnOptions,
        rpc: &mut Rpc<W, Request>,
        out: &UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<()> {
        match context {
            Request::Initialize => {
                rpc.send(json!({"method":"initialized"})).await?;
                let (method, params) = thread_request(opts)?;
                rpc.request(method, params, Request::Thread).await?;
            }
            Request::Thread => {
                let id = result["thread"]["id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .context("thread response missing thread.id")?;
                if let Some(expected) = opts.resume.as_ref().filter(|_| !opts.fork) {
                    ensure!(
                        expected == id,
                        "resumed thread id differs from requested id"
                    );
                }
                self.thread = Some(id.to_owned());
                self.ready = true;
                // Resuming a still-running server must steer, not start a second turn.
                self.turn = result["thread"]["turns"]
                    .as_array()
                    .and_then(|turns| turns.iter().rev().find(|t| t["status"] == "inProgress"))
                    .and_then(|t| t["id"].as_str())
                    .map(str::to_owned);
                let _ = out.send(AgentOutput::NativeId(id.to_owned()));
                if self.turn.is_some() {
                    emit(out, translate::state(SessionState::Working));
                }
            }
            Request::Start(generation) => {
                if self.starting == Some(generation) {
                    let id = result["turn"]["id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .context("turn/start response missing turn.id")?;
                    ensure!(
                        self.turn.as_deref().is_none_or(|active| active == id),
                        "turn/start response differs from turn/started"
                    );
                    self.starting = None;
                    if !self.finished.iter().any(|finished| finished == id) {
                        self.turn = Some(id.to_owned());
                    }
                    // Its first message is saved now, so the app can show it.
                    if std::mem::take(&mut self.reveal)
                        && let Some(thread) = &self.thread
                    {
                        crate::desktop::reveal_soon(thread.clone());
                    }
                    if std::mem::take(&mut self.announce)
                        && let Some(thread) = &self.thread
                    {
                        crate::desktop::announce_soon(thread.clone());
                    }
                }
            }
            Request::Steer | Request::Interrupt => {}
        }
        Ok(())
    }

    fn failure(
        &mut self,
        context: Request,
        message: &str,
        out: &UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<()> {
        match context {
            Request::Thread if message.contains("active writer") => bail!(ThreadBusy),
            Request::Initialize | Request::Thread => bail!("{message}"),
            Request::Start(generation) if self.starting == Some(generation) => {
                self.starting = None;
                emit(out, translate::notice(NoticeLevel::Error, message));
                // A started notification is authoritative if it raced the error.
                if self.turn.is_none() {
                    self.interrupt_pending = false;
                    emit(out, EventKind::TurnDone { summary: None });
                    emit(out, translate::state(SessionState::Idle));
                }
            }
            _ => emit(out, translate::notice(NoticeLevel::Error, message)),
        }
        Ok(())
    }

    async fn notification<W: AsyncWrite + Unpin>(
        &mut self,
        method: &str,
        params: &Value,
        rpc: &mut Rpc<W, Request>,
        out: &UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<()> {
        if self.thread.as_deref().is_none() || params["threadId"].as_str() != self.thread.as_deref()
        {
            return Ok(());
        }
        if let Some(turn) = params["turnId"].as_str()
            && (self.finished.iter().any(|id| id == turn)
                || self.turn.as_deref().is_some_and(|id| id != turn))
        {
            return Ok(());
        }
        if matches!(method, "turn/started" | "turn/completed") {
            let Some(turn) = params["turn"]["id"].as_str() else {
                emit(
                    out,
                    translate::notice(NoticeLevel::Warn, "Codex 回合通知缺少 turn.id"),
                );
                return Ok(());
            };
            if self.finished.iter().any(|id| id == turn) {
                return Ok(());
            }
            if self.turn.as_deref().is_some_and(|id| id != turn) {
                return Ok(());
            }
            if method == "turn/started" {
                self.turn = Some(turn.to_owned());
            } else {
                self.cancel_approvals(rpc, out, Some(turn)).await?;
                self.turn = None;
                self.starting = None;
                self.interrupt_pending = false;
                self.items.clear();
                self.finished.push_back(turn.to_owned());
                if self.finished.len() > 128 {
                    self.finished.pop_front();
                }
            }
        }
        if method == "item/started"
            && let Some(id) = params["item"]["id"].as_str()
            && matches!(
                params["item"]["type"].as_str(),
                Some("fileChange" | "commandExecution")
            )
        {
            self.items.insert(id.to_owned(), params["item"].clone());
        }
        if method == "serverRequest/resolved" {
            if let Ok(id) = serde_json::from_value::<Id>(params["requestId"].clone())
                && self.approvals.remove(&id.approval_id()).is_some()
            {
                emit(
                    out,
                    translate::notice(NoticeLevel::Info, "Codex 已结束此审批请求"),
                );
                if self.approvals.is_empty() {
                    emit(out, translate::state(SessionState::Working));
                }
            }
            return Ok(());
        }
        match translate::notification(method, params) {
            Ok(events) => {
                for event in events {
                    emit(out, event);
                }
            }
            Err(error) => emit(
                out,
                translate::notice(NoticeLevel::Warn, format!("无效的 Codex 通知：{error}")),
            ),
        }
        if method == "item/completed"
            && let Some(id) = params["item"]["id"].as_str()
        {
            self.items.remove(id);
        }
        Ok(())
    }

    async fn request<W: AsyncWrite + Unpin>(
        &mut self,
        id: Id,
        method: &str,
        params: &Value,
        rpc: &mut Rpc<W, Request>,
        out: &UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<()> {
        if let Some(thread) = params.get("threadId").and_then(Value::as_str)
            && Some(thread) != self.thread.as_deref()
        {
            return rpc
                .reject(&id, -32602, "request belongs to another thread")
                .await;
        }
        let approval_id = id.approval_id();
        if self.approvals.contains_key(&approval_id) {
            return rpc.reject(&id, -32600, "duplicate server request id").await;
        }
        let previous = params["itemId"].as_str().and_then(|id| self.items.get(id));
        match translate::request(method, params, approval_id.clone(), previous) {
            Ok(Some(approval)) => {
                if params["threadId"].as_str() != self.thread.as_deref() || !self.ready {
                    return rpc
                        .reject(&id, -32602, "approval missing this thread id")
                        .await;
                }
                let Some(turn_id) = params["turnId"].as_str().filter(|id| !id.is_empty()) else {
                    return rpc.reject(&id, -32602, "approval missing turnId").await;
                };
                if self.finished.iter().any(|id| id == turn_id)
                    || self.turn.as_deref().is_some_and(|id| id != turn_id)
                {
                    return rpc
                        .reject(&id, -32602, "approval belongs to an inactive turn")
                        .await;
                }
                self.approvals.insert(
                    approval_id,
                    ApprovalWait {
                        rpc_id: id,
                        turn_id: turn_id.to_owned(),
                    },
                );
                emit(out, EventKind::Approval { approval });
                emit(out, translate::state(SessionState::NeedsYou));
            }
            Ok(None) => {
                rpc.reject(&id, -32601, "手机端暂不支持此类请求").await?;
                emit(
                    out,
                    translate::notice(NoticeLevel::Warn, "手机端暂不支持此类请求"),
                );
            }
            Err(error) => {
                rpc.reject(&id, -32602, &error.to_string()).await?;
                emit(
                    out,
                    translate::notice(NoticeLevel::Warn, format!("无效的 Codex 审批：{error}")),
                );
            }
        }
        Ok(())
    }

    async fn cancel_approvals<W: AsyncWrite + Unpin>(
        &mut self,
        rpc: &mut Rpc<W, Request>,
        out: &UnboundedSender<AgentOutput>,
        turn: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut ids: Vec<_> = self
            .approvals
            .iter()
            .filter(|(_, wait)| turn.is_none_or(|id| wait.turn_id == id))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        for id in ids {
            if let Some(wait) = self.approvals.remove(&id) {
                rpc.send(json!({"id":wait.rpc_id,"result":{"decision":"cancel"}}))
                    .await?;
                emit(
                    out,
                    EventKind::ApprovalDone {
                        approval: id,
                        decision: Decision::Abort,
                    },
                );
                emit(out, translate::state(SessionState::Working));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;

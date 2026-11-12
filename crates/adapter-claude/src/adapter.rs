//! Process management is a thin wrapper around the same JSONL loop as replays.

use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tether_agent_api::{AgentCommand, AgentOutput, SpawnOptions};
use tether_proto::{EventKind, NoticeLevel, SessionState};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, BufReader},
    process::Command,
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
};

use crate::{
    COMMAND,
    protocol::{self, Message, Permission},
    translate::{self, Translator, notice, state},
};

const INIT_TIMEOUT: Duration = Duration::from_secs(30);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) fn spawn(
    opts: SpawnOptions,
    out: UnboundedSender<AgentOutput>,
) -> Result<UnboundedSender<AgentCommand>> {
    let runtime =
        tokio::runtime::Handle::try_current().context("Claude adapter needs a Tokio runtime")?;
    let (cmd, rx) = mpsc::unbounded_channel();
    runtime.spawn(async move {
        let (code, error) = match run_process(opts, rx, &out).await {
            Ok(exit) => (exit.code, exit.error),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let _ = out.send(AgentOutput::Exited { code, error });
    });
    Ok(cmd)
}

fn command(program: &Path, opts: &SpawnOptions) -> Command {
    let mut command = Command::new(program);
    command
        .args(protocol::arguments(opts.resume.as_deref(), &opts.args))
        .args(
            opts.model
                .iter()
                .flat_map(|m| ["--model".to_owned(), m.clone()]),
        )
        .current_dir(&opts.cwd)
        .envs(opts.env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, including GUI hosts.
    command
}

struct ProcessExit {
    code: Option<i32>,
    error: Option<String>,
}

// Abort the drain on every early return as well as on a timed-out inherited pipe.
struct StderrTask(tokio::task::JoinHandle<String>);
impl Drop for StderrTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn stderr_tail(mut reader: impl AsyncRead + Unpin) -> String {
    let mut tail = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => {
                tail.extend_from_slice(&buffer[..n]);
                if tail.len() > 8192 {
                    tail.drain(..tail.len() - 8192);
                }
            }
            Err(error) => {
                tracing::debug!(%error, "Claude stderr read failed");
                break;
            }
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

async fn run_process(
    opts: SpawnOptions,
    rx: UnboundedReceiver<AgentCommand>,
    out: &UnboundedSender<AgentOutput>,
) -> Result<ProcessExit> {
    // which handles .exe/.cmd on Windows; no shell interpolation of input text.
    let program = which::which(
        opts.program
            .as_deref()
            .unwrap_or_else(|| Path::new(COMMAND)),
    )
    .context("locate Claude executable")?;
    let mut child = command(&program, &opts)
        .spawn()
        .context("spawn Claude stream-json process")?;
    let reader = BufReader::new(child.stdout.take().context("Claude stdout not piped")?);
    let writer = child.stdin.take().context("Claude stdin not piped")?;
    let stderr = child.stderr.take().context("Claude stderr not piped")?;
    let mut stderr = StderrTask(tokio::spawn(stderr_tail(stderr)));
    let (io_result, status) = {
        let io = run_io(
            reader,
            writer,
            rx,
            out,
            Some(opts.cwd.to_string_lossy().into_owned()),
            INIT_TIMEOUT,
        );
        tokio::pin!(io);
        tokio::select! {
            result = &mut io => (result, None),
            status = child.wait() => {
                // Drain buffered final result/NativeId records even if wait won.
                let result = tokio::time::timeout(DRAIN_TIMEOUT, &mut io).await.unwrap_or(Ok(Stop::Eof));
                (result, Some(status))
            }
        }
    }; // Drop stdin before waiting for a process that closed stdout first.
    let shutdown = matches!(io_result, Ok(Stop::Shutdown));
    let mut error = io_result.err().map(|error| format!("{error:#}"));
    let status = if let Some(status) = status {
        status.context("wait for Claude")?
    } else if shutdown || error.is_some() {
        let _ = child.start_kill();
        child.wait().await.context("reap Claude")?
    } else {
        match tokio::time::timeout(DRAIN_TIMEOUT, child.wait()).await {
            Ok(status) => status.context("wait for Claude after EOF")?,
            Err(_) => {
                error = Some("Claude stdout closed but the process did not exit".into());
                let _ = child.start_kill();
                child.wait().await.context("reap Claude after EOF")?
            }
        }
    };
    let diagnostics = tokio::time::timeout(DRAIN_TIMEOUT, &mut stderr.0)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    if !shutdown && !status.success() && error.is_none() {
        error = Some(format!("Claude exited with {status}"));
    }
    if let Some(error) = &mut error
        && !diagnostics.trim().is_empty()
    {
        error.push_str("\nstderr: ");
        error.push_str(diagnostics.trim());
    }
    Ok(ProcessExit {
        code: status.code(),
        error,
    })
}

#[derive(Debug, PartialEq)]
enum Stop {
    Eof,
    Shutdown,
}

#[derive(Debug)]
enum Control {
    Initialize,
    Interrupt,
    SetModel,
}

enum Queued {
    User(String),
    Interrupt,
    SetModel(Option<String>),
}

fn enqueue(writer: &UnboundedSender<Value>, value: Value) -> Result<()> {
    writer.send(value).context("queue Claude JSONL")
}

struct Session {
    initialized: bool,
    next_request: u64,
    controls: HashMap<String, Control>,
    permissions: HashMap<String, Permission>,
    queued: VecDeque<Queued>,
    translator: Translator,
    cwd: Option<String>,
}

impl Session {
    fn new(cwd: Option<String>) -> Self {
        Self {
            initialized: false,
            next_request: 2,
            controls: HashMap::from([("req_1".into(), Control::Initialize)]),
            permissions: HashMap::new(),
            queued: VecDeque::new(),
            translator: Translator::default(),
            cwd,
        }
    }

    fn send_interrupt(&mut self, writer: &UnboundedSender<Value>) -> Result<()> {
        let id = format!("req_{}", self.next_request);
        self.next_request = self
            .next_request
            .checked_add(1)
            .context("control request counter overflow")?;
        enqueue(writer, protocol::interrupt(&id))?;
        self.controls.insert(id, Control::Interrupt);
        Ok(())
    }

    fn send_set_model(
        &mut self,
        model: Option<&str>,
        writer: &UnboundedSender<Value>,
    ) -> Result<()> {
        let id = format!("req_{}", self.next_request);
        self.next_request = self
            .next_request
            .checked_add(1)
            .context("control request counter overflow")?;
        enqueue(writer, protocol::set_model(&id, model))?;
        self.controls.insert(id, Control::SetModel);
        Ok(())
    }

    fn command(
        &mut self,
        command: AgentCommand,
        writer: &UnboundedSender<Value>,
        out: &UnboundedSender<AgentOutput>,
    ) -> Result<bool> {
        match command {
            AgentCommand::Send { text, images } => {
                let text = tether_agent_api::with_paths(&text, &images);
                let _ = out.send(state(SessionState::Working));
                if self.initialized {
                    enqueue(writer, protocol::user(&text))?;
                } else {
                    self.queued.push_back(Queued::User(text));
                }
            }
            AgentCommand::Interrupt => {
                if self.initialized {
                    self.send_interrupt(writer)?;
                } else {
                    self.queued.push_back(Queued::Interrupt);
                }
            }
            AgentCommand::Answer { approval, decision } => {
                let Some(permission) = self.permissions.get(&approval) else {
                    let _ = out.send(notice(
                        NoticeLevel::Warn,
                        format!("审批已失效或不存在：{approval}"),
                    ));
                    return Ok(false);
                };
                let reply = match permission.reply(decision) {
                    Ok(reply) => reply,
                    Err(error) => {
                        let _ = out.send(notice(NoticeLevel::Warn, error.to_string()));
                        return Ok(false); // Retain the request for a supported decision.
                    }
                };
                enqueue(writer, reply)?;
                self.permissions.remove(&approval);
                let _ = out.send(AgentOutput::Event(EventKind::ApprovalDone {
                    approval,
                    decision,
                }));
                let _ = out.send(state(SessionState::Working));
            }
            // Claude Code has no reasoning effort to set; the model switches mid-session.
            AgentCommand::SetModel { model, .. } => {
                if self.initialized {
                    self.send_set_model(model.as_deref(), writer)?;
                } else {
                    self.queued.push_back(Queued::SetModel(model));
                }
            }
            AgentCommand::Shutdown => return Ok(true),
        }
        Ok(false)
    }

    fn message(
        &mut self,
        message: Message,
        writer: &UnboundedSender<Value>,
        out: &UnboundedSender<AgentOutput>,
    ) -> Result<()> {
        match message {
            Message::ControlResponse { response } => {
                let Some(control) = self.controls.remove(&response.request_id) else {
                    let _ = out.send(notice(
                        NoticeLevel::Warn,
                        format!("忽略未知控制响应：{}", response.request_id),
                    ));
                    return Ok(());
                };
                if response.subtype != "success" {
                    let detail = response.error.as_deref().unwrap_or(&response.subtype);
                    match control {
                        Control::Initialize => bail!("Claude initialize failed: {detail}"),
                        Control::Interrupt => {
                            let _ = out.send(notice(
                                NoticeLevel::Error,
                                format!("Claude interrupt failed: {detail}"),
                            ));
                        }
                        Control::SetModel => {
                            let _ = out.send(notice(
                                NoticeLevel::Error,
                                format!("切换模型失败：{detail}"),
                            ));
                        }
                    }
                } else if matches!(control, Control::Initialize) {
                    self.initialized = true;
                    while let Some(command) = self.queued.pop_front() {
                        match command {
                            Queued::User(text) => enqueue(writer, protocol::user(&text))?,
                            Queued::Interrupt => self.send_interrupt(writer)?,
                            Queued::SetModel(model) => {
                                self.send_set_model(model.as_deref(), writer)?
                            }
                        }
                    }
                }
            }
            Message::ControlRequest {
                request_id,
                request,
            } => {
                if self.permissions.contains_key(&request_id) {
                    let _ = out.send(notice(
                        NoticeLevel::Warn,
                        format!("忽略重复审批：{request_id}"),
                    ));
                    return Ok(());
                }
                let permission = if request["subtype"] == "can_use_tool" {
                    Permission::parse(request_id.clone(), &request)
                } else {
                    Err(anyhow::anyhow!(
                        "unsupported Claude control request: {}",
                        request["subtype"]
                    ))
                };
                match permission {
                    Ok(permission) => {
                        let card = translate::approval(&permission, self.cwd.clone());
                        self.permissions.insert(request_id, permission);
                        let _ =
                            out.send(AgentOutput::Event(EventKind::Approval { approval: card }));
                        let _ = out.send(state(SessionState::NeedsYou));
                    }
                    Err(error) => {
                        let _ = out.send(notice(NoticeLevel::Warn, error.to_string()));
                        enqueue(
                            writer,
                            json!({"type":"control_response", "response":{
                            "subtype":"error", "request_id":request_id, "error":error.to_string()}}),
                        )?;
                    }
                }
            }
            Message::ControlCancelRequest { request_id } => {
                if self.permissions.remove(&request_id).is_some() {
                    let _ = out.send(notice(
                        NoticeLevel::Info,
                        format!("审批已取消：{request_id}"),
                    ));
                    if self.permissions.is_empty() {
                        let _ = out.send(state(SessionState::Working));
                    }
                }
            }
            message => {
                if matches!(message, Message::Result { .. }) {
                    self.permissions.clear();
                }
                for output in self.translator.translate(&message) {
                    let _ = out.send(output);
                }
            }
        }
        Ok(())
    }
}

async fn run_io(
    reader: impl AsyncBufRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
    mut commands: UnboundedReceiver<AgentCommand>,
    out: &UnboundedSender<AgentOutput>,
    cwd: Option<String>,
    init_timeout: Duration,
) -> Result<Stop> {
    let mut session = Session::new(cwd);
    let (wire, mut pending_writes) = mpsc::unbounded_channel();
    enqueue(&wire, protocol::initialize("req_1"))?;
    // Poll stdin writes alongside commands/stdout. A child that stops reading
    // stdin must not prevent Shutdown, sender-drop or approval cancellation.
    // One FIFO writer also prevents concurrent records from interleaving.
    let sink = async move {
        while let Some(value) = pending_writes.recv().await {
            protocol::write(&mut writer, &value).await?;
        }
        Ok::<(), anyhow::Error>(())
    };
    tokio::pin!(sink);
    let mut lines = reader.lines();
    let deadline = tokio::time::sleep(init_timeout);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            result = &mut sink => {
                result?;
                bail!("Claude stdin writer stopped");
            }
            _ = out.closed() => return Ok(Stop::Shutdown),
            _ = &mut deadline, if !session.initialized => bail!("Claude initialize timed out"),
            command = commands.recv() => {
                let Some(command) = command else { return Ok(Stop::Shutdown); };
                if session.command(command, &wire, out)? { return Ok(Stop::Shutdown); }
            }
            line = lines.next_line() => {
                let Some(line) = line.context("read Claude JSONL")? else {
                    if !session.initialized { bail!("Claude stdout closed before initialize completed"); }
                    return Ok(Stop::Eof);
                };
                if line.trim().is_empty() { continue; }
                match serde_json::from_str::<Message>(&line) {
                    Ok(message) => session.message(message, &wire, out)?,
                    Err(error) => { let _ = out.send(notice(NoticeLevel::Warn, format!("忽略无效 Claude JSONL：{error}"))); }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

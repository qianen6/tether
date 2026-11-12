//! Platform-specific process setup and reaping, not protocol interpretation.

use std::{ffi::OsString, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use tether_agent_api::{AgentCommand, AgentOutput, SpawnOptions};
use tether_proto::{EventKind, NoticeLevel, SessionState};
use tokio::{
    io::{AsyncRead, AsyncReadExt, BufReader},
    process::Command,
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
};

use crate::{
    AcpAgent, session,
    updates::{emit, tool_tail},
};

const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct ProcessConfig {
    program: PathBuf,
    args: Vec<String>,
    opts: SpawnOptions,
}

impl ProcessConfig {
    pub(crate) fn new(agent: &AcpAgent, opts: SpawnOptions) -> Result<Self> {
        let program = opts
            .program
            .clone()
            .or_else(|| agent.command.first().map(PathBuf::from))
            .context("ACP agent has no command")?;
        ensure!(
            !program.as_os_str().is_empty(),
            "ACP agent command is empty"
        );
        let args = agent
            .command
            .iter()
            .skip(1)
            .chain(opts.args.iter())
            .cloned()
            .collect();
        Ok(Self {
            program,
            args,
            opts,
        })
    }

    fn command(&self) -> Result<(Command, PathBuf)> {
        let cwd = if self.opts.cwd.as_os_str().is_empty() {
            std::env::current_dir().context("resolving ACP cwd")?
        } else {
            std::path::absolute(&self.opts.cwd).context("resolving ACP cwd")?
        };
        let path: Option<OsString> = self
            .opts
            .env
            .iter()
            .rev()
            .find(|(key, _)| {
                if cfg!(windows) {
                    key.eq_ignore_ascii_case("PATH")
                } else {
                    key == "PATH"
                }
            })
            .map(|(_, value)| OsString::from(value))
            .or_else(|| std::env::var_os("PATH"));
        let executable = which::which_in(&self.program, path, &cwd)
            .with_context(|| format!("ACP command not found: {}", self.program.display()))?;
        let mut command = Command::new(executable);
        command
            .args(&self.args)
            .envs(self.opts.env.iter().cloned())
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, including GUI hosts.
        Ok((command, cwd))
    }
}

pub(crate) async fn run(
    config: ProcessConfig,
    commands: UnboundedReceiver<AgentCommand>,
    out: UnboundedSender<AgentOutput>,
) {
    if let Err(error) = drive(config, commands, out.clone()).await {
        let text = format!("{error:#}");
        emit(
            &out,
            EventKind::Notice {
                level: NoticeLevel::Error,
                text: text.clone(),
            },
        );
        emit(
            &out,
            EventKind::State {
                state: SessionState::Error,
                status_line: None,
            },
        );
        let _ = out.send(AgentOutput::Exited {
            code: None,
            error: Some(text),
        });
    }
}

async fn drive(
    config: ProcessConfig,
    commands: UnboundedReceiver<AgentCommand>,
    out: UnboundedSender<AgentOutput>,
) -> Result<()> {
    let (mut command, cwd) = config.command()?;
    let mut child = command.spawn().context("starting ACP agent")?;
    let stdin = child.stdin.take().context("ACP child has no stdin")?;
    let stdout = child.stdout.take().context("ACP child has no stdout")?;
    let stderr = child.stderr.take().context("ACP child has no stderr")?;
    let mut stderr = tokio::spawn(drain_stderr(stderr));
    let mut connection = Box::pin(session::run(
        BufReader::new(stdout),
        stdin,
        cwd,
        config.opts.resume,
        commands,
        out.clone(),
    ));

    let (result, status) = tokio::select! {
        result = &mut connection => {
            // Closing stdin and killing on shutdown/error must not leave a
            // child (or an unconsumed wait handle) behind.
            drop(connection);
            if !matches!(result, Ok(session::End::Eof)) {
                let _ = child.start_kill();
            }
            let status = match tokio::time::timeout(DRAIN_TIMEOUT, child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            (result, status)
        }
        status = child.wait() => {
            // Drain buffered stdout before reporting Exited, including the
            // final prompt response sent just before a process exits.
            let result = tokio::time::timeout(DRAIN_TIMEOUT, &mut connection).await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("ACP stdout did not close after process exit")));
            drop(connection);
            (result, status)
        }
    };
    let tail = match tokio::time::timeout(DRAIN_TIMEOUT, &mut stderr).await {
        Ok(Ok(tail)) => tail,
        _ => {
            stderr.abort();
            String::new()
        }
    };
    let status = status.context("waiting for ACP agent")?;
    let shutdown = matches!(result, Ok(session::End::Shutdown));
    let mut error = result.err().map(|error| format!("{error:#}"));
    if !shutdown && !status.success() && error.is_none() {
        error = Some(format!("ACP agent exited with {status}"));
        emit(
            &out,
            EventKind::State {
                state: SessionState::Error,
                status_line: None,
            },
        );
    }
    if let Some(error) = &mut error
        && !tail.is_empty()
    {
        error.push_str("\nstderr: ");
        error.push_str(&tail);
    }
    let _ = out.send(AgentOutput::Exited {
        code: status.code(),
        error,
    });
    Ok(())
}

async fn drain_stderr<R: AsyncRead + Unpin>(mut stderr: R) -> String {
    let mut buffer = [0u8; 4096];
    let mut tail = String::new();
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => return tail,
            Ok(read) => {
                tail.push_str(&String::from_utf8_lossy(&buffer[..read]));
                tail = tool_tail(&tail);
            }
            Err(error) => {
                tracing::debug!(%error, "reading ACP stderr");
                return tail;
            }
        }
    }
}

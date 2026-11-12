//! A fake agent for tests: it echoes each message back as a streamed reply.
//!
//! - `"approve:<cmd>"` asks for approval to "run" `<cmd>` first and waits for
//!   the answer (`deny` → "已拒绝", `abort` → stops the turn).
//! - `"/exit"` ends the process normally; `"/crash"` ends it with code 1.
//! - Any other text is answered with `"echo: <text>"` in three deltas.

use tether_proto::{
    AgentInfo, AgentKind, Approval, ApprovalKind, Decision, EventKind, NoticeLevel, SessionState,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::{AgentAdapter, AgentCommand, AgentOutput, SpawnOptions};

#[derive(Debug, Default, Clone)]
pub struct EchoAdapter;

impl AgentAdapter for EchoAdapter {
    fn info(&self) -> AgentInfo {
        AgentInfo {
            id: "echo".into(),
            name: "Echo".into(),
            kind: AgentKind::Echo,
            available: true,
            detail: Some("测试用回显 agent".into()),
            chat: true,
            terminal_command: vec![],
        }
    }

    fn spawn(
        &self,
        opts: SpawnOptions,
        out: UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(opts, rx, out));
        Ok(tx)
    }
}

fn state(state: SessionState, status_line: Option<String>) -> AgentOutput {
    AgentOutput::Event(EventKind::State { state, status_line })
}

fn end_turn(out: &UnboundedSender<AgentOutput>) {
    let _ = out.send(AgentOutput::Event(EventKind::TurnDone { summary: None }));
    let _ = out.send(state(SessionState::Idle, None));
}

fn reply(out: &UnboundedSender<AgentOutput>, text: &str) {
    let item = uuid::Uuid::new_v4().to_string();
    let chars: Vec<char> = text.chars().collect();
    let step = chars.len().div_ceil(3).max(1);
    for piece in chars.chunks(step) {
        let _ = out.send(AgentOutput::Event(EventKind::AssistantDelta {
            item: item.clone(),
            text: piece.iter().collect(),
        }));
    }
    let _ = out.send(AgentOutput::Event(EventKind::AssistantText {
        item,
        text: text.to_string(),
    }));
}

async fn run(
    opts: SpawnOptions,
    mut rx: UnboundedReceiver<AgentCommand>,
    out: UnboundedSender<AgentOutput>,
) {
    let native = opts
        .resume
        .clone()
        .unwrap_or_else(|| format!("echo-{}", uuid::Uuid::new_v4()));
    let _ = out.send(AgentOutput::NativeId(native));
    let cwd = opts.cwd.display().to_string();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            AgentCommand::Send { text, images } => {
                let text = crate::with_paths(&text, &images);
                if text == "/exit" || text == "/crash" {
                    let code = if text == "/exit" { 0 } else { 1 };
                    let error = (code != 0).then(|| "crash requested".to_string());
                    let _ = out.send(AgentOutput::Exited {
                        code: Some(code),
                        error,
                    });
                    return;
                }
                let _ = out.send(state(SessionState::Working, None));
                match text.strip_prefix("approve:") {
                    Some(cmd) => {
                        if !approve(cmd, &cwd, &mut rx, &out).await {
                            return;
                        }
                    }
                    None => {
                        reply(&out, &format!("echo: {text}"));
                        end_turn(&out);
                    }
                }
            }
            // Says so, so tests can see the switch arrive.
            AgentCommand::SetModel { model, effort, .. } => {
                let _ = out.send(AgentOutput::Event(EventKind::Notice {
                    level: NoticeLevel::Info,
                    text: format!(
                        "model: {} effort: {}",
                        model.as_deref().unwrap_or("default"),
                        effort.as_deref().unwrap_or("default")
                    ),
                }));
            }
            // Nothing pending: answers and interrupts have nothing to act on.
            AgentCommand::Answer { .. } | AgentCommand::Interrupt => {}
            AgentCommand::Shutdown => break,
        }
    }
    let _ = out.send(AgentOutput::Exited {
        code: Some(0),
        error: None,
    });
}

/// One approval round. Returns false when the agent should exit.
async fn approve(
    cmd: &str,
    cwd: &str,
    rx: &mut UnboundedReceiver<AgentCommand>,
    out: &UnboundedSender<AgentOutput>,
) -> bool {
    let id = uuid::Uuid::new_v4().to_string();
    let title = format!("运行：{cmd}");
    let _ = out.send(AgentOutput::Event(EventKind::Approval {
        approval: Approval {
            id: id.clone(),
            kind: ApprovalKind::Command,
            title: title.clone(),
            detail: cmd.to_string(),
            cwd: Some(cwd.to_string()),
            choices: vec![
                Decision::AllowOnce,
                Decision::AllowSession,
                Decision::Deny,
                Decision::Abort,
            ],
        },
    }));
    let _ = out.send(state(SessionState::NeedsYou, Some(title)));

    loop {
        match rx.recv().await {
            Some(AgentCommand::Answer { approval, decision }) if approval == id => {
                let _ = out.send(AgentOutput::Event(EventKind::ApprovalDone {
                    approval,
                    decision,
                }));
                let _ = out.send(state(SessionState::Working, None));
                match decision {
                    Decision::AllowOnce | Decision::AllowSession => {
                        reply(out, &format!("已执行：{cmd}"))
                    }
                    Decision::Deny => reply(out, &format!("已拒绝：{cmd}")),
                    Decision::Abort => notice(out, "已停止"),
                }
                end_turn(out);
                return true;
            }
            Some(AgentCommand::Interrupt) => {
                notice(out, "已打断");
                end_turn(out);
                return true;
            }
            Some(AgentCommand::Shutdown) | None => {
                let _ = out.send(AgentOutput::Exited {
                    code: Some(0),
                    error: None,
                });
                return false;
            }
            // A message or a stale answer while waiting: ignored, as a real
            // agent would keep waiting for the pending approval.
            Some(_) => {}
        }
    }
}

fn notice(out: &UnboundedSender<AgentOutput>, text: &str) {
    let _ = out.send(AgentOutput::Event(EventKind::Notice {
        level: NoticeLevel::Info,
        text: text.into(),
    }));
}

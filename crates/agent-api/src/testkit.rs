//! Helpers for testing adapters without a real agent process.

use std::time::Duration;

use anyhow::{Context, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::{AgentAdapter, AgentCommand, AgentOutput, SpawnOptions};

/// Spawn `adapter`, send every command in `script` in order, and collect
/// outputs until `until` holds for everything collected so far.
pub async fn run(
    adapter: &dyn AgentAdapter,
    opts: SpawnOptions,
    script: Vec<AgentCommand>,
    until: impl Fn(&[AgentOutput]) -> bool,
    timeout: Duration,
) -> anyhow::Result<Vec<AgentOutput>> {
    let mut d = Driver::spawn(adapter, opts)?;
    for cmd in script {
        d.send(cmd)?;
    }
    d.wait_until(until, timeout).await?;
    Ok(d.outputs)
}

/// Step-by-step control of a spawned adapter, for scripts that depend on what
/// the agent said (e.g. answering an approval whose id is not known upfront).
pub struct Driver {
    pub cmd: UnboundedSender<AgentCommand>,
    pub rx: UnboundedReceiver<AgentOutput>,
    /// Everything received so far.
    pub outputs: Vec<AgentOutput>,
}

impl Driver {
    pub fn spawn(adapter: &dyn AgentAdapter, opts: SpawnOptions) -> anyhow::Result<Driver> {
        let (out, rx) = mpsc::unbounded_channel();
        let cmd = adapter.spawn(opts, out)?;
        Ok(Driver {
            cmd,
            rx,
            outputs: Vec::new(),
        })
    }

    pub fn send(&self, cmd: AgentCommand) -> anyhow::Result<()> {
        self.cmd
            .send(cmd)
            .ok()
            .context("adapter stopped taking commands")
    }

    /// Receive until `until(&outputs)` holds.
    pub async fn wait_until(
        &mut self,
        until: impl Fn(&[AgentOutput]) -> bool,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        while !until(&self.outputs) {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Some(o)) => self.outputs.push(o),
                Ok(None) => bail!("adapter output closed; got {:#?}", self.outputs),
                Err(_) => bail!("timed out after {timeout:?}; got {:#?}", self.outputs),
            }
        }
        Ok(())
    }

    /// Receive until an output matching `pick` arrives and return what it picked.
    pub async fn wait_for<T>(
        &mut self,
        pick: impl Fn(&AgentOutput) -> Option<T>,
        timeout: Duration,
    ) -> anyhow::Result<T> {
        let start = self.outputs.len();
        self.wait_until(|o| o[start..].iter().any(|x| pick(x).is_some()), timeout)
            .await?;
        Ok(self.outputs[start..]
            .iter()
            .find_map(pick)
            .expect("matched above"))
    }

    pub fn events(&self) -> Vec<&tether_proto::EventKind> {
        self.outputs.iter().filter_map(AgentOutput::event).collect()
    }
}

/// One end of an in-memory, line-oriented duplex stream. Give one end to the
/// adapter's protocol code (instead of a child's stdin/stdout) and play the
/// agent process with the other.
pub struct LineEnd {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
}

pub fn line_duplex() -> (LineEnd, LineEnd) {
    let (a, b) = tokio::io::duplex(1 << 20);
    (LineEnd::new(a), LineEnd::new(b))
}

impl LineEnd {
    fn new(s: DuplexStream) -> LineEnd {
        let (r, w) = tokio::io::split(s);
        LineEnd {
            reader: BufReader::new(r),
            writer: w,
        }
    }

    /// The raw halves, to hand to code that takes `AsyncBufRead` + `AsyncWrite`.
    pub fn into_parts(self) -> (BufReader<ReadHalf<DuplexStream>>, WriteHalf<DuplexStream>) {
        (self.reader, self.writer)
    }

    pub async fn send_line(&mut self, line: &str) -> anyhow::Result<()> {
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    pub async fn send_json(&mut self, v: &serde_json::Value) -> anyhow::Result<()> {
        self.send_line(&serde_json::to_string(v)?).await
    }

    /// The next line without its newline; `None` at end of stream.
    pub async fn next_line(&mut self) -> anyhow::Result<Option<String>> {
        let mut s = String::new();
        if self.reader.read_line(&mut s).await? == 0 {
            return Ok(None);
        }
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        Ok(Some(s))
    }

    pub async fn next_json(&mut self) -> anyhow::Result<Option<serde_json::Value>> {
        match self.next_line().await? {
            Some(l) => Ok(Some(
                serde_json::from_str(&l).with_context(|| format!("not JSON: {l}"))?,
            )),
            None => Ok(None),
        }
    }

    /// Replay a recorded JSONL file line by line (blank lines and `//` comments skipped).
    pub async fn send_fixture(&mut self, text: &str) -> anyhow::Result<()> {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            self.send_line(line).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EchoAdapter;
    use tether_proto::{Decision, EventKind, SessionState};

    const T: Duration = Duration::from_secs(5);

    fn is_idle(o: &AgentOutput) -> bool {
        matches!(
            o,
            AgentOutput::Event(EventKind::State {
                state: SessionState::Idle,
                ..
            })
        )
    }

    fn names(d: &Driver) -> Vec<&'static str> {
        d.events()
            .into_iter()
            .map(|e| match e {
                EventKind::UserText { .. } => "user_text",
                EventKind::AssistantDelta { .. } => "delta",
                EventKind::AssistantText { .. } => "text",
                EventKind::Thinking { .. } => "thinking",
                EventKind::ToolStart { .. } => "tool_start",
                EventKind::ToolEnd { .. } => "tool_end",
                EventKind::Approval { .. } => "approval",
                EventKind::ApprovalDone { .. } => "approval_done",
                EventKind::State {
                    state: SessionState::Idle,
                    ..
                } => "idle",
                EventKind::State {
                    state: SessionState::Working,
                    ..
                } => "working",
                EventKind::State {
                    state: SessionState::NeedsYou,
                    ..
                } => "needs_you",
                EventKind::State { .. } => "state",
                EventKind::TurnDone { .. } => "turn_done",
                EventKind::Notice { .. } => "notice",
            })
            .collect()
    }

    #[tokio::test]
    async fn echo_plain_turn_follows_the_rules() {
        let mut d = Driver::spawn(&EchoAdapter, SpawnOptions::default()).unwrap();
        d.send(AgentCommand::Send {
            text: "你好世界".into(),
            images: Vec::new(),
        })
        .unwrap();
        d.wait_until(|o| o.iter().any(is_idle), T).await.unwrap();

        assert!(matches!(d.outputs[0], AgentOutput::NativeId(_)));
        assert_eq!(
            names(&d),
            [
                "working",
                "delta",
                "delta",
                "delta",
                "text",
                "turn_done",
                "idle"
            ]
        );
        let deltas: String = d
            .events()
            .iter()
            .filter_map(|e| match e {
                EventKind::AssistantDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, "echo: 你好世界");
    }

    #[tokio::test]
    async fn echo_approval_turn_follows_the_rules() {
        let mut d = Driver::spawn(&EchoAdapter, SpawnOptions::default()).unwrap();
        d.send(AgentCommand::Send {
            text: "approve:npm test".into(),
            images: Vec::new(),
        })
        .unwrap();
        let (id, choices) = d
            .wait_for(
                |o| match o.event() {
                    Some(EventKind::Approval { approval }) => {
                        Some((approval.id.clone(), approval.choices.clone()))
                    }
                    _ => None,
                },
                T,
            )
            .await
            .unwrap();
        assert_eq!(choices.len(), 4);
        d.wait_until(
            |o| {
                matches!(
                    o.last().and_then(|x| x.event()),
                    Some(EventKind::State {
                        state: SessionState::NeedsYou,
                        ..
                    })
                )
            },
            T,
        )
        .await
        .unwrap();

        d.send(AgentCommand::Answer {
            approval: id,
            decision: Decision::AllowOnce,
        })
        .unwrap();
        d.wait_until(|o| o.iter().any(is_idle), T).await.unwrap();
        assert_eq!(
            names(&d),
            [
                "working",
                "approval",
                "needs_you",
                "approval_done",
                "working",
                "delta",
                "delta",
                "delta",
                "text",
                "turn_done",
                "idle"
            ]
        );
    }

    #[tokio::test]
    async fn echo_abort_and_interrupt_end_the_turn() {
        for finish in [Some(Decision::Abort), None] {
            let mut d = Driver::spawn(&EchoAdapter, SpawnOptions::default()).unwrap();
            d.send(AgentCommand::Send {
                text: "approve:rm -rf build".into(),
                images: Vec::new(),
            })
            .unwrap();
            let id = d
                .wait_for(
                    |o| match o.event() {
                        Some(EventKind::Approval { approval }) => Some(approval.id.clone()),
                        _ => None,
                    },
                    T,
                )
                .await
                .unwrap();
            match finish {
                Some(decision) => d
                    .send(AgentCommand::Answer {
                        approval: id,
                        decision,
                    })
                    .unwrap(),
                None => d.send(AgentCommand::Interrupt).unwrap(),
            }
            d.wait_until(|o| o.iter().any(is_idle), T).await.unwrap();
            assert!(names(&d).contains(&"notice"));
            assert!(!names(&d).contains(&"text"));
        }
    }

    #[tokio::test]
    async fn echo_resume_exit_and_shutdown() {
        let out = run(
            &EchoAdapter,
            SpawnOptions {
                resume: Some("echo-old".into()),
                ..Default::default()
            },
            vec![AgentCommand::Send {
                text: "/crash".into(),
                images: Vec::new(),
            }],
            |o| o.iter().any(|x| matches!(x, AgentOutput::Exited { .. })),
            T,
        )
        .await
        .unwrap();
        assert_eq!(out[0], AgentOutput::NativeId("echo-old".into()));
        assert_eq!(
            out.last(),
            Some(&AgentOutput::Exited {
                code: Some(1),
                error: Some("crash requested".into())
            })
        );

        let out = run(
            &EchoAdapter,
            SpawnOptions::default(),
            vec![AgentCommand::Shutdown],
            |o| o.iter().any(|x| matches!(x, AgentOutput::Exited { .. })),
            T,
        )
        .await
        .unwrap();
        assert_eq!(
            out.last(),
            Some(&AgentOutput::Exited {
                code: Some(0),
                error: None
            })
        );
    }

    #[tokio::test]
    async fn dropping_the_sender_shuts_down() {
        let mut d = Driver::spawn(&EchoAdapter, SpawnOptions::default()).unwrap();
        let (tx, _) = mpsc::unbounded_channel();
        drop(std::mem::replace(&mut d.cmd, tx));
        d.wait_until(
            |o| o.iter().any(|x| matches!(x, AgentOutput::Exited { .. })),
            T,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn line_duplex_carries_lines_and_fixtures() {
        let (mut a, mut b) = line_duplex();
        a.send_json(&serde_json::json!({"x": 1})).await.unwrap();
        a.send_fixture("// comment\n\n{\"y\":2}\r\n").await.unwrap();
        assert_eq!(b.next_json().await.unwrap().unwrap()["x"], 1);
        assert_eq!(b.next_line().await.unwrap().unwrap(), "{\"y\":2}");
        drop(a);
        assert!(b.next_line().await.unwrap().is_none());
    }
}

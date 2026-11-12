//! Claude Code adapter: drives `claude` in stream-json mode with its stdio
//! control protocol. Protocol and translation tests use in-memory streams only.

mod adapter;
mod protocol;
mod translate;

use std::sync::Arc;

use tether_agent_api::{AgentAdapter, AgentCommand, AgentOutput, SpawnOptions};
use tether_proto::{AgentInfo, AgentKind};
use tokio::sync::mpsc::UnboundedSender;

pub const ID: &str = "claude";
pub const COMMAND: &str = "claude";

#[derive(Debug, Default)]
pub struct ClaudeAdapter {
    _private: (),
}

pub fn adapter() -> Arc<dyn AgentAdapter> {
    Arc::new(ClaudeAdapter::default())
}

impl AgentAdapter for ClaudeAdapter {
    fn info(&self) -> AgentInfo {
        AgentInfo {
            id: ID.into(),
            // Anthropic's branding rules: not "Claude Code" inside our product.
            name: "Claude Agent".into(),
            kind: AgentKind::Claude,
            available: which::which(COMMAND).is_ok(),
            detail: None,
            chat: true,
            terminal_command: vec![COMMAND.into()],
        }
    }

    fn spawn(
        &self,
        opts: SpawnOptions,
        out: UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
        adapter::spawn(opts, out)
    }
}

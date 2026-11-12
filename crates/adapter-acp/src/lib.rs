//! ACP v1 over newline-delimited JSON-RPC on stdio.
//!
//! Process creation is a thin wrapper around the same duplex-stream client
//! exercised by the mock agent tests. No filesystem or terminal capabilities
//! are advertised. See `README.md` for protocol decisions and test fixtures.

use std::sync::Arc;

use tether_agent_api::{AgentAdapter, AgentCommand, AgentOutput, SpawnOptions};
use tether_proto::{AgentInfo, AgentKind};
use tokio::sync::mpsc::UnboundedSender;

mod process;
mod rpc;
mod session;
mod updates;

#[cfg(test)]
#[path = "../tests/mock_acp/mod.rs"]
mod tests;

/// One ACP agent: how to start it and what to call it.
#[derive(Debug, Clone, PartialEq)]
pub struct AcpAgent {
    pub id: String,
    pub name: String,
    /// Full command line, e.g. `["gemini", "--experimental-acp"]`.
    pub command: Vec<String>,
}

#[derive(Debug)]
pub struct AcpAdapter {
    agent: AcpAgent,
}

pub fn adapter(agent: AcpAgent) -> Arc<dyn AgentAdapter> {
    Arc::new(AcpAdapter { agent })
}

impl AgentAdapter for AcpAdapter {
    fn info(&self) -> AgentInfo {
        let program = self.agent.command.first().cloned().unwrap_or_default();
        AgentInfo {
            id: self.agent.id.clone(),
            name: self.agent.name.clone(),
            kind: AgentKind::Acp,
            available: which::which(&program).is_ok(),
            detail: None,
            chat: true,
            terminal_command: self.agent.command.clone(),
        }
    }

    fn spawn(
        &self,
        opts: SpawnOptions,
        out: UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
        let runtime = tokio::runtime::Handle::try_current()?;
        let config = process::ProcessConfig::new(&self.agent, opts)?;
        let (cmd, commands) = tokio::sync::mpsc::unbounded_channel();
        runtime.spawn(process::run(config, commands, out));
        Ok(cmd)
    }
}

//! Codex adapter: direct app-server JSON-RPC or an already loaded desktop thread.
//!
//! Wire shapes follow the vendored codex-cli 0.153.4 app-server schema.

mod adapter;
mod bus;
mod chats;
mod desktop;
mod follow;
mod history;
mod mirror;
mod protocol;
mod translate;

use std::{path::PathBuf, sync::Arc};

use tether_agent_api::{
    AgentAdapter, AgentCommand, AgentOutput, BoxFuture, HistoryQuery, HistoryRead, ModelQuery,
    Permissions, SpawnOptions,
};
use tether_proto::{AgentInfo, AgentKind, HistoryData, HistoryReadData, ModelInfo};
use tokio::sync::mpsc::UnboundedSender;

pub const ID: &str = "codex";
pub const COMMAND: &str = "codex";

#[derive(Debug)]
pub struct CodexAdapter {
    via_desktop: fn() -> bool,
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self {
            via_desktop: || false,
        }
    }
}

pub fn adapter() -> Arc<dyn AgentAdapter> {
    Arc::new(CodexAdapter::default())
}

/// Follow a desktop-owned thread when the predicate allows it at session start.
/// New threads, forks and threads without a desktop owner still run directly.
pub fn adapter_with(via_desktop: fn() -> bool) -> Arc<dyn AgentAdapter> {
    Arc::new(CodexAdapter { via_desktop })
}

impl AgentAdapter for CodexAdapter {
    fn info(&self) -> AgentInfo {
        AgentInfo {
            id: ID.into(),
            name: "Codex".into(),
            kind: AgentKind::Codex,
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
        adapter::spawn(opts, out, self.via_desktop)
    }

    fn list_history(&self, query: HistoryQuery) -> BoxFuture<HistoryData> {
        Box::pin(history::list(query))
    }

    fn read_history(&self, read: HistoryRead) -> BoxFuture<HistoryReadData> {
        Box::pin(history::read(read))
    }

    fn list_models(&self, query: ModelQuery) -> BoxFuture<Vec<ModelInfo>> {
        Box::pin(history::models(query))
    }

    fn list_permissions(&self, query: ModelQuery) -> BoxFuture<Permissions> {
        Box::pin(history::permissions(query))
    }

    /// `codex://threads/<id>`: the desktop app loads and lists it.
    fn reveal(&self, thread: &str) -> anyhow::Result<()> {
        desktop::reveal(thread)
    }

    /// A loaded thread is locked to its app-server (`thread-writer-locks`):
    /// let go as soon as a turn is over, so the Codex app can open it. The app
    /// does not try again by itself ("This is open in another app"), so a
    /// longer hold leaves it stuck there; starting again costs about a second.
    fn release_after(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(5))
    }

    /// Where the Codex app keeps its own chats, so they list together.
    fn chat_dir(&self, hint: &str) -> anyhow::Result<Option<PathBuf>> {
        let root = chats::root().ok_or_else(|| anyhow::anyhow!("找不到用户目录"))?;
        chats::create(&root, &chats::today(), hint).map(Some)
    }
}

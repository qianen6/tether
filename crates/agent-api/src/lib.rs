//! The seam between Tether's session manager and the agents it drives.
//!
//! An adapter turns one agent CLI (Codex, Claude Code, an ACP agent…) into a
//! stream of [`AgentOutput`]s and accepts [`AgentCommand`]s. Every adapter
//! follows the same rules (docs/CONTRACTS.md, "适配器行为约定"):
//!
//! 1. On `Send`, emit `state(working)` right away. Never emit `user_text`
//!    (the server does).
//! 2. After an `approval`, emit `state(needs_you)`; on its `Answer`, emit
//!    `approval_done`, then `state(working)` again.
//! 3. At the end of a turn emit `turn_done`, then `state(idle)`.
//! 4. When the process ends emit `Exited`. The server respawns with
//!    `resume = native_id` on the next message.
//! 5. Only offer `choices` the agent can honour.
//! 6. Cut `tool_end.output` with [`tether_proto::truncate_tail`] to
//!    [`tether_proto::MAX_TOOL_OUTPUT`].

use std::{future::Future, path::PathBuf, pin::Pin, time::Duration};

use tether_proto::{
    AgentInfo, Decision, EventKind, HistoryData, HistoryReadData, ModelInfo, PermissionInfo,
};
use tokio::sync::mpsc;

mod echo;
pub mod testkit;

pub use echo::EchoAdapter;

#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub cwd: PathBuf,
    /// The agent's own session id from an earlier [`AgentOutput::NativeId`].
    pub resume: Option<String>,
    /// Override the executable (tests, custom agents).
    pub program: Option<PathBuf>,
    /// Extra arguments (custom agents).
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// A conversation without a project: the agent picks a fresh folder when
    /// `cwd` is empty and may only read, asking before anything else.
    pub chat: bool,
    /// `None`: the agent's own default.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// A `SpeedInfo.id`, or "default" for standard speed.
    pub speed: Option<String>,
    /// A `PermissionInfo.id` from [`AgentAdapter::list_permissions`].
    /// `None`: as before there was a choice (see `chat`).
    pub permission: Option<String>,
    /// The agent's own project to file the conversation under (Codex
    /// `projectId`).
    pub project: Option<String>,
    /// Every folder of that project; `cwd` is one of them.
    pub roots: Vec<PathBuf>,
    /// Show the conversation in the agent's desktop app after its first
    /// message ([`AgentAdapter::reveal`]).
    pub reveal: bool,
    /// With `resume`: continue in a new copy of that conversation; the copy's
    /// id comes back as [`AgentOutput::NativeId`].
    pub fork: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentCommand {
    Send {
        text: String,
        /// Pictures on the computer sent with it.
        images: Vec<String>,
    },
    Answer {
        approval: String,
        decision: Decision,
    },
    Interrupt,
    /// For the next turn on. Adapters that cannot switch say so in a notice.
    SetModel {
        model: Option<String>,
        effort: Option<String>,
        speed: Option<String>,
        /// A `PermissionInfo.id`.
        permission: Option<String>,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentOutput {
    /// An event without a seq; the server numbers it.
    Event(EventKind),
    /// The agent's own session id, used to resume after a restart.
    NativeId(String),
    Exited {
        code: Option<i32>,
        error: Option<String>,
    },
}

/// A message with pictures, for an agent that takes text alone: the paths go
/// after the text, and the agent can open them itself.
pub fn with_paths(text: &str, images: &[String]) -> String {
    if images.is_empty() {
        return text.to_owned();
    }
    let paths: Vec<&str> = images.iter().map(String::as_str).collect();
    format!("{text}\n\n附图（电脑上的路径）：\n{}", paths.join("\n"))
}

impl AgentOutput {
    pub fn event(&self) -> Option<&EventKind> {
        match self {
            AgentOutput::Event(e) => Some(e),
            _ => None,
        }
    }
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send>>;

/// `list_history`: a page of the agent's saved conversations.
#[derive(Debug, Clone, Default)]
pub struct HistoryQuery {
    pub search: Option<String>,
    pub cursor: Option<String>,
    pub limit: u32,
    /// Override the executable, as in [`SpawnOptions::program`].
    pub program: Option<PathBuf>,
}

/// `read_history`: one saved conversation.
#[derive(Debug, Clone, Default)]
pub struct HistoryRead {
    pub thread: String,
    pub program: Option<PathBuf>,
}

/// `list_models`.
#[derive(Debug, Clone, Default)]
pub struct ModelQuery {
    pub program: Option<PathBuf>,
}

/// `list_permissions`: `ModelsData.permissions` and `default_permission`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Permissions {
    pub list: Vec<PermissionInfo>,
    pub default: Option<String>,
}

/// What an adapter without saved conversations answers; the server turns
/// it into `bad_request`.
#[derive(Debug)]
pub struct NoHistory;

impl std::fmt::Display for NoHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("这个 agent 没有可读取的历史对话")
    }
}

impl std::error::Error for NoHistory {}

pub trait AgentAdapter: Send + Sync + 'static {
    fn info(&self) -> AgentInfo;

    /// The agent's own saved conversations, newest first. Read only: nothing
    /// is resumed. Most agents have none.
    fn list_history(&self, _query: HistoryQuery) -> BoxFuture<HistoryData> {
        Box::pin(async { Err(NoHistory.into()) })
    }

    /// One saved conversation as it is on disk now, its turns as events.
    fn read_history(&self, _read: HistoryRead) -> BoxFuture<HistoryReadData> {
        Box::pin(async { Err(NoHistory.into()) })
    }

    /// Models to pick from. Empty: the phone takes a typed name, or none.
    fn list_models(&self, _query: ModelQuery) -> BoxFuture<Vec<ModelInfo>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// What the agent may be allowed to do, and which its own app uses now.
    /// Empty: no choice, the phone shows none.
    fn list_permissions(&self, _query: ModelQuery) -> BoxFuture<Permissions> {
        Box::pin(async { Ok(Permissions::default()) })
    }

    /// A fresh folder for a chat (`SpawnOptions::chat`), named after `hint`
    /// (its title). `None`: the server picks one.
    fn chat_dir(&self, _hint: &str) -> anyhow::Result<Option<PathBuf>> {
        Ok(None)
    }

    /// Open one of the agent's conversations in its desktop app, so it is
    /// listed there too. Most agents have no such app.
    fn reveal(&self, _thread: &str) -> anyhow::Result<()> {
        anyhow::bail!("这个 agent 没有电脑端的应用可以打开对话")
    }

    /// Stop an idle agent this long after its turn ends, so other programs
    /// may open the conversation; the next message resumes it. `None` keeps
    /// it running.
    fn release_after(&self) -> Option<Duration> {
        None
    }

    /// Must return at once: call it inside a tokio runtime and spawn the work.
    /// Dropping the returned sender is the same as `Shutdown`.
    fn spawn(
        &self,
        opts: SpawnOptions,
        out: mpsc::UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<mpsc::UnboundedSender<AgentCommand>>;
}

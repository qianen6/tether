//! Which agents this computer offers: built-ins, ACP presets, DeepSeek
//! Harness, `agents.toml`, and a plain shell for the terminal.

use std::{path::PathBuf, sync::Arc};

use tether_agent_api::{
    AgentAdapter, AgentCommand, AgentOutput, BoxFuture, HistoryQuery, HistoryRead, ModelQuery,
    NoHistory, Permissions, SpawnOptions,
};
use tether_proto::{AgentInfo, AgentKind, HistoryData, HistoryReadData, ModelInfo};
use tokio::sync::mpsc::UnboundedSender;

use crate::{CustomAgent, CustomMode, codex_desktop};

/// ACP presets. Commands come from each project's documentation; the ACP
/// repository (`.ref/agent-client-protocol`) lists the agents but not their
/// command lines, so all of these are unverified here.
const ACP_PRESETS: &[(&str, &str, &[&str])] = &[
    ("gemini", "Gemini CLI", &["gemini", "--experimental-acp"]),
    ("opencode", "OpenCode", &["opencode", "acp"]),
    ("kimi", "Kimi CLI", &["kimi", "--acp"]),
    ("qwen", "Qwen Code", &["qwen", "--experimental-acp"]),
    ("cursor", "Cursor", &["cursor-agent", "acp"]),
    ("copilot", "GitHub Copilot", &["copilot", "--acp"]),
];

/// Set to `1` to also offer the scripted echo agent (end-to-end tests).
pub(crate) const ECHO_ENV: &str = "TETHER_ENABLE_ECHO";

pub(crate) fn via_desktop() -> bool {
    std::env::var_os("TETHER_CODEX_DIRECT").is_none()
}

/// An adapter as listed on this computer: fills in `detail` when the command
/// is missing; terminal-only entries cannot chat.
struct Listed {
    inner: Option<Arc<dyn AgentAdapter>>,
    /// For terminal-only entries.
    info: AgentInfo,
    /// What the terminal runs instead of the adapter's own command: an ACP
    /// agent's interactive program rather than its `acp` server mode, which
    /// only answers JSON-RPC and sits silent in a terminal.
    terminal: Option<Vec<String>>,
}

impl AgentAdapter for Listed {
    fn info(&self) -> AgentInfo {
        let mut info = match &self.inner {
            Some(inner) => inner.info(),
            None => {
                let mut info = self.info.clone();
                info.available = self
                    .info
                    .terminal_command
                    .first()
                    .is_some_and(|c| which::which(c).is_ok());
                info
            }
        };
        if let Some(command) = &self.terminal {
            info.terminal_command = command.clone();
        }
        if !info.available && info.detail.is_none() {
            let program = info.terminal_command.first().cloned().unwrap_or_default();
            info.detail = Some(format!("未找到命令 {program}"));
        }
        info
    }

    fn spawn(
        &self,
        opts: SpawnOptions,
        out: UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
        match &self.inner {
            Some(inner) => inner.spawn(opts, out),
            None => anyhow::bail!("{} only runs in a terminal", self.info.name),
        }
    }

    fn list_history(&self, query: HistoryQuery) -> BoxFuture<HistoryData> {
        match &self.inner {
            Some(inner) => inner.list_history(query),
            None => Box::pin(async { Err(NoHistory.into()) }),
        }
    }

    fn read_history(&self, read: HistoryRead) -> BoxFuture<HistoryReadData> {
        match &self.inner {
            Some(inner) => inner.read_history(read),
            None => Box::pin(async { Err(NoHistory.into()) }),
        }
    }

    fn list_models(&self, query: ModelQuery) -> BoxFuture<Vec<ModelInfo>> {
        match &self.inner {
            Some(inner) => inner.list_models(query),
            None => Box::pin(async { Ok(Vec::new()) }),
        }
    }

    fn list_permissions(&self, query: ModelQuery) -> BoxFuture<Permissions> {
        match &self.inner {
            Some(inner) => inner.list_permissions(query),
            None => Box::pin(async { Ok(Permissions::default()) }),
        }
    }

    fn chat_dir(&self, hint: &str) -> anyhow::Result<Option<PathBuf>> {
        match &self.inner {
            Some(inner) => inner.chat_dir(hint),
            None => Ok(None),
        }
    }

    fn reveal(&self, thread: &str) -> anyhow::Result<()> {
        match &self.inner {
            Some(inner) => inner.reveal(thread),
            None => anyhow::bail!("这个 agent 没有电脑端的应用可以打开对话"),
        }
    }

    fn release_after(&self) -> Option<std::time::Duration> {
        self.inner.as_ref().and_then(|inner| inner.release_after())
    }
}

fn wrap(inner: Arc<dyn AgentAdapter>) -> Arc<dyn AgentAdapter> {
    let info = inner.info();
    Arc::new(Listed {
        inner: Some(inner),
        info,
        terminal: None,
    })
}

/// Another name for an adapter, where two of a kind sit side by side.
struct Named {
    inner: Arc<dyn AgentAdapter>,
    name: &'static str,
}

impl AgentAdapter for Named {
    fn info(&self) -> AgentInfo {
        AgentInfo {
            name: self.name.into(),
            ..self.inner.info()
        }
    }

    fn spawn(
        &self,
        opts: SpawnOptions,
        out: UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
        self.inner.spawn(opts, out)
    }

    fn list_history(&self, query: HistoryQuery) -> BoxFuture<HistoryData> {
        self.inner.list_history(query)
    }

    fn read_history(&self, read: HistoryRead) -> BoxFuture<HistoryReadData> {
        self.inner.read_history(read)
    }

    fn list_models(&self, query: ModelQuery) -> BoxFuture<Vec<ModelInfo>> {
        self.inner.list_models(query)
    }

    fn list_permissions(&self, query: ModelQuery) -> BoxFuture<Permissions> {
        self.inner.list_permissions(query)
    }

    fn chat_dir(&self, hint: &str) -> anyhow::Result<Option<PathBuf>> {
        self.inner.chat_dir(hint)
    }

    fn reveal(&self, thread: &str) -> anyhow::Result<()> {
        self.inner.reveal(thread)
    }

    fn release_after(&self) -> Option<std::time::Duration> {
        self.inner.release_after()
    }
}

fn terminal_only(id: &str, name: &str, command: Vec<String>) -> Arc<dyn AgentAdapter> {
    Arc::new(Listed {
        inner: None,
        terminal: None,
        info: AgentInfo {
            id: id.into(),
            name: name.into(),
            kind: AgentKind::Terminal,
            available: false,
            detail: None,
            chat: false,
            terminal_command: command,
        },
    })
}

fn dsh(id: &str, name: &str, command: Vec<String>) -> Arc<dyn AgentAdapter> {
    wrap(tether_adapter_dsh::adapter(tether_adapter_dsh::DshAgent {
        id: id.into(),
        name: name.into(),
        command,
        auto: false,
    }))
}

fn acp(id: &str, name: &str, command: Vec<String>) -> Arc<dyn AgentAdapter> {
    wrap(tether_adapter_acp::adapter(tether_adapter_acp::AcpAgent {
        id: id.into(),
        name: name.into(),
        command,
    }))
}

pub(crate) fn shell_command() -> Vec<String> {
    if cfg!(windows) {
        vec!["powershell.exe".into(), "-NoLogo".into()]
    } else {
        vec![
            std::env::var("SHELL")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/bin/sh".into()),
        ]
    }
}

/// Everything in display order. A custom agent replaces a built-in one with
/// the same id.
pub(crate) fn discover(custom: &[CustomAgent], echo: bool) -> Vec<Arc<dyn AgentAdapter>> {
    let mut list: Vec<Arc<dyn AgentAdapter>> = vec![
        // The `codex` on PATH and the desktop app's own: the phone picks one.
        wrap(Arc::new(Named {
            inner: tether_adapter_codex::adapter_with(via_desktop),
            name: "Codex 命令行",
        })),
        codex_desktop::adapter(),
        wrap(tether_adapter_claude::adapter()),
    ];
    for (id, name, command) in ACP_PRESETS {
        let inner = tether_adapter_acp::adapter(tether_adapter_acp::AcpAgent {
            id: id.to_string(),
            name: name.to_string(),
            command: command.iter().map(|s| s.to_string()).collect(),
        });
        // In a terminal each preset is its own program without the ACP switch.
        list.push(Arc::new(Listed {
            info: inner.info(),
            inner: Some(inner),
            terminal: Some(command[..1].iter().map(|s| s.to_string()).collect()),
        }));
    }
    list.push(wrap(tether_adapter_dsh::adapter(
        tether_adapter_dsh::DshAgent::default_agent(),
    )));
    list.push(terminal_only("shell", "Shell", shell_command()));
    if echo {
        list.push(Arc::new(tether_agent_api::EchoAdapter));
    }
    for agent in custom {
        let adapter = match agent.mode {
            CustomMode::Acp => acp(&agent.id, &agent.name, agent.command.clone()),
            CustomMode::Terminal => terminal_only(&agent.id, &agent.name, agent.command.clone()),
            CustomMode::Dsh => dsh(&agent.id, &agent.name, agent.command.clone()),
        };
        match list.iter().position(|a| a.info().id == agent.id) {
            Some(i) => {
                tracing::info!(id = %agent.id, "custom agent replaces a built-in one");
                list[i] = adapter;
            }
            None => list.push(adapter),
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[Arc<dyn AgentAdapter>]) -> Vec<String> {
        list.iter().map(|a| a.info().id).collect()
    }

    /// Offers a permission, as Codex does.
    struct Permitting;

    impl AgentAdapter for Permitting {
        fn info(&self) -> AgentInfo {
            tether_agent_api::EchoAdapter.info()
        }

        fn list_permissions(&self, _query: ModelQuery) -> BoxFuture<Permissions> {
            Box::pin(async {
                Ok(Permissions {
                    list: vec![tether_proto::PermissionInfo {
                        id: ":workspace".into(),
                        name: "默认权限".into(),
                        description: None,
                    }],
                    default: Some(":workspace".into()),
                })
            })
        }

        fn spawn(
            &self,
            opts: SpawnOptions,
            out: UnboundedSender<AgentOutput>,
        ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
            tether_agent_api::EchoAdapter.spawn(opts, out)
        }
    }

    #[tokio::test]
    async fn the_wrappers_pass_permissions_through() {
        let listed = wrap(Arc::new(Permitting));
        let named: Arc<dyn AgentAdapter> = Arc::new(Named {
            inner: listed.clone(),
            name: "另一个",
        });
        for adapter in [listed, named] {
            let got = adapter
                .list_permissions(ModelQuery::default())
                .await
                .unwrap();
            assert_eq!(got.default.as_deref(), Some(":workspace"));
            assert_eq!(got.list.len(), 1);
        }
    }

    #[test]
    fn built_ins_presets_and_shell() {
        let list = discover(&[], false);
        let ids = ids(&list);
        for id in [
            "codex",
            "codex-desktop",
            "claude",
            "gemini",
            "opencode",
            "cursor",
            "dsh",
            "shell",
        ] {
            assert!(ids.contains(&id.to_string()), "{id} missing");
        }
        assert!(!ids.contains(&"echo".to_string()));
        assert_eq!(list[0].info().name, "Codex 命令行");
        let desktop = list[1].info();
        assert_eq!(
            (desktop.id.as_str(), desktop.kind),
            ("codex-desktop", AgentKind::Codex)
        );
        let shell = list.iter().find(|a| a.info().id == "shell").unwrap().info();
        assert_eq!(shell.kind, AgentKind::Terminal);
        assert!(!shell.chat);
        assert_eq!(shell.terminal_command, shell_command());
        // A terminal runs Cursor itself, not its ACP server.
        let cursor = list
            .iter()
            .find(|a| a.info().id == "cursor")
            .unwrap()
            .info();
        assert_eq!(cursor.terminal_command, vec!["cursor-agent".to_string()]);
        let dsh = list.iter().find(|a| a.info().id == "dsh").unwrap().info();
        assert_eq!(dsh.kind, AgentKind::Dsh);
        assert!(dsh.chat);
        assert!(!dsh.terminal_command.is_empty());
    }

    #[test]
    fn custom_dsh_points_at_its_launcher() {
        let list = discover(
            &[CustomAgent {
                id: "dsh".into(),
                name: "My DSH".into(),
                command: vec!["tether-no-such-dsh-xyz".into()],
                mode: CustomMode::Dsh,
            }],
            false,
        );
        let ours: Vec<_> = list.iter().filter(|a| a.info().id == "dsh").collect();
        assert_eq!(ours.len(), 1, "the custom entry replaces the built-in one");
        let info = ours[0].info();
        assert_eq!((info.name.as_str(), info.kind), ("My DSH", AgentKind::Dsh));
        if std::env::var_os(tether_adapter_dsh::URL_ENV).is_none() {
            assert!(!info.available);
            assert!(info.detail.unwrap().contains("tether-no-such-dsh-xyz"));
        }
    }

    #[test]
    fn missing_commands_say_so() {
        let list = discover(
            &[CustomAgent {
                id: "ghost".into(),
                name: "Ghost".into(),
                command: vec!["tether-no-such-command-xyz".into(), "--acp".into()],
                mode: CustomMode::Acp,
            }],
            false,
        );
        let ghost = list.iter().find(|a| a.info().id == "ghost").unwrap().info();
        assert!(!ghost.available);
        assert_eq!(
            ghost.detail.as_deref(),
            Some("未找到命令 tether-no-such-command-xyz")
        );
        assert_eq!(ghost.kind, AgentKind::Acp);
        assert!(ghost.chat);
    }

    #[test]
    fn custom_terminal_and_override() {
        let list = discover(
            &[
                CustomAgent {
                    id: "codex".into(),
                    name: "My Codex".into(),
                    command: vec!["tether-no-such-command-xyz".into()],
                    mode: CustomMode::Terminal,
                },
                CustomAgent {
                    id: "top".into(),
                    name: "top".into(),
                    command: shell_command(),
                    mode: CustomMode::Terminal,
                },
            ],
            true,
        );
        let ids = ids(&list);
        assert_eq!(ids.iter().filter(|i| *i == "codex").count(), 1);
        assert_eq!(ids[0], "codex");
        let codex = list[0].info();
        assert_eq!(codex.name, "My Codex");
        assert!(!codex.chat);
        let top = list.iter().find(|a| a.info().id == "top").unwrap();
        assert!(top.info().available, "the shell exists");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(top.spawn(SpawnOptions::default(), tx).is_err());
        assert!(ids.contains(&"echo".to_string()));
    }
}

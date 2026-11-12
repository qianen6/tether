//! DeepSeek Harness (DSH) through its `dsh web` gateway.
//!
//! DSH has no stdio protocol (no ACP, no stream-json): its sessions live in a
//! `dsh web` process that clients drive over HTTP (`POST /api/<method>`) and
//! two WebSocket downlinks. This adapter shares one such process between all
//! DSH sessions ([`hub`]), turns each Tether session into one DSH session
//! ([`session`]) and its frames into Tether events ([`translate`]).
//!
//! Wire shapes were recorded from DSH 0.1.1-rc.2 (`fixtures/dsh/`); the
//! 0.2 gateway (cookie auth, `session/prompt` names) is not spoken here.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use serde_json::{Value, json};
use tether_agent_api::{
    AgentAdapter, AgentCommand, AgentOutput, BoxFuture, HistoryQuery, HistoryRead, ModelQuery,
    Permissions, SpawnOptions,
};
use tether_proto::{
    AgentInfo, AgentKind, Event, HistoryData, HistoryReadData, HistoryThread, ModelInfo,
    PermissionInfo,
};
use tokio::sync::mpsc::UnboundedSender;

mod api;
mod discover;
mod http;
mod hub;
mod job;
mod session;
mod setup;
mod terminal;
mod translate;

#[cfg(test)]
mod tests;

pub use api::RpcError;

/// Set to the `dsh` launcher when it is neither on `PATH` nor found in the
/// usual install folders.
pub const COMMAND_ENV: &str = "TETHER_DSH";
/// Set to a running `dsh web` (`http://127.0.0.1:3080`) to use only that one.
pub const URL_ENV: &str = hub::URL_ENV;

const ORIGIN_LABEL: &str = "DeepSeek Harness";
/// Messages read back for a saved conversation.
const HISTORY_MESSAGES: u64 = 400;

/// One DSH install: how to start it and what to call it.
#[derive(Debug, Clone, PartialEq)]
pub struct DshAgent {
    pub id: String,
    pub name: String,
    /// The `dsh` launcher and any launcher flags (`--profile web` is implied).
    pub command: Vec<String>,
}

impl DshAgent {
    /// The launcher named by `TETHER_DSH`, else `dsh` on `PATH`, else one
    /// found in the usual install folders (a portable `DeepSeekHarness`
    /// folder, the npm / pnpm global bins).
    pub fn default_agent() -> Self {
        let program = discover::locate(COMMAND_ENV).unwrap_or_else(|| "dsh".into());
        Self {
            id: "dsh".into(),
            name: "DeepSeek Harness".into(),
            command: vec![program],
        }
    }
}

pub struct DshAdapter {
    agent: DshAgent,
    hubs: Arc<hub::Hubs>,
}

pub fn adapter(agent: DshAgent) -> Arc<dyn AgentAdapter> {
    Arc::new(DshAdapter {
        agent,
        hubs: Arc::default(),
    })
}

impl DshAdapter {
    fn program(&self) -> &str {
        self.agent
            .command
            .first()
            .map(String::as_str)
            .unwrap_or("dsh")
    }

    fn resolved(&self) -> Option<PathBuf> {
        which::which(self.program()).ok()
    }

    async fn api(hubs: &hub::Hubs, command: &[String]) -> anyhow::Result<Arc<dyn api::Api>> {
        Ok(hubs.get(command).await?.api())
    }
}

impl AgentAdapter for DshAdapter {
    fn info(&self) -> AgentInfo {
        let resolved = self.resolved();
        let url = std::env::var(URL_ENV).is_ok_and(|u| !u.trim().is_empty());
        let available = resolved.is_some() || url;
        AgentInfo {
            id: self.agent.id.clone(),
            name: self.agent.name.clone(),
            kind: AgentKind::Dsh,
            available,
            detail: (!available).then(|| {
                format!(
                    "未找到 DSH（命令 {}）：PATH 和常见安装目录里都没有。把 dsh 加进 PATH，或设置环境变量 {COMMAND_ENV} 为它的完整路径",
                    self.program()
                )
            }),
            chat: true,
            terminal_command: terminal::command(self.program(), resolved.as_deref()),
        }
    }

    fn spawn(
        &self,
        opts: SpawnOptions,
        out: UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
        let runtime = tokio::runtime::Handle::try_current()?;
        let (tx, commands) = tokio::sync::mpsc::unbounded_channel();
        let hubs = self.hubs.clone();
        let command = launch_command(&self.agent.command, opts.program.as_ref());
        runtime.spawn(async move {
            match hubs.get(&command).await {
                Ok(hub) => {
                    let frames = hub.subscribe();
                    session::run(hub.api(), frames, opts, commands, out).await;
                }
                Err(error) => {
                    let _ = out.send(AgentOutput::Exited {
                        code: None,
                        error: Some(format!("无法连接 DSH：{error:#}")),
                    });
                }
            }
        });
        Ok(tx)
    }

    fn list_history(&self, query: HistoryQuery) -> BoxFuture<HistoryData> {
        let hubs = self.hubs.clone();
        let command = launch_command(&self.agent.command, query.program.as_ref());
        Box::pin(async move {
            let api = Self::api(&hubs, &command).await?;
            list_history(api.as_ref(), &query).await
        })
    }

    fn read_history(&self, read: HistoryRead) -> BoxFuture<HistoryReadData> {
        let hubs = self.hubs.clone();
        let command = launch_command(&self.agent.command, read.program.as_ref());
        Box::pin(async move {
            let api = Self::api(&hubs, &command).await?;
            read_history(api.as_ref(), &read.thread).await
        })
    }

    fn list_models(&self, query: ModelQuery) -> BoxFuture<Vec<ModelInfo>> {
        let hubs = self.hubs.clone();
        let command = launch_command(&self.agent.command, query.program.as_ref());
        Box::pin(async move {
            let hub = hubs.get(&command).await?;
            let current = hub::describe(&hub.origin).await.unwrap_or(Value::Null);
            let api = hub.api();
            let models = models_from(&api.call("llm.models", json!({})).await?, &current);
            let mut providers: Vec<String> = models
                .iter()
                .filter_map(|m| m.id.split_once('/').map(|(p, _)| p.to_owned()))
                .collect();
            providers.dedup();
            let missing = setup::missing_keys(api.as_ref(), &providers).await;
            setup::usable(models, &missing)
        })
    }

    fn list_permissions(&self, _query: ModelQuery) -> BoxFuture<Permissions> {
        Box::pin(async { Ok(permissions(terminal::dsh_home().as_deref())) })
    }
}

/// The configured launcher, or the phone's override of its program.
fn launch_command(command: &[String], program: Option<&PathBuf>) -> Vec<String> {
    let mut command = command.to_vec();
    if command.is_empty() {
        command.push("dsh".into());
    }
    if let Some(program) = program {
        command[0] = program.display().to_string();
    }
    command
}

/// DSH's permission presets (dsh-permission-presets), with the person's
/// default for new sessions from `settings.yaml` when set.
fn permissions(home: Option<&std::path::Path>) -> Permissions {
    let list = [
        ("read-only", "只读", "只能读文件；改动前要先问你"),
        (
            "workspace-write",
            "可改工作区",
            "可以改会话文件夹里的文件；越出沙箱前要问你",
        ),
        ("danger-full-access", "完全访问", "不用沙箱，也不再问你"),
    ]
    .into_iter()
    .map(|(id, name, description)| PermissionInfo {
        id: id.into(),
        name: name.into(),
        description: Some(description.into()),
    })
    .collect::<Vec<_>>();
    let configured = home
        .and_then(|h| std::fs::read_to_string(h.join("settings.yaml")).ok())
        .and_then(|text| default_preset(&text));
    let default = configured
        .filter(|d| list.iter().any(|p| &p.id == d))
        .unwrap_or_else(|| "workspace-write".into());
    Permissions {
        list,
        default: Some(default),
    }
}

/// `permissionPresets: { defaultPreset: X }` in DSH's `settings.yaml`.
fn default_preset(settings: &str) -> Option<String> {
    let mut inside = false;
    for line in settings.lines() {
        if !line.starts_with(' ') && !line.starts_with('\t') {
            inside = line.trim_end() == "permissionPresets:";
            continue;
        }
        if inside && let Some(value) = line.trim().strip_prefix("defaultPreset:") {
            let value = value.trim().trim_matches(['"', '\'']);
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn models_from(models: &Value, current: &Value) -> Vec<ModelInfo> {
    let current_provider = translate::str_at(current, &["provider"]);
    let current_model = translate::str_at(current, &["model"]);
    let mut list = Vec::new();
    for group in models
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let provider = translate::str_at(group, &["id"]).unwrap_or_default();
        let provider_name = translate::str_at(group, &["name"]).unwrap_or(provider);
        for model in group
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = translate::str_at(model, &["id"]) else {
                continue;
            };
            let efforts = model
                .get("reasoning")
                .and_then(|r| r.get("efforts"))
                .and_then(Value::as_array)
                .map(|e| {
                    e.iter()
                        .filter_map(|x| translate::str_at(x, &["id"]).map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            // The name fits the composer's pill; which provider it comes
            // from is the note under it in the menu.
            let description = match translate::str_at(model, &["description"]) {
                Some(text) => format!("{provider_name} · {text}"),
                None => provider_name.to_owned(),
            };
            list.push(ModelInfo {
                id: format!("{provider}/{id}"),
                name: translate::str_at(model, &["name"]).unwrap_or(id).to_owned(),
                description: Some(description),
                efforts,
                default_effort: translate::str_at(model, &["reasoning", "defaultEffort"])
                    .map(str::to_owned),
                is_default: current_provider == Some(provider) && current_model == Some(id),
                speeds: Vec::new(),
                default_speed: None,
            });
        }
    }
    list
}

fn thread_from(item: &Value) -> Option<HistoryThread> {
    let id = translate::str_at(item, &["sessionId"])?.to_owned();
    let title = translate::str_at(item, &["projections", "values", "title"])
        .filter(|t| !t.trim().is_empty())
        .unwrap_or("（无标题）")
        .to_owned();
    let updated = item.get("updatedAt").and_then(Value::as_u64).unwrap_or(0);
    Some(HistoryThread {
        id,
        preview: title.clone(),
        title,
        cwd: translate::str_at(item, &["cwd"])
            .unwrap_or_default()
            .to_owned(),
        origin: Some(ORIGIN_LABEL.into()),
        created_at: updated,
        updated_at: updated,
        chat: false,
        project: None,
    })
}

async fn sessions(api: &dyn api::Api) -> anyhow::Result<Vec<Value>> {
    let list = api.call("session.list", json!({})).await?;
    let mut items: Vec<Value> = list
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        // A subagent belongs to its parent's conversation.
        .filter(|s| translate::str_at(s, &["origin"]) != Some("subagent"))
        .filter(|s| s.get("blank").and_then(Value::as_bool) != Some(true))
        .collect();
    items.sort_by_key(|s| std::cmp::Reverse(s.get("updatedAt").and_then(Value::as_u64)));
    Ok(items)
}

async fn list_history(api: &dyn api::Api, query: &HistoryQuery) -> anyhow::Result<HistoryData> {
    let mut items = sessions(api).await?;
    if let Some(search) = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let found = api
            .call("session.search", json!({ "query": search }))
            .await?;
        let ids: Vec<&str> = found
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|i| translate::str_at(i, &["sessionId"]))
            .collect();
        items.retain(|s| translate::str_at(s, &["sessionId"]).is_some_and(|id| ids.contains(&id)));
    }
    let start: usize = match &query.cursor {
        Some(cursor) => cursor.parse().context("历史对话的 cursor 无效")?,
        None => 0,
    };
    let limit = if query.limit == 0 {
        50
    } else {
        query.limit as usize
    };
    let page: Vec<HistoryThread> = items
        .iter()
        .skip(start)
        .take(limit)
        .filter_map(thread_from)
        .collect();
    let next = start + limit;
    Ok(HistoryData {
        threads: page,
        next_cursor: (next < items.len()).then(|| next.to_string()),
        projects: Vec::new(),
    })
}

async fn read_history(api: &dyn api::Api, id: &str) -> anyhow::Result<HistoryReadData> {
    let page = api
        .call(
            "session.history",
            json!({ "sessionId": id, "maxMessages": HISTORY_MESSAGES }),
        )
        .await
        .with_context(|| format!("读取 DSH 会话 {id} 失败"))?;
    let listed = sessions(api).await.unwrap_or_default();
    let mut thread = listed
        .iter()
        .find(|s| translate::str_at(s, &["sessionId"]) == Some(id))
        .and_then(thread_from)
        .unwrap_or_else(|| HistoryThread {
            id: id.to_owned(),
            title: "（无标题）".into(),
            preview: String::new(),
            cwd: String::new(),
            origin: Some(ORIGIN_LABEL.into()),
            created_at: 0,
            updated_at: 0,
            chat: false,
            project: None,
        });
    if let Some(title) = translate::str_at(&page, &["projections", "values", "title"]) {
        thread.title = title.to_owned();
    }
    let events = history_events(&page);
    if let Some(first) = events.first() {
        thread.created_at = first.at;
    }
    if let Some(first_user) = events.iter().find_map(|e| match &e.kind {
        tether_proto::EventKind::UserText { text, .. } => Some(text.clone()),
        _ => None,
    }) {
        thread.preview = first_user.chars().take(120).collect();
    }
    Ok(HistoryReadData {
        thread,
        events,
        truncated: page.get("hasMore").and_then(Value::as_bool) == Some(true),
    })
}

/// A `session.history` page as Tether events, `seq` from 1.
fn history_events(page: &Value) -> Vec<Event> {
    let mut tr = translate::Translator::history();
    let mut events = Vec::new();
    for entry in page
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(event) = entry.get("event") else {
            continue;
        };
        let mut with_view = event.clone();
        if let Some(view) = entry.get("view") {
            with_view["view"] = view.clone();
        }
        let at = event.get("time").and_then(Value::as_f64).unwrap_or(0.0) as u64;
        for kind in tr.event(&with_view) {
            events.push(Event {
                seq: events.len() as u64 + 1,
                at,
                kind,
            });
        }
    }
    events
}

//! The Codex app's view of one thread, kept in step from its broadcasts: a
//! snapshot (`conversationState`), then patches in immer's form (`op` add,
//! replace or remove at a `path` of keys and indexes). Each change becomes the
//! events the thread's progress amounts to.
//!
//! Turns are `turnHistory.history.entitiesByKey` entries (`status`, `turnId`,
//! `items`, `error`); items have app-server shapes, plus `startedAtMs` and
//! `completedAtMs`. Approvals the app waits on are `requests` (`id`, `method`,
//! `params`), as app-server asked for them.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Context, bail, ensure};
use serde_json::Value;
use tether_proto::{Decision, EventKind, NoticeLevel, SessionState};

use crate::{history, translate};

#[cfg(test)]
pub(crate) const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
pub(crate) const FILE_APPROVAL: &str = "item/fileChange/requestApproval";

#[derive(Debug, Default)]
struct Seen {
    /// Bytes of an agent message already sent as deltas.
    text: usize,
    done: bool,
}

/// An approval the app is waiting on.
#[derive(Debug, Clone)]
pub(crate) struct Asked {
    /// The app's request id, as it is.
    pub(crate) id: Value,
    pub(crate) method: String,
    item: Option<String>,
}

#[derive(Debug)]
pub(crate) enum Applied {
    Events(Vec<EventKind>),
    /// A patch did not fit: ask for a snapshot.
    Stale,
}

#[derive(Debug)]
pub(crate) struct Mirror {
    thread: String,
    state: Value,
    revision: Option<u64>,
    items: HashMap<String, Seen>,
    /// Turns seen in progress, by entity key and by turn id.
    open: HashSet<String>,
    asked: HashMap<String, Asked>,
    answered: HashSet<String>,
    /// Texts the phone sent: their user messages are the server's to show.
    sent: VecDeque<String>,
    /// What the last state event said.
    working: bool,
}

impl Mirror {
    pub(crate) fn new(thread: impl Into<String>) -> Self {
        Self {
            thread: thread.into(),
            state: Value::Null,
            revision: None,
            items: HashMap::new(),
            open: HashSet::new(),
            asked: HashMap::new(),
            answered: HashSet::new(),
            sent: VecDeque::new(),
            working: false,
        }
    }

    pub(crate) fn sent(&mut self, text: &str) {
        self.sent.push_back(text.trim().to_owned());
    }

    /// A rejected start must not hide a later message typed at the computer.
    pub(crate) fn unsent(&mut self, text: &str) {
        if let Some(at) = self.sent.iter().position(|sent| sent == text.trim()) {
            self.sent.remove(at);
        }
    }

    /// Start replies and state broadcasts can arrive in either order.
    pub(crate) fn has_turn(&self, turn: &str) -> bool {
        entities(&self.state).any(|(_, entity)| entity["turnId"] == turn)
    }

    pub(crate) fn approval_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.asked.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// The phone said `state(working)` (or the session did otherwise).
    pub(crate) fn set_working(&mut self, working: bool) {
        self.working = working;
    }

    pub(crate) fn working(&self) -> bool {
        self.working
    }

    /// A whole state. The first only sets the scene: what is already there is
    /// history, and only a turn in progress or a waiting approval is told.
    pub(crate) fn snapshot(&mut self, state: Value, revision: u64, first: bool) -> Vec<EventKind> {
        self.state = state;
        self.revision = Some(revision);
        self.diff(first)
    }

    pub(crate) fn patch(&mut self, base: u64, revision: u64, patches: &[Value]) -> Applied {
        if self.revision != Some(base) {
            return Applied::Stale;
        }
        for patch in patches {
            if let Err(error) = apply(&mut self.state, patch) {
                tracing::debug!(%error, "Codex app patch did not apply");
                self.revision = None;
                return Applied::Stale;
            }
        }
        self.revision = Some(revision);
        Applied::Events(self.diff(false))
    }

    /// Whether a turn is in progress.
    pub(crate) fn busy(&self) -> bool {
        !self.open.is_empty()
    }

    /// The turn in progress, once the app knows its id.
    pub(crate) fn active_turn(&self) -> Option<String> {
        entities(&self.state)
            .find(|(_, e)| e["status"] == "inProgress")
            .and_then(|(_, e)| e["turnId"].as_str())
            .map(str::to_owned)
    }

    /// The approval to answer, if the app still waits on it.
    pub(crate) fn answer(&mut self, approval: &str) -> Option<Asked> {
        let asked = self.asked.remove(approval)?;
        self.answered.insert(approval.to_owned());
        Some(asked)
    }

    pub(crate) fn waiting(&self) -> bool {
        !self.asked.is_empty()
    }

    fn diff(&mut self, first: bool) -> Vec<EventKind> {
        let mut out = Vec::new();
        // Settled approvals first: their items end after them.
        let (asked_new, resolved) = self.requests(&mut out);
        let mut ended = false;
        let Self {
            state,
            items,
            open,
            sent,
            working,
            ..
        } = self;
        let mut turns: Vec<(&String, &Value)> = entities(state).collect();
        turns.sort_by_key(|(_, e)| started_at(e));
        for (key, entity) in turns {
            let turn = entity["turnId"].as_str();
            let status = entity["status"].as_str().unwrap_or_default();
            let known = open.contains(key.as_str()) || turn.is_some_and(|t| open.contains(t));
            if status == "inProgress" {
                open.insert(key.clone());
                if let Some(turn) = turn {
                    open.insert(turn.to_owned());
                }
                if !*working {
                    out.push(translate::state(SessionState::Working));
                    *working = true;
                }
            }
            // What a running turn has done so far is not in the saved history yet.
            let quiet = first && status != "inProgress";
            for item in entity["items"].as_array().into_iter().flatten() {
                item_events(item, items, sent, quiet, first, &mut out);
            }
            if known && matches!(status, "completed" | "failed" | "interrupted") {
                open.remove(key.as_str());
                if let Some(turn) = turn {
                    open.remove(turn);
                }
                if let Some(error) = entity.get("error").filter(|e| !e.is_null()) {
                    out.push(translate::notice(
                        NoticeLevel::Error,
                        translate::error_text(error),
                    ));
                }
                out.push(EventKind::TurnDone { summary: None });
                ended = true;
            }
        }
        if asked_new {
            out.push(translate::state(SessionState::NeedsYou));
            self.working = true;
        } else if resolved && self.asked.is_empty() && self.busy() {
            out.push(translate::state(SessionState::Working));
            self.working = true;
        }
        if ended && !self.busy() {
            out.push(translate::state(SessionState::Idle));
            self.working = false;
        }
        out
    }

    /// New approvals, and those settled in the app; whether there were any.
    fn requests(&mut self, out: &mut Vec<EventKind>) -> (bool, bool) {
        let mut current = HashSet::new();
        let mut asked_new = false;
        for request in self.state["requests"].as_array().into_iter().flatten() {
            let method = request["method"].as_str().unwrap_or_default();
            let params = &request["params"];
            if params["threadId"]
                .as_str()
                .is_some_and(|t| t != self.thread)
            {
                continue;
            }
            let id = approval_id(&request["id"]);
            current.insert(id.clone());
            if self.asked.contains_key(&id) || self.answered.contains(&id) {
                continue;
            }
            let item = params["itemId"].as_str();
            let previous = item.and_then(|item| find_item(&self.state, item));
            match translate::request(method, params, id.clone(), previous) {
                Ok(Some(approval)) => {
                    self.asked.insert(
                        id,
                        Asked {
                            id: request["id"].clone(),
                            method: method.to_owned(),
                            item: item.map(str::to_owned),
                        },
                    );
                    out.push(EventKind::Approval { approval });
                    asked_new = true;
                }
                Ok(None) => {}
                Err(error) => tracing::debug!(%error, method, "Codex app request not shown"),
            }
        }
        let settled: Vec<String> = self
            .asked
            .keys()
            .filter(|id| !current.contains(*id))
            .cloned()
            .collect();
        for id in &settled {
            if let Some(asked) = self.asked.remove(id) {
                // Answered at the computer: the item tells which way.
                let declined = asked
                    .item
                    .as_deref()
                    .and_then(|item| find_item(&self.state, item))
                    .is_some_and(|item| item["status"] == "declined");
                out.push(EventKind::ApprovalDone {
                    approval: id.clone(),
                    decision: if declined {
                        Decision::Deny
                    } else {
                        Decision::AllowOnce
                    },
                });
            }
        }
        self.answered.retain(|id| current.contains(id));
        (asked_new, !settled.is_empty())
    }
}

fn entities(state: &Value) -> impl Iterator<Item = (&String, &Value)> {
    state
        .pointer("/turnHistory/history/entitiesByKey")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
}

fn started_at(entity: &Value) -> u64 {
    entity["turnStartedAtMs"]
        .as_u64()
        .or_else(|| entity["items"][0]["startedAtMs"].as_u64())
        .unwrap_or(u64::MAX)
}

fn find_item<'a>(state: &'a Value, id: &str) -> Option<&'a Value> {
    entities(state)
        .flat_map(|(_, e)| e["items"].as_array().into_iter().flatten())
        .find(|item| item["id"] == id)
}

/// Request ids are numbers or strings; approvals are named by their text.
fn approval_id(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn done(item: &Value) -> bool {
    !item["completedAtMs"].is_null()
        || matches!(
            item["status"].as_str(),
            Some("completed" | "failed" | "declined")
        )
}

fn item_events(
    item: &Value,
    items: &mut HashMap<String, Seen>,
    sent: &mut VecDeque<String>,
    quiet: bool,
    first: bool,
    out: &mut Vec<EventKind>,
) {
    let Some(id) = item["id"].as_str() else {
        return;
    };
    let kind = item["type"].as_str().unwrap_or_default();
    let new = !items.contains_key(id);
    if quiet {
        let text = item["text"].as_str().map_or(0, str::len);
        items.insert(id.to_owned(), Seen { text, done: true });
        return;
    }
    let seen = items.entry(id.to_owned()).or_default();
    match kind {
        "userMessage" => {
            // The phone's history has it: a turn's prompt is saved as it starts.
            if new && !first {
                let (text, images) = history::user_message(item);
                if sent.front().is_some_and(|s| *s == text.trim()) {
                    sent.pop_front();
                } else if !text.trim().is_empty() || !images.is_empty() {
                    // Typed at the computer.
                    out.push(EventKind::UserText {
                        text,
                        images: history::pictures(images),
                    });
                }
            }
            seen.done = true;
        }
        "agentMessage" => {
            let text = item["text"].as_str().unwrap_or_default();
            if text.len() > seen.text && text.is_char_boundary(seen.text) {
                out.push(EventKind::AssistantDelta {
                    item: id.to_owned(),
                    text: text[seen.text..].to_owned(),
                });
                seen.text = text.len();
            }
            if done(item) && !seen.done {
                seen.done = true;
                extend(out, translate::item(item, true));
            }
        }
        _ => {
            if new {
                extend(out, translate::item(item, false));
            }
            if done(item) && !seen.done {
                seen.done = true;
                extend(out, translate::item(item, true));
            }
        }
    }
}

fn extend(out: &mut Vec<EventKind>, events: anyhow::Result<Vec<EventKind>>) {
    match events {
        Ok(events) => out.extend(events),
        Err(error) => tracing::debug!(%error, "Codex app item not shown"),
    }
}

/// One immer patch.
fn apply(doc: &mut Value, patch: &Value) -> anyhow::Result<()> {
    let op = patch["op"].as_str().context("patch without op")?;
    let path = patch["path"].as_array().context("patch without path")?;
    let value = patch.get("value").cloned().unwrap_or(Value::Null);
    let Some((last, parents)) = path.split_last() else {
        ensure!(op != "remove", "cannot remove the whole state");
        *doc = value;
        return Ok(());
    };
    let mut target = doc;
    for segment in parents {
        target = match target {
            Value::Object(map) => map.get_mut(&key(segment)?),
            Value::Array(list) => list.get_mut(index(segment)?),
            _ => None,
        }
        .context("patch path is missing")?;
    }
    match target {
        Value::Object(map) => match op {
            "add" | "replace" => {
                map.insert(key(last)?, value);
            }
            "remove" => {
                map.remove(&key(last)?);
            }
            _ => bail!("unknown patch op {op}"),
        },
        Value::Array(list) => {
            if last == "length" {
                let len = value.as_u64().context("length is not a number")?;
                list.resize(usize::try_from(len)?, Value::Null);
                return Ok(());
            }
            let at = if last == "-" {
                list.len()
            } else {
                index(last)?
            };
            match op {
                "add" => {
                    ensure!(at <= list.len(), "patch index out of range");
                    list.insert(at, value);
                }
                "replace" => *list.get_mut(at).context("patch index out of range")? = value,
                "remove" => {
                    ensure!(at < list.len(), "patch index out of range");
                    list.remove(at);
                }
                _ => bail!("unknown patch op {op}"),
            }
        }
        _ => bail!("patch path goes through a plain value"),
    }
    Ok(())
}

fn key(segment: &Value) -> anyhow::Result<String> {
    match segment {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        _ => bail!("odd patch path segment"),
    }
}

fn index(segment: &Value) -> anyhow::Result<usize> {
    match segment {
        Value::Number(n) => Ok(usize::try_from(n.as_u64().context("negative index")?)?),
        Value::String(s) => Ok(s.parse()?),
        _ => bail!("odd patch index"),
    }
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod tests;

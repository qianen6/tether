//! What the Codex desktop app keeps to itself, and how to point it at a
//! conversation.
//!
//! - The app files conversations under its projects and "Chats" in its own
//!   state file, `~/.codex/.codex-global-state.json` (`thread-project-assignments`,
//!   `projectless-thread-ids`). It belongs to the app and is only read here.
//! - The app lists conversations other programs started only after it reloads
//!   its list; opening `codex://threads/<id>` makes it load that one (as
//!   Remodex does, `phodex-bridge/src/desktop-handler.js`, Apache-2.0), and a
//!   "thread-unarchived" broadcast on its bus lists one without opening it.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde_json::Value;

pub(crate) const STATE_FILE: &str = ".codex-global-state.json";
/// Visiting another page first makes an app already showing that thread
/// reload it; then the router needs a moment.
#[cfg(all(windows, not(test)))]
const BOUNCE: &str = "codex://settings";
#[cfg(all(windows, not(test)))]
const SETTLE: std::time::Duration = std::time::Duration::from_millis(600);
/// After a turn starts, before its thread is opened.
#[cfg(not(test))]
const AFTER_TURN: std::time::Duration = std::time::Duration::from_millis(800);

/// Where the app files conversations.
#[derive(Debug, Default)]
pub(crate) struct Placement {
    /// Thread id → app-server project id.
    pub(crate) projects: HashMap<String, String>,
    /// Threads in the app's "Chats".
    pub(crate) chats: HashSet<String>,
}

/// `~/.codex`, or `CODEX_HOME`.
pub(crate) fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".codex")))
}

/// The app's placement on this computer; empty when it has none.
pub(crate) fn placement() -> Placement {
    let Some(path) = codex_home().map(|home| home.join(STATE_FILE)) else {
        return Placement::default();
    };
    match read_placement(&path) {
        Ok(placement) => placement,
        Err(error) => {
            tracing::debug!(%error, "no Codex desktop placement");
            Placement::default()
        }
    }
}

/// What the app lets its agent do now on this computer ("full-access",
/// "guardian-approvals", …), as its composer shows it; `None` without the app.
pub(crate) fn agent_mode() -> Option<String> {
    let path = codex_home()?.join(STATE_FILE);
    let text = std::fs::read(path).ok()?;
    let state: Value = serde_json::from_slice(&text).ok()?;
    agent_mode_from(&state)
}

pub(crate) fn agent_mode_from(state: &Value) -> Option<String> {
    let atoms = &state["electron-persisted-atom-state"];
    let selection = &atoms["permission-selection-by-host-id:local"];
    selection["agentMode"]
        .as_str()
        .or(atoms["agent-mode-by-host-id"]["local"].as_str())
        .filter(|m| !m.is_empty())
        .map(Into::into)
}

fn read_placement(path: &Path) -> anyhow::Result<Placement> {
    let text = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let state: Value = serde_json::from_slice(&text).context("parse Codex desktop state")?;
    Ok(placement_from(&state))
}

/// Assignments name the app's older project ids; the app keeps a map from
/// those to app-server ones per host ("local:<codex home>").
pub(crate) fn placement_from(state: &Value) -> Placement {
    let mut ids: HashMap<&str, &str> = HashMap::new();
    if let Some(hosts) = state["app-server-project-id-by-legacy-project-id-by-host"].as_object() {
        for (host, map) in hosts {
            if !host.starts_with("local") {
                continue;
            }
            for (legacy, id) in map.as_object().into_iter().flatten() {
                if let Some(id) = id.as_str() {
                    ids.insert(legacy, id);
                }
            }
        }
    }
    let projects = state["thread-project-assignments"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, a)| a["projectKind"].as_str().is_none_or(|k| k == "local"))
        .filter_map(|(thread, a)| {
            let legacy = a["projectId"].as_str()?;
            let id = ids.get(legacy).copied().unwrap_or(legacy);
            Some((thread.clone(), id.to_owned()))
        })
        .collect();
    let chats = state["projectless-thread-ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect();
    Placement { projects, chats }
}

/// Thread ids are uuids; nothing else goes into a URL that is handed to the
/// system.
fn valid(thread: &str) -> bool {
    !thread.is_empty()
        && thread.len() <= 128
        && thread
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Opens `thread` in the desktop app now (in the background).
pub(crate) fn reveal(thread: &str) -> anyhow::Result<()> {
    ensure!(valid(thread), "对话编号无效");
    let thread = thread.to_owned();
    std::thread::spawn(move || {
        if let Err(error) = open_thread(&thread) {
            tracing::warn!(%error, "open the Codex desktop app");
        }
    });
    Ok(())
}

/// Opens `thread` in the desktop app shortly, once its first turn is saved.
pub(crate) fn reveal_soon(thread: String) {
    #[cfg(test)]
    {
        REVEALED.with(|r| r.borrow_mut().push(thread));
    }
    #[cfg(not(test))]
    {
        if !valid(&thread) {
            return;
        }
        std::thread::spawn(move || {
            std::thread::sleep(AFTER_TURN);
            if let Err(error) = open_thread(&thread) {
                tracing::warn!(%error, "open the Codex desktop app");
            }
        });
    }
}

/// Lists `thread` in the desktop app's sidebar shortly, once its first turn is
/// saved, without opening it (`follow::announce`).
pub(crate) fn announce_soon(thread: String) {
    #[cfg(test)]
    {
        ANNOUNCED.with(|r| r.borrow_mut().push(thread));
    }
    #[cfg(not(test))]
    {
        if !valid(&thread) {
            return;
        }
        tokio::spawn(async move {
            tokio::time::sleep(AFTER_TURN).await;
            let result = match crate::bus::connect().await {
                Ok((reader, writer)) => crate::follow::announce(reader, writer, &thread).await,
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                tracing::debug!(
                    error = format!("{error:#}"),
                    "list the thread in the Codex desktop app"
                );
            }
        });
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static ANNOUNCED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    pub(crate) static REVEALED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn open_thread(_thread: &str) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(not(test))]
fn open_thread(thread: &str) -> anyhow::Result<()> {
    let url = format!("codex://threads/{thread}");
    #[cfg(windows)]
    {
        open_url(BOUNCE)?;
        std::thread::sleep(SETTLE);
    }
    open_url(&url)
}

#[cfg(all(not(test), windows))]
fn open_url(url: &str) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    let system = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("rundll32.exe"))
        .unwrap_or_else(|| PathBuf::from("rundll32.exe"));
    let status = std::process::Command::new(system)
        .args(["url.dll,FileProtocolHandler", url])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()
        .context("start rundll32")?;
    ensure!(status.success(), "rundll32 exited with {status}");
    Ok(())
}

#[cfg(all(not(test), not(windows)))]
fn open_url(url: &str) -> anyhow::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = std::process::Command::new(opener)
        .arg(url)
        .status()
        .with_context(|| format!("start {opener}"))?;
    ensure!(status.success(), "{opener} exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_older_project_ids_to_app_server_ones() {
        let placement = placement_from(&json!({
            "app-server-project-id-by-legacy-project-id-by-host": {
                "local:C:\\Users\\q\\.codex": {"7761": "01a1bed8", "local-42": "01a06f29"},
                "remote:box": {"r": "elsewhere"}
            },
            "thread-project-assignments": {
                "t1": {"projectKind": "local", "projectId": "7761"},
                "t2": {"projectKind": "local", "projectId": "local-42"},
                "t3": {"projectKind": "local", "projectId": "01a1ffff"},
                "t4": {"projectKind": "remote", "projectId": "r"},
                "t5": {"projectKind": "local"}
            },
            "projectless-thread-ids": ["c1", "c2", 3]
        }));
        let mut projects: Vec<_> = placement
            .projects
            .iter()
            .map(|(t, p)| (t.as_str(), p.as_str()))
            .collect();
        projects.sort();
        assert_eq!(
            projects,
            vec![("t1", "01a1bed8"), ("t2", "01a06f29"), ("t3", "01a1ffff")]
        );
        assert_eq!(placement.chats.len(), 2);
        assert!(placement.chats.contains("c1"));
    }

    #[test]
    fn a_missing_or_odd_state_places_nothing() {
        let placement = placement_from(&json!({"thread-project-assignments": []}));
        assert!(placement.projects.is_empty() && placement.chats.is_empty());
        let missing = read_placement(Path::new("/definitely/not/here.json"));
        assert!(missing.is_err());
    }

    #[test]
    fn only_plain_ids_are_opened() {
        assert!(valid("01a1bf34-a298-7781-b400-c0bdf0a68bca"));
        assert!(!valid(""));
        assert!(!valid("x&calc"));
        assert!(!valid("../settings"));
        assert!(reveal("a b").is_err());
        assert!(reveal("01a1bf34").is_ok());
    }
}

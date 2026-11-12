//! Models, chats and the agent's own history, through a fake that records
//! what it was asked for.

use super::*;
use std::{path::PathBuf, sync::Mutex, time::Duration};
use tether_agent_api::{
    AgentAdapter, AgentCommand, AgentOutput, BoxFuture, HistoryQuery, HistoryRead, ModelQuery,
    SpawnOptions,
};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Default)]
struct Picky {
    spawns: Mutex<Vec<SpawnOptions>>,
    revealed: Mutex<Vec<String>>,
    chat_root: PathBuf,
    release: Option<Duration>,
}

impl AgentAdapter for Picky {
    fn info(&self) -> AgentInfo {
        AgentInfo {
            id: "picky".into(),
            ..EchoAdapter.info()
        }
    }

    fn spawn(
        &self,
        opts: SpawnOptions,
        out: UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
        lock(&self.spawns).push(opts.clone());
        let mut opts = opts;
        // Echo answers to the id it is given: a copy gets one of its own.
        if opts.fork {
            opts.resume = opts.resume.map(|id| format!("{id}-copy"));
        }
        EchoAdapter.spawn(opts, out)
    }

    fn release_after(&self) -> Option<Duration> {
        self.release
    }

    fn list_history(&self, query: HistoryQuery) -> BoxFuture<HistoryData> {
        Box::pin(async move {
            Ok(HistoryData {
                threads: vec![thread(query.search.as_deref().unwrap_or("all"))],
                next_cursor: query.cursor.map(|c| format!("{c}+")),
                projects: Vec::new(),
            })
        })
    }

    fn read_history(&self, read: HistoryRead) -> BoxFuture<HistoryReadData> {
        Box::pin(async move {
            anyhow::ensure!(read.thread != "broken", "disk on fire");
            Ok(HistoryReadData {
                thread: thread(&read.thread),
                events: vec![Event {
                    seq: 1,
                    at: 5,
                    kind: EventKind::UserText {
                        text: "旧消息".into(),
                        images: None,
                    },
                }],
                truncated: false,
            })
        })
    }

    fn list_models(&self, _query: ModelQuery) -> BoxFuture<Vec<ModelInfo>> {
        Box::pin(async {
            Ok(vec![ModelInfo {
                id: "gpt-6.1-sol".into(),
                name: "gpt-6.1-sol".into(),
                description: None,
                efforts: vec!["low".into(), "high".into()],
                default_effort: Some("high".into()),
                is_default: true,
                speeds: Vec::new(),
                default_speed: None,
            }])
        })
    }

    fn reveal(&self, thread: &str) -> anyhow::Result<()> {
        anyhow::ensure!(thread != "nowhere", "没有这个对话");
        lock(&self.revealed).push(thread.into());
        Ok(())
    }

    fn chat_dir(&self, hint: &str) -> anyhow::Result<Option<PathBuf>> {
        let dir = self.chat_root.join(hint.replace(' ', "-"));
        std::fs::create_dir_all(&dir)?;
        Ok(Some(dir))
    }
}

fn thread(id: &str) -> HistoryThread {
    HistoryThread {
        id: id.into(),
        title: format!("对话 {id}"),
        preview: String::new(),
        cwd: "/w".into(),
        origin: Some("Codex Desktop".into()),
        created_at: 1,
        updated_at: 2,
        chat: false,
        project: None,
    }
}

/// A folder as the server records it.
fn real(path: &std::path::Path) -> std::path::PathBuf {
    crate::gateway::simplify(std::fs::canonicalize(path).unwrap())
}

async fn picky() -> (Server, Arc<Picky>, Phone, tempfile::TempDir) {
    picky_releasing(None).await
}

async fn picky_releasing(
    release: Option<Duration>,
) -> (Server, Arc<Picky>, Phone, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let agent = Arc::new(Picky {
        chat_root: dir.path().to_path_buf(),
        release,
        ..Default::default()
    });
    let server = Server::start_with(
        config(),
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryTerms::default()),
        vec![agent.clone(), Arc::new(EchoAdapter)],
    )
    .await
    .unwrap();
    let phone = Phone::pair(&server).await;
    (server, agent, phone, dir)
}

fn session(data: serde_json::Value) -> SessionInfo {
    serde_json::from_value(data["session"].clone()).unwrap()
}

#[tokio::test]
async fn a_chat_gets_its_own_folder_and_the_chosen_model() {
    let (_server, agent, mut phone, dir) = picky().await;
    let info = session(
        phone
            .call(
                "create_session",
                json!({"agent":"picky","title":"hello world","model":"gpt-6.1-sol","effort":"high","chat":true,"cwd":"/ignored"}),
            )
            .await,
    );
    assert!(info.chat);
    assert_eq!(
        (info.model.as_deref(), info.effort.as_deref()),
        (Some("gpt-6.1-sol"), Some("high"))
    );
    let folder = dir.path().join("hello-world");
    assert_eq!(PathBuf::from(&info.cwd), folder);
    assert!(folder.is_dir());

    phone.subscribe(&info.id, 0).await;
    phone.send_text(&info.id, "hi").await;
    phone
        .wait(|w| {
            matches!(
                w,
                Wire::Json(ServerMsg::Event {
                    event: Event {
                        kind: EventKind::TurnDone { .. },
                        ..
                    },
                    ..
                })
            )
        })
        .await;
    let spawned = lock(&agent.spawns)[0].clone();
    assert!(spawned.chat);
    assert_eq!(spawned.cwd, folder);
    assert_eq!(spawned.model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(spawned.effort.as_deref(), Some("high"));
}

#[tokio::test]
async fn set_model_is_kept_told_to_the_agent_and_used_on_respawn() {
    let (_server, agent, mut phone, _dir) = picky().await;
    let info = session(phone.call("create_session", json!({"agent":"picky"})).await);
    assert_eq!(info.model, None);

    // Nothing runs yet: only remembered.
    phone
        .call(
            "set_model",
            json!({"session":info.id,"model":"a","effort":null}),
        )
        .await;
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::SessionUpdate { session }) if session.model.as_deref() == Some("a")))
        .await;

    phone.subscribe(&info.id, 0).await;
    phone.send_text(&info.id, "hi").await;
    phone
        .wait(|w| {
            matches!(
                w,
                Wire::Json(ServerMsg::Event {
                    event: Event {
                        kind: EventKind::TurnDone { .. },
                        ..
                    },
                    ..
                })
            )
        })
        .await;
    assert_eq!(lock(&agent.spawns)[0].model.as_deref(), Some("a"));

    // Running: the agent hears it; the effort alone changes.
    phone
        .call("set_model", json!({"session":info.id,"effort":"low"}))
        .await;
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::Event { event: Event { kind: EventKind::Notice { text, .. }, .. }, .. }) if text == "model: default effort: low"))
        .await;
    let listed: SessionsData =
        serde_json::from_value(phone.call("list_sessions", json!({})).await).unwrap();
    let kept = listed.sessions.iter().find(|s| s.id == info.id).unwrap();
    assert_eq!(
        (kept.model.as_deref(), kept.effort.as_deref()),
        (Some("a"), Some("low"))
    );

    let bad = phone
        .request("set_model", json!({"session":info.id,"model":"x\ny"}))
        .await;
    phone.error(Some(bad), ErrorCode::BadRequest).await;
    let bad = phone
        .request("create_session", json!({"agent":"picky","effort":""}))
        .await;
    phone.error(Some(bad), ErrorCode::BadRequest).await;
}

#[tokio::test]
async fn models_and_history_come_from_the_agent() {
    let (_server, _agent, mut phone, _dir) = picky().await;
    let models: ModelsData =
        serde_json::from_value(phone.call("list_models", json!({"agent":"picky"})).await).unwrap();
    assert_eq!(models.models[0].id, "gpt-6.1-sol");
    let none: ModelsData =
        serde_json::from_value(phone.call("list_models", json!({"agent":"echo"})).await).unwrap();
    assert!(none.models.is_empty());

    let page: HistoryData = serde_json::from_value(
        phone
            .call(
                "list_history",
                json!({"agent":"picky","search":"微信","cursor":"c"}),
            )
            .await,
    )
    .unwrap();
    assert_eq!(page.threads[0].id, "微信");
    assert_eq!(page.next_cursor.as_deref(), Some("c+"));

    let read: HistoryReadData = serde_json::from_value(
        phone
            .call("read_history", json!({"agent":"picky","thread":"t1"}))
            .await,
    )
    .unwrap();
    assert_eq!(read.thread.id, "t1");
    assert_eq!(read.events.len(), 1);

    let broken = phone
        .request("read_history", json!({"agent":"picky","thread":"broken"}))
        .await;
    phone.error(Some(broken), ErrorCode::Internal).await;
    let bad = phone
        .request("read_history", json!({"agent":"picky","thread":""}))
        .await;
    phone.error(Some(bad), ErrorCode::BadRequest).await;
    let without = phone.request("list_history", json!({"agent":"echo"})).await;
    phone.error(Some(without), ErrorCode::BadRequest).await;
}

#[tokio::test]
async fn resuming_a_thread_reuses_its_idle_session_only_by_replacing_it() {
    let (_server, agent, mut phone, _dir) = picky().await;
    let first = session(
        phone
            .call("create_session", json!({"agent":"picky","resume":"t1"}))
            .await,
    );
    phone.subscribe(&first.id, 0).await;
    phone.send_text(&first.id, "hi").await;
    phone
        .wait(|w| {
            matches!(
                w,
                Wire::Json(ServerMsg::Event {
                    event: Event {
                        kind: EventKind::TurnDone { .. },
                        ..
                    },
                    ..
                })
            )
        })
        .await;
    assert_eq!(lock(&agent.spawns)[0].resume.as_deref(), Some("t1"));

    // Idle: start again from the record on disk.
    let second = session(
        phone
            .call("create_session", json!({"agent":"picky","resume":"t1"}))
            .await,
    );
    assert_ne!(first.id, second.id);
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::SessionClosed { session }) if *session == first.id))
        .await;
    let listed: SessionsData =
        serde_json::from_value(phone.call("list_sessions", json!({})).await).unwrap();
    assert_eq!(listed.sessions.len(), 1);
}

#[tokio::test]
async fn a_project_conversation_starts_in_its_project_and_can_be_revealed() {
    let (_server, agent, mut phone, dir) = picky().await;
    let root = dir.path().join("lib");
    let other = dir.path().join("meet");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    // As the server records folders: macOS' /var is /private/var, Windows'
    // temp folder may come in its short (RUNNER~1) form.
    let (root, other) = (real(&root), real(&other));
    let info = session(
        phone
            .call(
                "create_session",
                json!({"agent":"picky","cwd":root,"project":"p1","roots":[root, other],
                       "speed":"priority","reveal":true}),
            )
            .await,
    );
    assert_eq!(info.speed.as_deref(), Some("priority"));
    phone.subscribe(&info.id, 0).await;
    phone.send_text(&info.id, "hi").await;
    phone
        .wait(|w| {
            matches!(
                w,
                Wire::Json(ServerMsg::Event {
                    event: Event {
                        kind: EventKind::TurnDone { .. },
                        ..
                    },
                    ..
                })
            )
        })
        .await;
    let spawned = lock(&agent.spawns)[0].clone();
    assert_eq!(spawned.project.as_deref(), Some("p1"));
    assert_eq!(spawned.roots, vec![root.clone(), other]);
    assert_eq!(spawned.speed.as_deref(), Some("priority"));
    assert!(spawned.reveal);

    phone
        .call("set_model", json!({"session":info.id,"speed":"default"}))
        .await;
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::SessionUpdate { session }) if session.speed.as_deref() == Some("default")))
        .await;

    // A chat has no project, whatever the phone says.
    let chat = session(
        phone
            .call(
                "create_session",
                json!({"agent":"picky","title":"c","chat":true,"project":"p1","roots":[root]}),
            )
            .await,
    );
    phone.subscribe(&chat.id, 0).await;
    phone.send_text(&chat.id, "hi").await;
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::Event { session, event: Event { kind: EventKind::TurnDone { .. }, .. } }) if *session == chat.id))
        .await;
    let spawned = lock(&agent.spawns)[1].clone();
    assert_eq!(
        (spawned.project, spawned.roots.len(), spawned.reveal),
        (None, 0, false)
    );

    phone
        .call("reveal", json!({"agent":"picky","thread":"t1"}))
        .await;
    assert_eq!(*lock(&agent.revealed), vec!["t1".to_string()]);
    let missing = phone
        .request("reveal", json!({"agent":"picky","thread":"nowhere"}))
        .await;
    phone.error(Some(missing), ErrorCode::BadRequest).await;
    let unsupported = phone
        .request("reveal", json!({"agent":"echo","thread":"t1"}))
        .await;
    phone.error(Some(unsupported), ErrorCode::BadRequest).await;
}

async fn turn(phone: &mut Phone, id: &str, text: &str) {
    phone.send_text(id, text).await;
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::Event { session, event: Event { kind: EventKind::TurnDone { .. }, .. } }) if session == id))
        .await;
}

async fn listed(phone: &mut Phone, id: &str) -> SessionInfo {
    let data = phone.call("list_sessions", json!({})).await;
    let all: Vec<SessionInfo> = serde_json::from_value(data["sessions"].clone()).unwrap();
    all.into_iter().find(|s| s.id == id).unwrap()
}

#[tokio::test]
async fn an_idle_agent_lets_go_and_the_next_message_resumes_it() {
    let (_server, agent, mut phone, dir) = picky_releasing(Some(Duration::from_millis(100))).await;
    let info = session(
        phone
            .call("create_session", json!({"agent":"picky","cwd":dir.path()}))
            .await,
    );
    assert_eq!(info.thread, None);
    phone.subscribe(&info.id, 0).await;
    turn(&mut phone, &info.id, "hi").await;
    let thread = listed(&mut phone, &info.id).await.thread.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    turn(&mut phone, &info.id, "again").await;
    let spawns = lock(&agent.spawns).clone();
    assert_eq!(spawns.len(), 2);
    assert_eq!(spawns[1].resume.as_deref(), Some(thread.as_str()));
}

#[tokio::test]
async fn revealing_a_conversation_hands_it_over() {
    let (_server, agent, mut phone, dir) = picky().await;
    let info = session(
        phone
            .call("create_session", json!({"agent":"picky","cwd":dir.path()}))
            .await,
    );
    phone.subscribe(&info.id, 0).await;
    turn(&mut phone, &info.id, "hi").await;
    let thread = listed(&mut phone, &info.id).await.thread.unwrap();
    phone
        .call("reveal", json!({"agent":"picky","thread":thread}))
        .await;
    assert_eq!(*lock(&agent.revealed), vec![thread.clone()]);
    // Let go: the next message starts it again from the record.
    turn(&mut phone, &info.id, "again").await;
    assert_eq!(lock(&agent.spawns).len(), 2);
}

#[tokio::test]
async fn a_fork_starts_from_a_copy_and_leaves_the_original() {
    let (_server, agent, mut phone, dir) = picky().await;
    let info = session(
        phone
            .call(
                "create_session",
                json!({"agent":"picky","cwd":dir.path(),"resume":"t-old","fork":true,"chat":true}),
            )
            .await,
    );
    // Not the original's yet, and a chat copy keeps the folder it was given.
    assert_eq!(info.thread, None);
    assert_eq!(std::path::Path::new(&info.cwd), real(dir.path()));
    phone.subscribe(&info.id, 0).await;
    turn(&mut phone, &info.id, "hi").await;
    let spawned = lock(&agent.spawns)[0].clone();
    assert_eq!(
        (spawned.resume.as_deref(), spawned.fork),
        (Some("t-old"), true)
    );
    assert_eq!(
        listed(&mut phone, &info.id).await.thread.as_deref(),
        Some("t-old-copy")
    );

    let bad = phone
        .request("create_session", json!({"agent":"picky","fork":true}))
        .await;
    phone.error(Some(bad), ErrorCode::BadRequest).await;
}

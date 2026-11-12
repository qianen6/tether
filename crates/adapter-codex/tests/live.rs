//! Explicit opt-in only: these tests launch the configured Codex and use a model.
//! cargo test -p tether-adapter-codex --offline --test live -- --ignored --nocapture
//! Set TETHER_RECORD_DIR to retain raw directional JSONL and stderr.

use std::time::Duration;

use tether_agent_api::{AgentCommand, AgentOutput, SpawnOptions, testkit::Driver};
use tether_proto::{Decision, EventKind};

const T: Duration = Duration::from_secs(180);

fn driver(dir: &tempfile::TempDir, resume: Option<String>) -> Driver {
    Driver::spawn(
        tether_adapter_codex::adapter().as_ref(),
        SpawnOptions {
            cwd: dir.path().to_owned(),
            resume,
            ..Default::default()
        },
    )
    .unwrap()
}

async fn native_id(driver: &mut Driver) -> String {
    driver
        .wait_for(
            |o| match o {
                AgentOutput::NativeId(id) => Some(id.clone()),
                _ => None,
            },
            T,
        )
        .await
        .unwrap()
}

async fn turn_done(driver: &mut Driver) {
    driver
        .wait_for(
            |o| matches!(o.event(), Some(EventKind::TurnDone { .. })).then_some(()),
            T,
        )
        .await
        .unwrap();
}

async fn shutdown(driver: &mut Driver) {
    driver.send(AgentCommand::Shutdown).unwrap();
    driver
        .wait_for(|o| matches!(o, AgentOutput::Exited { .. }).then_some(()), T)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "manual: launches real Codex and calls the configured model"]
async fn question_and_answer() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(&dir, None);
    native_id(&mut d).await;
    d.send(AgentCommand::Send {
        text: "只回复 OK".into(),
        images: Vec::new(),
    })
    .unwrap();
    turn_done(&mut d).await;
    assert!(
        d.events()
            .iter()
            .any(|e| matches!(e, EventKind::AssistantText { text, .. } if text.trim() == "OK"))
    );
    shutdown(&mut d).await;
}

#[tokio::test]
#[ignore = "manual: real model, shell approval and temporary file creation"]
async fn command_approval_creates_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(&dir, None);
    native_id(&mut d).await;
    d.send(AgentCommand::Send { text: "请用 shell 在当前目录创建 a.txt，内容为 hi。执行命令时明确请求用户审批（sandbox_permissions=require_escalated，理由：验证手机命令审批）。不要用文件编辑工具代替命令。完成后只回复 OK。".into(), images: Vec::new() }).unwrap();
    let approval = d
        .wait_for(
            |o| match o.event() {
                Some(EventKind::Approval { approval }) => Some(approval.id.clone()),
                _ => None,
            },
            T,
        )
        .await
        .unwrap();
    d.send(AgentCommand::Answer {
        approval,
        decision: Decision::AllowOnce,
    })
    .unwrap();
    turn_done(&mut d).await;
    let bytes = std::fs::read(dir.path().join("a.txt")).unwrap();
    let content = String::from_utf8_lossy(&bytes);
    assert!(content.contains("hi") || bytes.windows(4).any(|w| w == b"h\0i\0"));
    shutdown(&mut d).await;
}

#[tokio::test]
#[ignore = "manual: real model and persisted native thread resume"]
async fn resume_thread_and_continue() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(&dir, None);
    let id = native_id(&mut d).await;
    d.send(AgentCommand::Send {
        text: "请记住标记 TETHER_T04_RESUME，且只回复 OK。".into(),
        images: Vec::new(),
    })
    .unwrap();
    turn_done(&mut d).await;
    shutdown(&mut d).await;
    let mut resumed = driver(&dir, Some(id.clone()));
    assert_eq!(native_id(&mut resumed).await, id);
    resumed
        .send(AgentCommand::Send {
            text: "我上条消息让你记住的标记是什么？只回复标记。".into(),
            images: Vec::new(),
        })
        .unwrap();
    turn_done(&mut resumed).await;
    assert!(resumed.events().iter().any(
        |e| matches!(e, EventKind::AssistantText { text, .. } if text.contains("TETHER_T04_RESUME"))
    ));
    shutdown(&mut resumed).await;
}

/// Reads only: the models, the saved conversations and the newest one.
#[tokio::test]
#[ignore = "manual: launches real Codex (no model call)"]
async fn models_and_history_on_this_computer() {
    let codex = tether_adapter_codex::adapter();
    let models = codex
        .list_models(tether_agent_api::ModelQuery::default())
        .await
        .unwrap();
    println!(
        "models: {:?}",
        models
            .iter()
            .map(|m| (&m.id, m.is_default, &m.default_effort, &m.default_speed))
            .collect::<Vec<_>>()
    );
    assert!(models.iter().filter(|m| m.is_default).count() <= 1);
    let permissions = codex
        .list_permissions(tether_agent_api::ModelQuery::default())
        .await
        .unwrap();
    println!("permissions: {permissions:?}");
    let page = codex
        .list_history(tether_agent_api::HistoryQuery {
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    for p in &page.projects {
        println!("project {} {} {:?}", p.id, p.name, p.roots);
    }
    for t in &page.threads {
        println!(
            "thread {} chat={} project={:?} {:?} {}",
            t.id, t.chat, t.project, t.origin, t.title
        );
    }
    if let Some(first) = page.threads.first() {
        let read = codex
            .read_history(tether_agent_api::HistoryRead {
                thread: first.id.clone(),
                ..Default::default()
            })
            .await
            .unwrap();
        println!("{} events, truncated={}", read.events.len(), read.truncated);
        assert_eq!(read.thread.id, first.id);
    }
}

/// A chat with a chosen model, then a switched effort.
#[tokio::test]
#[ignore = "manual: launches real Codex and calls the configured model twice"]
async fn chat_with_a_chosen_model() {
    let codex = tether_adapter_codex::adapter();
    let models = codex
        .list_models(tether_agent_api::ModelQuery::default())
        .await
        .unwrap();
    // TETHER_LIVE_MODEL picks one the account accepts, when the default does not.
    let model = std::env::var("TETHER_LIVE_MODEL")
        .ok()
        .or_else(|| models.iter().find(|m| m.is_default).map(|m| m.id.clone()));
    let cwd = codex.chat_dir("tether live test").unwrap().unwrap();
    println!("chat folder {}", cwd.display());
    let mut d = Driver::spawn(
        codex.as_ref(),
        SpawnOptions {
            cwd: cwd.clone(),
            chat: true,
            model: model.clone(),
            effort: Some("low".into()),
            ..Default::default()
        },
    )
    .unwrap();
    native_id(&mut d).await;
    d.send(AgentCommand::Send {
        text: "只回答一个数字：1+1 等于几？".into(),
        images: Vec::new(),
    })
    .unwrap();
    let answer = d
        .wait_for(
            |o| match o.event() {
                Some(EventKind::AssistantText { text, .. }) => Some(text.clone()),
                _ => None,
            },
            T,
        )
        .await
        .unwrap();
    println!("answer: {answer}");
    assert!(answer.contains('2'), "{answer}");
    turn_done(&mut d).await;
    d.send(AgentCommand::SetModel {
        model: None,
        effort: Some("medium".into()),
        speed: None,
        permission: None,
    })
    .unwrap();
    d.send(AgentCommand::Send {
        text: "只回答一个数字：2+2 等于几？".into(),
        images: Vec::new(),
    })
    .unwrap();
    turn_done(&mut d).await;
    shutdown(&mut d).await;
    let _ = std::fs::remove_dir(&cwd);
}

//! Replays of real `dsh web` traffic (`fixtures/dsh/`, DSH 0.1.1-rc.2 driven
//! by a scripted model) through the translator, the session loop and the
//! history readers. No DSH and no model run here.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use tether_agent_api::{AgentCommand, AgentOutput, BoxFuture, HistoryQuery, SpawnOptions};
use tether_proto::{ApprovalKind, Decision, EventKind, NoticeLevel, SessionState};
use tokio::sync::{broadcast, mpsc};

use crate::{
    api::{Api, RpcError},
    hub::Frame,
    session,
    translate::{self, Translator},
};

const REPLY_TOOL_CANCEL: &str = include_str!("../../../fixtures/dsh/reply-tool-cancel.jsonl");
const APPROVAL_QUESTION: &str = include_str!("../../../fixtures/dsh/approval-question.jsonl");
const RESUME_COMMANDS: &str = include_str!("../../../fixtures/dsh/resume-commands.jsonl");
const WAIT: Duration = Duration::from_secs(5);

fn lines(fixture: &str) -> Vec<(String, Value)> {
    fixture
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("fixture line is JSON");
            (v["dir"].as_str().unwrap().to_owned(), v["v"].clone())
        })
        .collect()
}

fn mux(fixture: &str) -> Vec<(String, Value)> {
    lines(fixture)
        .into_iter()
        .filter(|(dir, _)| dir == "events.mux")
        .map(|(_, v)| {
            (
                v["rpcId"].as_str().unwrap().to_owned(),
                v["payload"].clone(),
            )
        })
        .collect()
}

/// The value DSH answered `method` with, the `nth` time it was called.
fn response(fixture: &str, method: &str, nth: usize) -> Value {
    let all = lines(fixture);
    let ids: Vec<String> = all
        .iter()
        .filter(|(dir, v)| dir == "req" && v["method"] == method)
        .map(|(_, v)| v["rpcId"].as_str().unwrap().to_owned())
        .collect();
    let id = &ids[nth];
    all.iter()
        .find(|(dir, v)| dir == "res" && v["rpcId"] == id.as_str())
        .map(|(_, v)| v["result"]["value"].clone())
        .expect("response recorded")
}

fn session_of(fixture: &str) -> String {
    response(fixture, "session.create", 0)["sessionId"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn translate(fixture: &str) -> Vec<EventKind> {
    let sid = session_of(fixture);
    let mut tr = Translator::live(Some(r"C:\work\demo".into()));
    mux(fixture)
        .iter()
        .filter(|(_, p)| p["sessionId"] == sid.as_str())
        .flat_map(|(_, p)| tr.frame(p))
        .collect()
}

#[test]
fn replies_tools_and_a_stop() {
    let events = translate(REPLY_TOOL_CANCEL);
    // Streamed pieces, then the whole reply under the same item.
    let deltas: String = events
        .iter()
        .filter_map(|e| match e {
            EventKind::AssistantDelta { item, text } if item == "a1.1" => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "你好，我是脚本模型。");
    assert!(events.contains(&EventKind::AssistantText {
        item: "a1.1".into(),
        text: "你好，我是脚本模型。".into()
    }));
    // A PowerShell command that failed, as a tool card.
    let (item, summary) = events
        .iter()
        .find_map(|e| match e {
            EventKind::ToolStart {
                item,
                tool,
                summary,
            } if tool == "pwsh" => Some((item.clone(), summary.clone())),
            _ => None,
        })
        .expect("tool start");
    assert!(summary.starts_with("Remove-Item -Recurse"), "{summary}");
    let end = events
        .iter()
        .find_map(|e| match e {
            EventKind::ToolEnd {
                item: i,
                ok,
                output,
            } if *i == item => Some((*ok, output.clone())),
            _ => None,
        })
        .expect("tool end");
    assert!(!end.0, "a failed command is not ok");
    assert!(end.1.contains("[stderr]"));
    // Four turns end; the last was stopped.
    let done = events
        .iter()
        .filter(|e| matches!(e, EventKind::TurnDone { .. }))
        .count();
    assert_eq!(done, 4);
    assert!(events.contains(&EventKind::Notice {
        level: NoticeLevel::Info,
        text: "已停止".into()
    }));
    assert_eq!(
        events.last(),
        Some(&EventKind::State {
            state: SessionState::Idle,
            status_line: None
        })
    );
    // Nothing the person typed is echoed (the server adds it).
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EventKind::UserText { .. }))
    );
}

#[test]
fn approvals_and_questions() {
    let events = translate(APPROVAL_QUESTION);
    let approvals: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EventKind::Approval { approval } => Some(approval.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(approvals.len(), 3);
    let first = &approvals[0];
    assert_eq!(first.kind, ApprovalKind::Command);
    assert_eq!(first.title, "运行：Write-Output escalate");
    assert!(first.detail.contains("danger-full-access"));
    assert_eq!(first.cwd.as_deref(), Some(r"C:\work\demo"));
    assert_eq!(
        first.choices,
        vec![Decision::AllowOnce, Decision::Deny, Decision::Abort]
    );
    let decided: Vec<Decision> = events
        .iter()
        .filter_map(|e| match e {
            EventKind::ApprovalDone { decision, .. } => Some(*decision),
            _ => None,
        })
        .collect();
    assert_eq!(
        decided,
        vec![Decision::Deny, Decision::AllowOnce, Decision::AllowOnce]
    );
    // Every approval puts the session in needs_you first.
    let needs = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                EventKind::State {
                    state: SessionState::NeedsYou,
                    ..
                }
            )
        })
        .count();
    assert!(needs >= 4, "three approvals and a question");
    let question = events
        .iter()
        .find_map(|e| match e {
            EventKind::AssistantText { item, text } if item.starts_with('q') => Some(text.clone()),
            _ => None,
        })
        .expect("question shown");
    assert!(question.contains("哪个颜色？"));
    assert!(question.contains("1. 红"));
    assert!(question.contains("2. 蓝 — 冷色"));
    // The question tool itself is no card.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EventKind::ToolStart { tool, .. } if tool == "ask_user_question"))
    );
}

#[test]
fn answers_from_a_reply() {
    let questions = vec![json!({
        "id": "color", "question": "哪个颜色？",
        "options": [{"label": "红"}, {"label": "蓝"}]
    })];
    assert_eq!(
        translate::answers(&questions, "2"),
        json!([{"id": "color", "selected": ["蓝"]}])
    );
    assert_eq!(
        translate::answers(&questions, "红"),
        json!([{"id": "color", "selected": ["红"]}])
    );
    assert_eq!(
        translate::answers(&questions, "绿色吧"),
        json!([{"id": "color", "selected": [], "custom": "绿色吧"}])
    );
    let two = vec![
        json!({"id": "a", "question": "?", "multiSelect": true,
               "options": [{"label": "x"}, {"label": "y"}, {"label": "z"}]}),
        json!({"id": "b", "question": "?"}),
    ];
    assert_eq!(
        translate::answers(&two, "1) 1, 3\n2) 随便"),
        json!([
            {"id": "a", "selected": ["x", "z"]},
            {"id": "b", "selected": [], "custom": "随便"}
        ])
    );
    assert_eq!(
        translate::answers(&two, ""),
        json!([{"id": "a", "selected": []}, {"id": "b", "selected": []}])
    );
}

// ------------------------------------------------------------- session loop

#[derive(Default)]
struct FakeApi {
    calls: Mutex<Vec<(String, Value)>>,
    replies: Mutex<HashMap<String, VecDeque<Result<Value, RpcError>>>>,
    responded: Mutex<Vec<Value>>,
    accept: Mutex<bool>,
}

impl FakeApi {
    fn new() -> Arc<Self> {
        let api = Self::default();
        *api.accept.lock().unwrap() = true;
        Arc::new(api)
    }

    fn reply(&self, method: &str, value: Result<Value, RpcError>) {
        self.replies
            .lock()
            .unwrap()
            .entry(method.into())
            .or_default()
            .push_back(value);
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

impl Api for FakeApi {
    fn call(&self, method: &str, payload: Value) -> BoxFuture<Value> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_owned(), payload));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .get_mut(method)
            .and_then(VecDeque::pop_front)
            .unwrap_or(Ok(Value::Null));
        Box::pin(async move { reply.map_err(anyhow::Error::from) })
    }

    fn respond(&self, message: Value) -> BoxFuture<bool> {
        self.responded.lock().unwrap().push(message);
        let accept = *self.accept.lock().unwrap();
        Box::pin(async move { Ok(accept) })
    }
}

struct Run {
    api: Arc<FakeApi>,
    frames: broadcast::Sender<Arc<Frame>>,
    cmd: mpsc::UnboundedSender<AgentCommand>,
    rx: mpsc::UnboundedReceiver<AgentOutput>,
    seen: Vec<AgentOutput>,
}

impl Run {
    fn start(api: Arc<FakeApi>, opts: SpawnOptions) -> Self {
        let (frames, rx_frames) = broadcast::channel(1024);
        let (cmd, commands) = mpsc::unbounded_channel();
        let (out, rx) = mpsc::unbounded_channel();
        tokio::spawn(session::run(api.clone(), rx_frames, opts, commands, out));
        Self {
            api,
            frames,
            cmd,
            rx,
            seen: Vec::new(),
        }
    }

    fn push(&self, rpc_id: &str, payload: &Value) {
        self.frames
            .send(Arc::new(Frame::Mux {
                rpc_id: rpc_id.into(),
                payload: payload.clone(),
            }))
            .unwrap();
    }

    async fn until<T>(&mut self, pick: impl Fn(&AgentOutput) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let o = tokio::time::timeout_at(deadline, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out; got {:#?}", self.seen))
                .unwrap_or_else(|| panic!("output closed; got {:#?}", self.seen));
            let got = pick(&o);
            self.seen.push(o);
            if let Some(got) = got {
                return got;
            }
        }
    }
}

#[tokio::test]
async fn a_session_against_recorded_frames() {
    let sid = session_of(APPROVAL_QUESTION);
    let api = FakeApi::new();
    api.reply(
        "session.create",
        Ok(json!({ "sessionId": sid, "agentPreset": "standard" })),
    );
    api.reply("session.prompt", Ok(json!({ "accepted": true })));
    let mut run = Run::start(
        api.clone(),
        SpawnOptions {
            cwd: r"C:\work\demo".into(),
            permission: Some("read-only".into()),
            ..Default::default()
        },
    );
    let id = run
        .until(|o| match o {
            AgentOutput::NativeId(id) => Some(id.clone()),
            _ => None,
        })
        .await;
    assert_eq!(id, sid);
    assert_eq!(
        api.calls("session.create"),
        vec![json!({ "cwd": r"C:\work\demo" })]
    );
    run.cmd
        .send(AgentCommand::Send {
            text: "WRITE one".into(),
            images: vec![],
        })
        .unwrap();
    run.until(|o| {
        matches!(
            o,
            AgentOutput::Event(EventKind::State {
                state: SessionState::Working,
                ..
            })
        )
        .then_some(())
    })
    .await;
    // Frames of the first turn, up to its approval request.
    let frames = mux(APPROVAL_QUESTION);
    let ask = frames
        .iter()
        .position(|(_, p)| p["type"] == "approval/requested")
        .unwrap();
    for (rpc, payload) in &frames[..=ask] {
        run.push(rpc, payload);
    }
    let approval = run
        .until(|o| match o {
            AgentOutput::Event(EventKind::Approval { approval }) => Some(approval.id.clone()),
            _ => None,
        })
        .await;
    run.cmd
        .send(AgentCommand::Answer {
            approval: approval.clone(),
            decision: Decision::Deny,
        })
        .unwrap();
    // The rest of the recording: DSH resolves it and the turn ends.
    let end = frames
        .iter()
        .position(|(_, p)| p["type"] == "session/event" && p["event"]["type"] == "turn/end")
        .unwrap();
    // Wait for the answer to reach DSH before replaying what followed it.
    for _ in 0..100 {
        if !run.api.responded.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for (rpc, payload) in &frames[ask + 1..=end] {
        run.push(rpc, payload);
    }
    let decision = run
        .until(|o| match o {
            AgentOutput::Event(EventKind::ApprovalDone { decision, .. }) => Some(*decision),
            _ => None,
        })
        .await;
    assert_eq!(decision, Decision::Deny);
    run.until(|o| matches!(o, AgentOutput::Event(EventKind::TurnDone { .. })).then_some(()))
        .await;
    let answer = run.api.responded.lock().unwrap()[0].clone();
    assert_eq!(answer["rpcId"], frames[ask].0.as_str());
    assert_eq!(answer["result"]["value"]["outcome"], "rejected");
    assert_eq!(answer["result"]["value"]["approvalId"], approval.as_str());
    // The prompt went out as DSH's web client sends it.
    let prompt = &api.calls("session.prompt")[0];
    assert_eq!(prompt["mode"], "queue");
    assert_eq!(prompt["content"][0]["text"], "WRITE one");
    // The permission was set through DSH's own command.
    let commands = api.calls("commands/execute");
    assert_eq!(commands[0]["args"]["line"], "/permission read-only");
    // Shutdown ends the task without touching the DSH session.
    run.cmd.send(AgentCommand::Shutdown).unwrap();
    let exited = run
        .until(|o| match o {
            AgentOutput::Exited { error, .. } => Some(error.clone()),
            _ => None,
        })
        .await;
    assert_eq!(exited, None);
    assert!(api.calls("session.cancel").is_empty());
}

#[tokio::test]
async fn a_question_is_answered_by_the_next_message() {
    let sid = session_of(APPROVAL_QUESTION);
    let api = FakeApi::new();
    let mut run = Run::start(
        api.clone(),
        SpawnOptions {
            resume: Some(sid.clone()),
            ..Default::default()
        },
    );
    run.until(|o| matches!(o, AgentOutput::NativeId(_)).then_some(()))
        .await;
    // Resuming reads the session, which attaches it in dsh web.
    assert_eq!(api.calls("session.history")[0]["sessionId"], sid.as_str());
    let (rpc, ask) = mux(APPROVAL_QUESTION)
        .into_iter()
        .find(|(_, p)| p["type"] == "question/requested")
        .unwrap();
    run.push(&rpc, &ask);
    run.until(|o| match o {
        AgentOutput::Event(EventKind::State {
            state: SessionState::NeedsYou,
            ..
        }) => Some(()),
        _ => None,
    })
    .await;
    run.cmd
        .send(AgentCommand::Send {
            text: "2".into(),
            images: vec![],
        })
        .unwrap();
    run.until(|o| {
        matches!(
            o,
            AgentOutput::Event(EventKind::State {
                state: SessionState::Working,
                ..
            })
        )
        .then_some(())
    })
    .await;
    let answer = api.responded.lock().unwrap()[0].clone();
    assert_eq!(answer["rpcId"], rpc.as_str());
    assert_eq!(
        answer["result"]["value"],
        json!({"sessionId": sid, "answer": {"answers": [{"id": "color", "selected": ["蓝"]}]}})
    );
    // Not a new prompt.
    assert!(api.calls("session.prompt").is_empty());
}

#[tokio::test]
async fn slash_commands_go_to_dsh_first() {
    let api = FakeApi::new();
    api.reply("session.create", Ok(json!({ "sessionId": "s1" })));
    let done = response(RESUME_COMMANDS, "commands/execute", 0);
    api.reply("commands/execute", Ok(done));
    // Unknown to DSH: answered without a value, then sent as a message.
    api.reply("commands/execute", Ok(Value::Null));
    let mut run = Run::start(api.clone(), SpawnOptions::default());
    run.until(|o| matches!(o, AgentOutput::NativeId(_)).then_some(()))
        .await;
    run.cmd
        .send(AgentCommand::Send {
            text: "/permission read-only".into(),
            images: vec![],
        })
        .unwrap();
    let text = run
        .until(|o| match o {
            AgentOutput::Event(EventKind::Notice { text, .. }) => Some(text.clone()),
            _ => None,
        })
        .await;
    assert_eq!(text, "preset read-only");
    run.cmd
        .send(AgentCommand::Send {
            text: "/not-a-command".into(),
            images: vec![],
        })
        .unwrap();
    for _ in 0..100 {
        if !api.calls("session.prompt").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        api.calls("session.prompt")[0]["content"][0]["text"],
        "/not-a-command"
    );
}

#[tokio::test]
async fn errors_and_a_lost_connection() {
    let api = FakeApi::new();
    api.reply(
        "session.history",
        Err(RpcError {
            code: "session-not-found".into(),
            message: "session \"gone\" not found".into(),
        }),
    );
    let mut run = Run::start(
        api.clone(),
        SpawnOptions {
            resume: Some("gone".into()),
            ..Default::default()
        },
    );
    let error = run
        .until(|o| match o {
            AgentOutput::Exited { error, .. } => Some(error.clone()),
            _ => None,
        })
        .await;
    assert!(error.unwrap().contains("gone"));

    let api = FakeApi::new();
    api.reply("session.create", Ok(json!({ "sessionId": "s1" })));
    let mut run = Run::start(api, SpawnOptions::default());
    run.until(|o| matches!(o, AgentOutput::NativeId(_)).then_some(()))
        .await;
    run.push(
        "r1",
        &json!({"type": "approval/requested", "sessionId": "s1", "approvalId": "a1", "toolName": "pwsh"}),
    );
    run.frames
        .send(Arc::new(Frame::Down("与 DSH 的连接断开".into())))
        .unwrap();
    // Outstanding approvals are retired before Exited.
    let done = run
        .until(|o| match o {
            AgentOutput::Event(EventKind::ApprovalDone { approval, decision }) => {
                Some((approval.clone(), *decision))
            }
            _ => None,
        })
        .await;
    assert_eq!(done, ("a1".to_string(), Decision::Abort));
    let error = run
        .until(|o| match o {
            AgentOutput::Exited { error, .. } => Some(error.clone()),
            _ => None,
        })
        .await;
    assert_eq!(error.as_deref(), Some("与 DSH 的连接断开"));
}

// ------------------------------------------------------------- history, models

#[test]
fn a_saved_conversation_as_events() {
    let page = response(REPLY_TOOL_CANCEL, "session.history", 0);
    let events = crate::history_events(&page);
    assert!(
        events
            .iter()
            .enumerate()
            .all(|(i, e)| e.seq == i as u64 + 1)
    );
    let kinds: Vec<&EventKind> = events.iter().map(|e| &e.kind).collect();
    assert!(kinds.contains(&&EventKind::UserText {
        text: "ECHO please".into(),
        images: None
    }));
    assert!(kinds.contains(&&EventKind::AssistantText {
        item: "a1.1".into(),
        text: "你好，我是脚本模型。".into()
    }));
    // Stored events carry no live pieces and no states.
    assert!(!kinds.iter().any(|k| matches!(
        k,
        EventKind::AssistantDelta { .. } | EventKind::State { .. }
    )));
    // DSH's own runtime notes to the model are not the person's words.
    assert!(!kinds.iter().any(
        |k| matches!(k, EventKind::UserText { text, .. } if text.contains("runtime context"))
    ));
}

#[tokio::test]
async fn saved_conversations_newest_first() {
    let api = FakeApi::new();
    let list = response(REPLY_TOOL_CANCEL, "session.list", 0);
    let mut older = list["items"][0].clone();
    older["sessionId"] = json!("session-older");
    older["updatedAt"] = json!(1);
    let mut blank = list["items"][0].clone();
    blank["sessionId"] = json!("session-blank");
    blank["blank"] = json!(true);
    let mut child = list["items"][0].clone();
    child["sessionId"] = json!("session-child");
    child["origin"] = json!("subagent");
    let items = json!({ "items": [older, list["items"][0].clone(), blank, child] });
    api.reply("session.list", Ok(items));
    let data = crate::list_history(
        api.as_ref(),
        &HistoryQuery {
            limit: 1,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(data.threads.len(), 1);
    let t = &data.threads[0];
    assert_eq!(t.title, "脚本标题");
    assert_eq!(t.cwd, r"C:\work\demo");
    assert_eq!(t.origin.as_deref(), Some("DeepSeek Harness"));
    assert_eq!(data.next_cursor.as_deref(), Some("1"));
}

#[test]
fn models_and_permissions() {
    let models = response(RESUME_COMMANDS, "llm.models", 0);
    let list = crate::models_from(
        &models,
        &json!({"provider": "deepseek-official", "model": "deepseek-v4-flash"}),
    );
    let flash = list
        .iter()
        .find(|m| m.id == "deepseek-official/deepseek-v4-flash")
        .expect("DeepSeek's own model listed");
    assert!(flash.is_default);
    assert_eq!(flash.name, "DeepSeek-V4-Flash");
    assert_eq!(flash.description.as_deref(), Some("DeepSeek"));
    assert!(flash.efforts.contains(&"max".to_string()));
    assert_eq!(flash.default_effort.as_deref(), Some("high"));
    assert_eq!(list.iter().filter(|m| m.is_default).count(), 1);

    assert_eq!(
        crate::default_preset("ui:\n  x: 1\npermissionPresets:\n  defaultPreset: read-only\n"),
        Some("read-only".into())
    );
    assert_eq!(crate::default_preset("defaultPreset: nope\n"), None);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("settings.yaml"),
        "permissionPresets:\n  defaultPreset: danger-full-access\n",
    )
    .unwrap();
    let p = crate::permissions(Some(dir.path()));
    assert_eq!(p.default.as_deref(), Some("danger-full-access"));
    assert_eq!(
        p.list.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
        vec!["read-only", "workspace-write", "danger-full-access"]
    );
    assert_eq!(
        crate::permissions(None).default.as_deref(),
        Some("workspace-write")
    );
}

// ------------------------------------------------------------- live

/// Against a real `dsh web`: `TETHER_DSH_URL=http://127.0.0.1:PORT cargo test
/// -p tether-adapter-dsh -- --ignored live`. Sends one message, so the
/// `dsh web` should run a cheap or scripted model (`TETHER_DSH_LIVE_MODEL`
/// picks one, as `provider/model`). DSH 0.2 needs the whole URL it printed,
/// `?token=…` included.
#[tokio::test]
#[ignore = "needs a running dsh web (TETHER_DSH_URL)"]
async fn live_round_trip() {
    use tether_agent_api::testkit::Driver;
    assert!(
        std::env::var_os(crate::URL_ENV).is_some(),
        "set {} to a running dsh web",
        crate::URL_ENV
    );
    let adapter = crate::adapter(crate::DshAgent::default_agent());
    let models = adapter
        .list_models(Default::default())
        .await
        .expect("models");
    assert!(!models.is_empty());
    let dir = tempfile::tempdir().unwrap();
    let mut d = Driver::spawn(
        adapter.as_ref(),
        SpawnOptions {
            cwd: dir.path().to_path_buf(),
            model: std::env::var("TETHER_DSH_LIVE_MODEL").ok(),
            ..Default::default()
        },
    )
    .unwrap();
    d.wait_for(
        |o| matches!(o, AgentOutput::NativeId(_)).then_some(()),
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    d.send(AgentCommand::Send {
        text: std::env::var("TETHER_DSH_LIVE_PROMPT").unwrap_or_else(|_| "hi".into()),
        images: vec![],
    })
    .unwrap();
    // Approvals are allowed once; a question is answered with its first
    // option, the way a reply typed on the phone would.
    enum Next {
        Approve(String),
        Reply,
        Done,
    }
    loop {
        let next = d
            .wait_for(
                |o| match o {
                    AgentOutput::Event(EventKind::Approval { approval }) => {
                        Some(Next::Approve(approval.id.clone()))
                    }
                    AgentOutput::Event(EventKind::State {
                        state: SessionState::NeedsYou,
                        status_line: Some(_),
                    }) => Some(Next::Reply),
                    AgentOutput::Event(EventKind::TurnDone { .. }) => Some(Next::Done),
                    _ => None,
                },
                Duration::from_secs(120),
            )
            .await
            .unwrap();
        match next {
            Next::Approve(id) => d
                .send(AgentCommand::Answer {
                    approval: id,
                    decision: Decision::AllowOnce,
                })
                .unwrap(),
            Next::Reply => d
                .send(AgentCommand::Send {
                    text: "1".into(),
                    images: vec![],
                })
                .unwrap(),
            Next::Done => break,
        }
    }
    for o in &d.outputs {
        eprintln!("{o:?}");
    }
    assert!(
        d.outputs
            .iter()
            .any(|o| matches!(o, AgentOutput::Event(EventKind::AssistantText { .. })))
    );
    let history = adapter
        .list_history(Default::default())
        .await
        .expect("history");
    assert!(!history.threads.is_empty());
}

/// Starts `dsh web` itself: `TETHER_DSH=<path to dsh> cargo test -p
/// tether-adapter-dsh -- --ignored live_launch`. Reads models and saved
/// conversations only; sends nothing to a model.
#[tokio::test]
#[ignore = "starts the dsh named by TETHER_DSH"]
async fn live_launch() {
    assert!(
        std::env::var_os(crate::COMMAND_ENV).is_some(),
        "set TETHER_DSH"
    );
    let adapter = crate::adapter(crate::DshAgent::default_agent());
    let info = adapter.info();
    eprintln!("{info:?}");
    assert!(info.available);
    let models = adapter
        .list_models(Default::default())
        .await
        .expect("models");
    eprintln!(
        "{} models, default {:?}",
        models.len(),
        models.iter().find(|m| m.is_default).map(|m| &m.id)
    );
    let history = adapter
        .list_history(Default::default())
        .await
        .expect("history");
    eprintln!("{} saved conversations", history.threads.len());
    for t in history.threads.iter().take(3) {
        eprintln!("  {} {} {}", t.updated_at, t.title, t.cwd);
    }
}

#[tokio::test]
async fn providers_without_a_key() {
    let api = FakeApi::new();
    api.reply(
        "llm.providers",
        Ok(json!({"providers": [
            {"provider": "deepseek-official", "displayName": "DeepSeek", "settingsNs": "llm-deepseek", "settingsPath": [], "active": true},
            {"provider": "scripted", "displayName": "scripted", "settingsNs": "llm-pi-ai", "settingsPath": ["providers", "scripted"], "active": true},
            {"provider": "anthropic", "displayName": "anthropic", "settingsNs": "llm-pi-ai", "settingsPath": ["providers", "anthropic"], "active": false}
        ]})),
    );
    api.reply(
        "settings.describe",
        Ok(json!({"namespaces": [
            {"ns": "llm-deepseek", "value": {"apiKeyEnv": "DEEPSEEK_API_KEY"}},
            {"ns": "llm-pi-ai", "value": {"providers": {"scripted": {"apiKeyEnv": "SCRIPTED_KEY"}}}}
        ]})),
    );
    api.reply(
        "credentials.describe",
        Ok(json!({"credentials": {
            "DEEPSEEK_API_KEY": {"configured": false, "writable": true},
            "SCRIPTED_KEY": {"configured": true, "source": "env", "writable": false}
        }})),
    );
    let providers = vec!["deepseek-official".to_string(), "scripted".to_string()];
    let missing = crate::setup::missing_keys(api.as_ref(), &providers).await;
    assert_eq!(missing.len(), 1);
    assert_eq!(missing["deepseek-official"].env, "DEEPSEEK_API_KEY");
    assert_eq!(
        api.calls("credentials.describe"),
        vec![json!({"refs": ["DEEPSEEK_API_KEY", "SCRIPTED_KEY"]})]
    );

    let model = |id: &str, default: bool| tether_proto::ModelInfo {
        id: id.into(),
        name: id.into(),
        description: None,
        efforts: Vec::new(),
        default_effort: None,
        is_default: default,
        speeds: Vec::new(),
        default_speed: None,
    };
    // One provider can answer: the others are marked.
    let list = crate::setup::usable(
        vec![
            model("deepseek-official/deepseek-v4-flash", true),
            model("scripted/scripted-1", false),
        ],
        &missing,
        &[],
    )
    .unwrap();
    assert!(
        list[0]
            .description
            .as_deref()
            .unwrap()
            .contains("DEEPSEEK_API_KEY")
    );
    assert_eq!(list[1].description, None);
    // None can: the phone is told to set DSH up on the computer.
    let error = crate::setup::usable(
        vec![model("deepseek-official/deepseek-v4-flash", true)],
        &missing,
        &[],
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("DeepSeek 的 API Key（DEEPSEEK_API_KEY）还没有配置"),
        "{error}"
    );
    assert!(error.contains("在电脑上"));
    assert!(error.contains("web 配置"), "{error}");
    // Nothing listed: DSH's own reasons are passed on (0.2 `failures`).
    let failed = crate::setup::failures(&json!({
        "groups": [],
        "failures": [{ "id": "chatecnu", "name": "ChatECNU", "message": "no credential for provider route" }]
    }));
    assert_eq!(
        failed,
        vec!["ChatECNU：no credential for provider route".to_string()]
    );
    let error = crate::setup::usable(Vec::new(), &missing, &failed)
        .unwrap_err()
        .to_string();
    assert!(error.contains("还没有可用的模型"), "{error}");
    assert!(error.contains("ChatECNU：no credential"), "{error}");
    assert!(crate::setup::failures(&json!({ "groups": [] })).is_empty());
}

#[test]
fn missing_credential_turns() {
    let mut tr = Translator::live(None);
    let out = tr.event(&json!({
        "type": "turn/end", "seq": 15, "time": 1,
        "data": {"turn": 1, "reason": {"kind": "error", "error": {
            "message": "llm-deepseek: no API key for provider route \"deepseek-official\"; store DEEPSEEK_API_KEY through the credentials service (the web Models page writes it), or export DEEPSEEK_API_KEY in the launching environment",
            "code": "MISSING_CREDENTIAL"
        }}}
    }));
    let text = out
        .iter()
        .find_map(|k| match k {
            EventKind::Notice { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        text.contains("deepseek-official 的 API Key（DEEPSEEK_API_KEY）还没有配置"),
        "{text}"
    );
    assert!(text.contains("dsh web"));
}

#[test]
fn a_saved_launcher_applies_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let launcher = dir
        .path()
        .join(if cfg!(windows) { "dsh.cmd" } else { "dsh" });
    std::fs::write(&launcher, "").unwrap();
    let agent = crate::adapter(crate::DshAgent {
        id: "dsh".into(),
        name: "DeepSeek Harness".into(),
        command: vec!["tether-no-such-dsh-xyz".into()],
        auto: true,
    });
    assert!(!agent.info().available);
    crate::set_saved_launcher(Some(launcher.clone()));
    let info = agent.info();
    crate::set_saved_launcher(Some(dir.path().join("gone").join("dsh.cmd")));
    let gone = agent.info();
    crate::set_saved_launcher(None);
    #[cfg(windows)]
    assert!(info.available, "{info:?}");
    assert!(!info.terminal_command.is_empty());
    assert!(!gone.available);
    assert!(gone.detail.unwrap().contains("已经不存在"));
    assert!(agent.info().detail.unwrap().contains("填写 dsh.cmd 的位置"));
}

/// Checks a pasted address the way the desktop app does:
/// `TETHER_DSH_URL=<dsh web URL> cargo test -p tether-adapter-dsh --
/// --ignored live_check_url`. Opens no session.
#[tokio::test]
#[ignore = "needs a running dsh web in TETHER_DSH_URL"]
async fn live_check_url() {
    let url = std::env::var(crate::URL_ENV).expect("set TETHER_DSH_URL");
    let line = format!("dsh web: {url}");
    assert_eq!(crate::check_url(&line).await.unwrap(), url);
    let wrong = match url.split_once("token=") {
        Some((head, _)) => format!("{head}token=wrong"),
        None => return,
    };
    let error = format!("{:#}", crate::check_url(&wrong).await.unwrap_err());
    eprintln!("{error}");
    assert!(error.contains("没有接受"), "{error}");
}

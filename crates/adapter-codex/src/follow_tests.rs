use super::*;
use tokio::{
    io::{DuplexStream, WriteHalf},
    sync::mpsc,
    task::JoinHandle,
};

const T: Duration = Duration::from_secs(5);
const THREAD: &str = "01a1bf37-26ff-7040-8161-fe2910fb9d02";
const OWNER: &str = "1d9581c1-5072-4d2f-ad38-89d1d7ac477a";
const SENT: &str = "测试：手机经桌面版发送。请只回复“收到”两个字。";
const TURN: &str = "01a1c0c4-96ee-7930-a881-b32cbaa7e151";
const FIXTURE: &str = include_str!("../../../fixtures/codex/desktop-turn.jsonl");

struct Desktop {
    rx: Receiver<anyhow::Result<Value>>,
    writer: WriteHalf<DuplexStream>,
}

impl Desktop {
    fn new(stream: DuplexStream) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            rx: bus::frames(reader),
            writer,
        }
    }

    async fn next(&mut self) -> Value {
        timeout(T, self.rx.recv())
            .await
            .expect("desktop request timed out")
            .expect("follower disconnected")
            .unwrap()
    }

    async fn send(&mut self, frame: Value) {
        timeout(T, bus::write(&mut self.writer, &frame))
            .await
            .unwrap()
            .unwrap();
    }

    async fn response(&mut self, request: &Value, result: Value) {
        self.send(json!({"type":"response","requestId":request["requestId"],
            "resultType":"success","result":result,"handledByClientId":OWNER}))
            .await;
    }

    async fn error(&mut self, request: &Value, error: Value) {
        self.send(json!({"type":"response","requestId":request["requestId"],
            "resultType":"error","error":error}))
            .await;
    }

    async fn discover(&mut self) -> Value {
        let init = self.next().await;
        assert_eq!(init["sourceClientId"], bus::UNINITIALIZED);
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["version"], 0);
        assert_eq!(init["params"], json!({"clientType":"tether"}));
        self.send(json!({"type":"client-discovery-request","requestId":"during-init"}))
            .await;
        assert_eq!(
            self.next().await,
            bus::decline(&json!({"requestId":"during-init"}))
        );
        self.response(&init, json!({"clientId":"phone"})).await;
        let owner = self.next().await;
        assert_eq!(owner["sourceClientId"], "phone");
        assert_eq!(owner["method"], "thread-follower-read-model-settings");
        assert_eq!(owner["version"], 1);
        assert_eq!(owner["params"], json!({"conversationId":THREAD}));
        owner
    }

    async fn no_request(&mut self) {
        assert!(
            timeout(Duration::from_millis(40), self.rx.recv())
                .await
                .is_err()
        );
    }
}

struct Harness {
    desktop: Desktop,
    cmd: UnboundedSender<AgentCommand>,
    rx: UnboundedReceiver<AgentOutput>,
    outputs: Vec<AgentOutput>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    async fn new(opts: SpawnOptions) -> Self {
        let (a, b) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(a);
        let (cmd, commands) = mpsc::unbounded_channel();
        let (out, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let link = owned(reader, writer, THREAD).await?.expect("owner");
            run(link, THREAD, &opts, commands, &out).await
        });
        let mut h = Self {
            desktop: Desktop::new(b),
            cmd,
            rx,
            outputs: Vec::new(),
            task,
        };
        let owner = h.desktop.discover().await;
        h.desktop.response(&owner, json!({})).await;
        h.following(true).await;
        h
    }

    async fn following(&mut self, following: bool) {
        assert_eq!(
            self.desktop.next().await,
            bus::broadcast(
                "phone",
                "thread-stream-following-changed",
                1,
                json!({"conversationId":THREAD,"hostId":"local","following":following})
            )
        );
    }

    async fn snapshot(&mut self, state: Value, revision: u64) {
        self.desktop
            .send(changed(
                json!({"type":"snapshot","revision":revision,"conversationState":state}),
            ))
            .await;
    }

    async fn ready(&mut self, state: Value) {
        self.snapshot(state, 1).await;
        self.wait(|o| matches!(o, AgentOutput::NativeId(id) if id == THREAD))
            .await;
    }

    async fn wait(&mut self, until: impl Fn(&AgentOutput) -> bool) {
        let deadline = Instant::now() + T;
        loop {
            let output = timeout_at(deadline, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("outputs: {:?}", self.outputs))
                .expect("output closed");
            let done = until(&output);
            self.outputs.push(output);
            if done {
                return;
            }
        }
    }

    async fn state(&mut self, state: SessionState) {
        self.wait(
            |o| matches!(o.event(), Some(EventKind::State {state: got, ..}) if *got == state),
        )
        .await;
    }

    fn send(&self, text: &str) {
        self.cmd
            .send(AgentCommand::Send {
                text: text.into(),
                images: Vec::new(),
            })
            .unwrap();
    }

    async fn start(&mut self, text: &str) -> Value {
        let request = self.desktop.next().await;
        assert_eq!(request["method"], "thread-follower-start-turn");
        assert_eq!(request["version"], 2);
        assert_eq!(request["sourceClientId"], "phone");
        assert_eq!(request["params"]["conversationId"], THREAD);
        assert_eq!(request["params"]["turnStart"]["context"], json!({}));
        assert_eq!(
            request["params"]["turnStart"]["request"]["input"],
            json!([{"type":"text","text":text,"text_elements":[]}])
        );
        assert!(uuid::Uuid::parse_str(request["requestId"].as_str().unwrap()).is_ok());
        request
    }

    async fn started(&mut self, request: &Value, turn: &str) {
        self.desktop
            .response(request, json!({"result":{"turn":{"id":turn}}}))
            .await;
    }

    async fn stop(mut self) {
        self.cmd.send(AgentCommand::Shutdown).unwrap();
        self.following(false).await;
        timeout(T, self.task).await.unwrap().unwrap().unwrap();
    }

    fn events(&self) -> Vec<EventKind> {
        self.outputs
            .iter()
            .filter_map(|o| o.event().cloned())
            .collect()
    }
}

fn changed(change: Value) -> Value {
    bus::broadcast(
        OWNER,
        "thread-stream-state-changed",
        11,
        json!({"conversationId":THREAD,"hostId":"local","change":change}),
    )
}

fn base() -> Value {
    json!({"requests":[],"turnHistory":{"history":{"entitiesByKey":{}}}})
}

fn turn_state(turn: Value, status: &str) -> Value {
    let mut state = base();
    state["turnHistory"]["history"]["entitiesByKey"]["tail:1"] =
        json!({"turnId":turn,"status":status,"turnStartedAtMs":1,"items":[]});
    state
}

fn approval_state(file: bool) -> Value {
    let mut state = turn_state(json!("u1"), "inProgress");
    state["requests"] = json!([{"id":if file { json!("file-7") } else { json!(7) },
        "method":if file { mirror::FILE_APPROVAL } else { mirror::COMMAND_APPROVAL },
        "params":{"threadId":THREAD,"turnId":"u1","itemId":"tool-1","command":"cargo test","reason":"tests"}}]);
    state
}

#[tokio::test]
async fn real_recording_matches_mirror_events_without_echoing_the_phone() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    let frames: Vec<Value> = FIXTURE
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    h.desktop.send(frames[0].clone()).await;
    h.wait(|o| matches!(o, AgentOutput::NativeId(_))).await;
    h.send(SENT);
    let request = h.start(SENT).await;
    h.started(&request, TURN).await;
    for frame in &frames[1..] {
        h.desktop.send(frame.clone()).await;
    }
    h.state(SessionState::Idle).await;
    assert_eq!(
        h.events(),
        vec![
            translate::state(SessionState::Working),
            EventKind::Thinking {
                item: "rs_0067737af50d29e4016af1dc55221887d0aaf1effc4b60c58b".into(),
                text: String::new()
            },
            EventKind::AssistantDelta {
                item: "msg_0067737af50d29e4016af1dc55da7087d0a5c49ee84ffb0571".into(),
                text: "收到".into()
            },
            EventKind::AssistantText {
                item: "msg_0067737af50d29e4016af1dc55da7087d0a5c49ee84ffb0571".into(),
                text: "收到".into()
            },
            EventKind::TurnDone { summary: None },
            translate::state(SessionState::Idle),
        ]
    );
    h.stop().await;
}

#[tokio::test]
async fn recording_initiated_on_the_computer_emits_user_text() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    for line in FIXTURE.lines() {
        h.desktop.send(serde_json::from_str(line).unwrap()).await;
    }
    h.state(SessionState::Idle).await;
    assert_eq!(
        h.events()
            .iter()
            .filter(|e| matches!(e, EventKind::UserText { .. }))
            .cloned()
            .collect::<Vec<_>>(),
        vec![EventKind::UserText {
            text: SENT.into(),
            images: None
        }]
    );
    h.stop().await;
}

#[tokio::test]
async fn no_owner_returns_none_and_never_follows() {
    let (a, b) = tokio::io::duplex(4096);
    let (r, w) = tokio::io::split(a);
    let task = tokio::spawn(owned(r, w, THREAD));
    let mut desktop = Desktop::new(b);
    let owner = desktop.discover().await;
    desktop.error(&owner, json!("no-client-found")).await;
    assert!(timeout(T, task).await.unwrap().unwrap().unwrap().is_none());
    // The frame-reading task must release its half even when no more bytes arrive.
    assert!(timeout(T, desktop.rx.recv()).await.unwrap().is_none());
}

#[tokio::test]
async fn initialization_holds_broadcasts_and_declines_discovery() {
    let (a, b) = tokio::io::duplex(4096);
    let (r, w) = tokio::io::split(a);
    let task = tokio::spawn(owned(r, w, THREAD));
    let mut desktop = Desktop::new(b);
    let owner = desktop.discover().await;
    let frame = changed(json!({"type":"snapshot","revision":1,"conversationState":base()}));
    desktop.send(frame.clone()).await;
    desktop
        .send(json!({"type":"client-discovery-request","requestId":"during-owner"}))
        .await;
    assert_eq!(desktop.next().await["response"]["canHandle"], false);
    desktop.response(&owner, json!({})).await;
    let mut link = timeout(T, task).await.unwrap().unwrap().unwrap().unwrap();
    assert_eq!(link.next().await.unwrap(), frame);
    assert_eq!(link.owner, OWNER);
}

#[tokio::test]
async fn announcing_lists_the_thread_as_unarchived_without_following() {
    let (a, b) = tokio::io::duplex(4096);
    let (r, w) = tokio::io::split(a);
    let task = tokio::spawn(async move { announce(r, w, THREAD).await });
    let mut desktop = Desktop::new(b);
    let init = desktop.next().await;
    assert_eq!(init["method"], "initialize");
    desktop
        .send(
            json!({"type":"response","requestId":init["requestId"],"resultType":"success",
            "result":{"clientId":"me"}}),
        )
        .await;
    let frame = desktop.next().await;
    assert_eq!(
        frame,
        json!({"type":"broadcast","method":"thread-unarchived","sourceClientId":"me","version":1,
            "params":{"hostId":"local","conversationId":THREAD}})
    );
    timeout(T, task).await.unwrap().unwrap().unwrap();
}

#[tokio::test]
async fn pipe_closure_ends_with_the_reconnect_message() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(base()).await;
    drop(h.desktop);
    let error = timeout(T, h.task).await.unwrap().unwrap().unwrap_err();
    assert_eq!(error.to_string(), bus::CLOSED_TEXT);
}

#[tokio::test]
async fn a_newer_state_version_says_the_app_was_updated() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(base()).await;
    let mut newer = changed(json!({"type":"snapshot","revision":2,"conversationState":base()}));
    newer["version"] = json!(12);
    h.desktop.send(newer).await;
    let error = timeout(T, h.task).await.unwrap().unwrap().unwrap_err();
    assert_eq!(error.to_string(), bus::UPDATED_TEXT);
    assert!(error.to_string().contains("分叉成新对话"));
}

#[tokio::test]
async fn only_the_owners_disconnect_ends_the_session() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(base()).await;
    h.desktop
        .send(bus::broadcast(
            "router",
            "client-status-changed",
            0,
            json!({"clientId":"someone-else","status":"disconnected"}),
        ))
        .await;
    h.desktop.no_request().await;
    assert!(!h.task.is_finished());
    h.desktop
        .send(bus::broadcast(
            "router",
            "client-status-changed",
            0,
            json!({"clientId":OWNER,"status":"disconnected"}),
        ))
        .await;
    h.following(false).await;
    assert_eq!(
        h.task.await.unwrap().unwrap_err().to_string(),
        bus::CLOSED_TEXT
    );
}

#[tokio::test]
async fn command_and_file_approvals_keep_the_original_request_id() {
    for file in [false, true] {
        let mut h = Harness::new(SpawnOptions::default()).await;
        h.ready(approval_state(file)).await;
        h.state(SessionState::NeedsYou).await;
        let approval = if file { "file-7" } else { "7" };
        h.cmd
            .send(AgentCommand::Answer {
                approval: approval.into(),
                decision: Decision::AllowSession,
            })
            .unwrap();
        let request = h.desktop.next().await;
        assert_eq!(
            request["method"],
            if file {
                "thread-follower-file-approval-decision"
            } else {
                "thread-follower-command-approval-decision"
            }
        );
        assert_eq!(request["version"], 1);
        assert_eq!(
            request["params"],
            json!({"conversationId":THREAD,"requestId":if file {json!(approval)} else {json!(7)},"decision":"acceptForSession"})
        );
        h.desktop.response(&request, json!({})).await;
        h.state(SessionState::Working).await;
        h.snapshot(turn_state(json!("u1"), "completed"), 2).await;
        h.state(SessionState::Idle).await;
        assert_eq!(
            h.events()
                .iter()
                .filter(|e| matches!(e, EventKind::ApprovalDone { .. }))
                .count(),
            1
        );
        h.cmd
            .send(AgentCommand::Answer {
                approval: approval.into(),
                decision: Decision::Deny,
            })
            .unwrap();
        h.wait(|o| matches!(o.event(),Some(EventKind::Notice {level:NoticeLevel::Warn,text}) if text == "审批已结束或不存在")).await;
        h.desktop.no_request().await;
        h.stop().await;
    }
}

#[tokio::test]
async fn sends_during_a_turn_are_fifo_not_steers() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(turn_state(json!("u1"), "inProgress")).await;
    h.state(SessionState::Working).await;
    h.send("two");
    h.send("three");
    h.state(SessionState::Working).await;
    h.state(SessionState::Working).await;
    h.desktop.no_request().await;
    h.snapshot(turn_state(json!("u1"), "completed"), 2).await;
    let two = h.start("two").await;
    h.started(&two, "u2").await;
    // An ACK arriving before the state broadcast must not drain "three".
    h.desktop.no_request().await;
    h.snapshot(turn_state(json!("u2"), "inProgress"), 3).await;
    h.desktop.no_request().await;
    h.snapshot(turn_state(json!("u2"), "completed"), 4).await;
    let three = h.start("three").await;
    assert_ne!(two["requestId"], three["requestId"]);
    h.stop().await;
}

#[tokio::test]
async fn completion_before_the_start_reply_does_not_lose_the_next_message() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(base()).await;
    h.send("one");
    let one = h.start("one").await;
    h.send("two");
    h.snapshot(turn_state(json!("u1"), "inProgress"), 2).await;
    h.snapshot(turn_state(json!("u1"), "completed"), 3).await;
    h.state(SessionState::Idle).await;
    h.desktop.no_request().await;
    h.started(&one, "u1").await;
    h.start("two").await;
    h.stop().await;
}

#[tokio::test]
async fn interrupt_aborts_approvals_and_discards_queued_messages() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(approval_state(false)).await;
    h.state(SessionState::NeedsYou).await;
    h.send("discard me");
    h.cmd.send(AgentCommand::Interrupt).unwrap();
    let request = h.desktop.next().await;
    assert_eq!(request["method"], "thread-follower-interrupt-turn");
    assert_eq!(request["version"], 4);
    assert_eq!(
        request["params"],
        json!({"conversationId":THREAD,"mode":"user-stop","expectedTurnId":"u1"})
    );
    h.desktop.response(&request, json!({})).await;
    h.snapshot(turn_state(json!("u1"), "interrupted"), 2).await;
    h.state(SessionState::Idle).await;
    assert!(h.events().contains(&EventKind::ApprovalDone {
        approval: "7".into(),
        decision: Decision::Abort
    }));
    h.desktop.no_request().await;
    h.stop().await;
}

#[tokio::test]
async fn interrupt_before_turn_id_waits_for_start_response() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(base()).await;
    h.send("one");
    let start = h.start("one").await;
    h.cmd.send(AgentCommand::Interrupt).unwrap();
    h.desktop.no_request().await;
    h.started(&start, "u1").await;
    let interrupt = h.desktop.next().await;
    assert_eq!(interrupt["method"], "thread-follower-interrupt-turn");
    assert_eq!(interrupt["params"]["expectedTurnId"], "u1");
    h.stop().await;
}

#[tokio::test]
async fn interrupt_gets_turn_id_from_a_patch_before_the_reply() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(turn_state(Value::Null, "inProgress")).await;
    h.cmd.send(AgentCommand::Interrupt).unwrap();
    h.desktop.no_request().await;
    h.snapshot(turn_state(json!("u1"), "inProgress"), 2).await;
    assert_eq!(h.desktop.next().await["params"]["expectedTurnId"], "u1");
    h.stop().await;
}

#[tokio::test]
async fn stale_patches_pause_sends_until_a_recovery_snapshot() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(base()).await;
    h.desktop
        .send(changed(
            json!({"type":"patches","baseRevision":99,"revision":100,"patches":[]}),
        ))
        .await;
    h.following(true).await;
    h.send("queued");
    h.state(SessionState::Working).await;
    h.desktop.no_request().await;
    h.snapshot(base(), 101).await;
    h.start("queued").await;
    h.stop().await;
}

#[tokio::test]
async fn early_patches_and_other_threads_do_not_initialize_the_mirror() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.desktop
        .send(changed(
            json!({"type":"patches","baseRevision":0,"revision":1,"patches":[]}),
        ))
        .await;
    let mut other = changed(json!({"type":"snapshot","revision":1,"conversationState":base()}));
    other["params"]["conversationId"] = json!("other");
    h.desktop.send(other).await;
    h.send("queued");
    h.state(SessionState::Working).await;
    h.desktop.no_request().await;
    h.ready(base()).await;
    h.start("queued").await;
    h.stop().await;
}

#[tokio::test]
async fn settings_are_forwarded_and_none_keeps_the_previous_value() {
    let mut h = Harness::new(SpawnOptions {
        model: Some("old".into()),
        effort: Some("high".into()),
        speed: Some("priority".into()),
        ..SpawnOptions::default()
    })
    .await;
    h.ready(base()).await;
    h.cmd
        .send(AgentCommand::SetModel {
            model: Some("new".into()),
            effort: None,
            speed: Some("default".into()),
            permission: Some(":workspace".into()),
        })
        .unwrap();
    h.wait(|o| matches!(o.event(),Some(EventKind::Notice {text,..}) if text == "下一条消息起使用 new（思考强度：high，速度：标准，权限：默认权限）")).await;
    h.send("one");
    let start = h.start("one").await;
    let request = &start["params"]["turnStart"]["request"];
    assert_eq!(request["model"], "new");
    assert_eq!(request["effort"], "high");
    assert_eq!(request["serviceTier"], "default");
    assert_eq!(request["permissions"], ":workspace");
    h.stop().await;
}

#[tokio::test]
async fn failed_start_reports_error_then_idle_and_does_not_hide_computer_text() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(base()).await;
    h.send("same");
    let start = h.start("same").await;
    h.desktop.error(&start, json!({"message":"denied"})).await;
    h.state(SessionState::Idle).await;
    assert!(
        h.events()
            .contains(&translate::notice(NoticeLevel::Error, "denied"))
    );
    let mut state = turn_state(json!("computer"), "inProgress");
    state["turnHistory"]["history"]["entitiesByKey"]["tail:1"]["items"] = json!([
        {"type":"userMessage","id":"user-1","content":[{"type":"text","text":"same"}]}]);
    h.snapshot(state, 2).await;
    h.wait(|o| matches!(o.event(),Some(EventKind::UserText {text, ..}) if text == "same"))
        .await;
    h.stop().await;
}

#[tokio::test]
async fn pending_requests_get_the_same_disconnect_error() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.ready(base()).await;
    h.send("one");
    let start = h.start("one").await;
    h.desktop.error(&start, json!("client-disconnected")).await;
    h.following(false).await;
    assert_eq!(
        h.task.await.unwrap().unwrap_err().to_string(),
        bus::CLOSED_TEXT
    );
}

#[tokio::test]
async fn closing_command_or_output_channels_unfollows_even_before_snapshot() {
    for close_output in [false, true] {
        let mut h = Harness::new(SpawnOptions::default()).await;
        if close_output {
            h.rx.close();
        } else {
            let (replacement, _rx) = mpsc::unbounded_channel();
            drop(std::mem::replace(&mut h.cmd, replacement));
        }
        h.following(false).await;
        timeout(T, h.task).await.unwrap().unwrap().unwrap();
    }
}

#[tokio::test]
async fn interrupt_cancels_a_message_queued_before_the_idle_snapshot() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    h.send("discard me");
    h.cmd.send(AgentCommand::Interrupt).unwrap();
    h.state(SessionState::Working).await;
    h.desktop.no_request().await;
    h.ready(base()).await;
    h.state(SessionState::Idle).await;
    h.desktop.no_request().await;
    h.stop().await;
}

#[tokio::test]
async fn initialization_times_out_without_a_response() {
    let (a, b) = tokio::io::duplex(4096);
    let (r, w) = tokio::io::split(a);
    let task = tokio::spawn(owned(r, w, THREAD));
    let mut desktop = Desktop::new(b);
    assert_eq!(desktop.next().await["method"], "initialize");
    let result = timeout(TIMEOUT + T, task).await.unwrap().unwrap();
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("initialize timed out")
    );
}

#[tokio::test]
async fn first_snapshot_times_out_and_unfollows() {
    let mut h = Harness::new(SpawnOptions::default()).await;
    let result = timeout(TIMEOUT + T, &mut h.task).await.unwrap().unwrap();
    assert_eq!(
        result.unwrap_err().to_string(),
        "等待 Codex 桌面版对话快照超时"
    );
    h.following(false).await;
}

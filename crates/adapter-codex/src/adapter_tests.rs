use super::*;
use tether_agent_api::{
    AgentAdapter,
    testkit::{LineEnd, line_duplex},
};
use tokio::{task::JoinHandle, time::timeout};

const T: Duration = Duration::from_secs(5);
const QA: &str = include_str!("../../../fixtures/codex/qa.jsonl");
const COMMAND_APPROVAL: &str = include_str!("../../../fixtures/codex/command-approval.jsonl");
const FILE_APPROVAL: &str = include_str!("../../../fixtures/codex/file-approval.jsonl");
const INTERRUPT: &str = include_str!("../../../fixtures/codex/interrupt.jsonl");
const ERROR: &str = include_str!("../../../fixtures/codex/error.jsonl");
const TOOLS: &str = include_str!("../../../fixtures/codex/tools.jsonl");

#[test]
fn only_existing_threads_can_be_open_in_the_app() {
    let direct = crate::CodexAdapter::default();
    assert!(!(direct.via_desktop)());
    let mut opts = SpawnOptions::default();
    assert_eq!(desktop_thread(&opts), None);
    opts.resume = Some("existing".into());
    assert_eq!(desktop_thread(&opts), Some("existing"));
    opts.fork = true;
    assert_eq!(desktop_thread(&opts), None);
    assert_eq!(
        crate::adapter_with(|| true).release_after(),
        Some(Duration::from_secs(5))
    );
}

type Connection = anyhow::Result<(crate::bus::Reader, crate::bus::Writer)>;

/// A bus whose app answers `initialize` and the owner query with `owner`.
fn fake_app(owner: Value) -> (Connection, JoinHandle<()>) {
    let (agent, mut peer) = tokio::io::duplex(4096);
    let (r, w) = tokio::io::split(agent);
    let task = tokio::spawn(async move {
        for method in ["initialize", "thread-follower-read-model-settings"] {
            let Ok(Some(request)) = crate::bus::read(&mut peer).await else {
                return;
            };
            assert_eq!(request["method"], method);
            let mut reply = if method == "initialize" {
                json!({"resultType":"success","result":{"clientId":"phone"}})
            } else {
                owner.clone()
            };
            reply["type"] = json!("response");
            reply["requestId"] = request["requestId"].clone();
            crate::bus::write(&mut peer, &reply).await.unwrap();
        }
    });
    (Ok((Box::new(r), Box::new(w))), task)
}

async fn route(
    resume: Option<&str>,
    fork: bool,
    via: fn() -> bool,
    connection: Connection,
) -> anyhow::Result<(Option<i32>, Option<String>)> {
    let (_cmd, rx) = mpsc::unbounded_channel();
    let (out, _events) = mpsc::unbounded_channel();
    let opts = SpawnOptions {
        resume: resume.map(str::to_owned),
        fork,
        program: Some("tether-missing-direct-executable".into()),
        ..Default::default()
    };
    process_with(opts, rx, &out, via, || async move { connection }).await
}

fn ran_directly(result: anyhow::Result<(Option<i32>, Option<String>)>) -> bool {
    result
        .unwrap_err()
        .to_string()
        .contains("find codex executable")
}

#[tokio::test]
async fn without_the_app_open_the_user_is_told_to_open_it() {
    for (resume, fork) in [(None, false), (Some("t"), false), (Some("t"), true)] {
        let result = route(resume, fork, || true, Err(anyhow::anyhow!("no pipe"))).await;
        assert_eq!(
            result.unwrap(),
            (None, Some(crate::bus::NOT_OPEN_TEXT.to_owned()))
        );
    }
}

#[tokio::test]
async fn threads_the_app_does_not_have_run_directly_while_it_is_open() {
    for (resume, fork) in [(None, false), (Some("t"), true)] {
        let (connection, app) = fake_app(json!({}));
        assert!(ran_directly(route(resume, fork, || true, connection).await));
        app.abort();
    }
    let (connection, app) = fake_app(json!({"resultType":"error","error":"no-client-found"}));
    assert!(ran_directly(
        route(Some("t"), false, || true, connection).await
    ));
    app.await.unwrap();
}

#[tokio::test]
async fn an_app_that_answers_differently_was_updated() {
    let (connection, app) = fake_app(json!({"resultType":"error","error":"unsupported-version"}));
    assert_eq!(
        route(Some("t"), false, || true, connection).await.unwrap(),
        (None, Some(crate::bus::UPDATED_TEXT.to_owned()))
    );
    app.await.unwrap();
}

#[tokio::test]
async fn an_app_that_does_not_answer_is_reported() {
    let (agent, _peer) = tokio::io::duplex(4096);
    let (r, w) = tokio::io::split(agent);
    let connection: Connection = Ok((Box::new(r), Box::new(w)));
    assert_eq!(
        route(Some("t"), false, || true, connection).await.unwrap(),
        (None, Some(crate::bus::NO_ANSWER_TEXT.to_owned()))
    );
}

#[tokio::test]
async fn direct_mode_never_connects_to_the_bus() {
    for (resume, fork) in [(None, false), (Some("t"), false), (Some("t"), true)] {
        let (_cmd, rx) = mpsc::unbounded_channel();
        let (out, _events) = mpsc::unbounded_channel();
        let opts = SpawnOptions {
            resume: resume.map(str::to_owned),
            fork,
            program: Some("tether-missing-direct-executable".into()),
            ..Default::default()
        };
        let result = process_with(
            opts,
            rx,
            &out,
            || false,
            || async {
                panic!("must not connect");
            },
        )
        .await;
        assert!(ran_directly(result));
    }
}

#[tokio::test]
async fn desktop_owned_session_does_not_launch_app_server_and_preserves_errors() {
    for closed in [false, true] {
        let (agent, mut peer) = tokio::io::duplex(4096);
        let (r, w) = tokio::io::split(agent);
        let (cmd, rx) = mpsc::unbounded_channel();
        let (out, mut events) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let opts = SpawnOptions {
                resume: Some("existing".into()),
                reveal: true,
                program: Some("tether-must-not-launch-this".into()),
                ..Default::default()
            };
            process_with(
                opts,
                rx,
                &out,
                || true,
                || async {
                    Ok((
                        Box::new(r) as crate::bus::Reader,
                        Box::new(w) as crate::bus::Writer,
                    ))
                },
            )
            .await
        });
        for result in [json!({"clientId":"phone"}), json!({})] {
            let request = crate::bus::read(&mut peer).await.unwrap().unwrap();
            crate::bus::write(&mut peer,&json!({"type":"response","requestId":request["requestId"],"resultType":"success","result":result,"handledByClientId":"owner"})).await.unwrap();
        }
        let follow = crate::bus::read(&mut peer).await.unwrap().unwrap();
        assert_eq!(follow["params"]["following"], true);
        if closed {
            drop(peer);
        } else {
            crate::bus::write(&mut peer,&crate::bus::broadcast("owner","thread-stream-state-changed",11,
                json!({"conversationId":"existing","hostId":"local","change":{"type":"snapshot","revision":1,"conversationState":{"requests":[],"turnHistory":{"history":{"entitiesByKey":{}}}}}}))).await.unwrap();
            assert!(
                matches!(timeout(T,events.recv()).await.unwrap(),Some(AgentOutput::NativeId(id)) if id == "existing")
            );
            cmd.send(AgentCommand::Shutdown).unwrap();
            assert_eq!(
                crate::bus::read(&mut peer).await.unwrap().unwrap()["params"]["following"],
                false
            );
        }
        let result = timeout(T, task).await.unwrap().unwrap().unwrap();
        assert_eq!(
            result,
            (None, closed.then(|| crate::bus::CLOSED_TEXT.to_owned()))
        );
    }
}

struct Harness {
    peer: LineEnd,
    cmd: UnboundedSender<AgentCommand>,
    rx: UnboundedReceiver<AgentOutput>,
    outputs: Vec<AgentOutput>,
    task: JoinHandle<anyhow::Result<Stop>>,
}

impl Harness {
    fn new(opts: SpawnOptions) -> Self {
        Self::with_timeout(opts, REQUEST_TIMEOUT)
    }

    fn with_timeout(opts: SpawnOptions, timeout: Duration) -> Self {
        let (agent, peer) = line_duplex();
        let (reader, writer) = agent.into_parts();
        let (cmd, mut commands) = mpsc::unbounded_channel();
        let (out, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            run_stream(
                reader,
                writer,
                &opts,
                Inbox::new(&mut commands),
                &out,
                None,
                timeout,
            )
            .await
        });
        Self {
            peer,
            cmd,
            rx,
            outputs: Vec::new(),
            task,
        }
    }

    async fn next(&mut self) -> Value {
        tokio::time::timeout(T, self.peer.next_json())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }

    async fn boot(&mut self, resume: bool) {
        assert_eq!(
            self.next().await,
            json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"tether","title":"Tether","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}})
        );
        self.peer
            .send_json(&json!({"id":1,"result":{"userAgent":"fake"}}))
            .await
            .unwrap();
        assert_eq!(self.next().await, json!({"method":"initialized"}));
        let request = self.next().await;
        assert_eq!(
            request["method"],
            if resume {
                "thread/resume"
            } else {
                "thread/start"
            }
        );
        assert_eq!(request["params"]["approvalPolicy"], "on-request");
        assert_eq!(request["params"]["sandbox"], "workspace-write");
        if resume {
            assert_eq!(request["params"]["threadId"], "thread-1");
        }
        self.peer
            .send_json(
                &json!({"id":request["id"],"result":{"thread":{"id":"thread-1","turns":[]}}}),
            )
            .await
            .unwrap();
        self.wait(|o| matches!(o, AgentOutput::NativeId(id) if id == "thread-1"))
            .await;
    }

    async fn wait(&mut self, until: impl Fn(&AgentOutput) -> bool) {
        let deadline = Instant::now() + T;
        loop {
            let output = tokio::time::timeout_at(deadline, self.rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out; outputs: {:?}", self.outputs))
                .expect("stream ended");
            let done = until(&output);
            self.outputs.push(output);
            if done {
                return;
            }
        }
    }

    async fn idle(&mut self) {
        self.wait(|o| {
            matches!(
                o.event(),
                Some(EventKind::State {
                    state: SessionState::Idle,
                    ..
                })
            )
        })
        .await;
    }

    async fn begin(&mut self) -> Value {
        self.cmd
            .send(AgentCommand::Send {
                text: "你好".into(),
                images: Vec::new(),
            })
            .unwrap();
        let request = self.next().await;
        assert_eq!(request["method"], "turn/start");
        assert_eq!(
            request["params"],
            json!({"threadId":"thread-1","input":[{"type":"text","text":"你好","text_elements":[]}]})
        );
        request
    }

    async fn stop(self) {
        self.cmd.send(AgentCommand::Shutdown).unwrap();
        assert_eq!(
            tokio::time::timeout(T, self.task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Stop::Shutdown
        );
    }

    fn names(&self) -> Vec<&'static str> {
        self.outputs
            .iter()
            .filter_map(AgentOutput::event)
            .map(|event| match event {
                EventKind::UserText { .. } => panic!("server, not adapter, owns user_text"),
                EventKind::AssistantDelta { .. } => "delta",
                EventKind::AssistantText { .. } => "text",
                EventKind::Thinking { .. } => "thinking",
                EventKind::ToolStart { .. } => "tool_start",
                EventKind::ToolEnd { .. } => "tool_end",
                EventKind::Approval { .. } => "approval",
                EventKind::ApprovalDone { .. } => "approval_done",
                EventKind::State {
                    state: SessionState::Working,
                    ..
                } => "working",
                EventKind::State {
                    state: SessionState::NeedsYou,
                    ..
                } => "needs_you",
                EventKind::State {
                    state: SessionState::Idle,
                    ..
                } => "idle",
                EventKind::State { .. } => "state",
                EventKind::TurnDone { .. } => "turn_done",
                EventKind::Notice { .. } => "notice",
            })
            .collect()
    }
}

fn started(turn: &str) -> Value {
    json!({"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":turn,"items":[],"status":"inProgress"}}})
}
fn completed(turn: &str) -> Value {
    json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":turn,"items":[],"status":"completed"}}})
}

#[tokio::test]
async fn qa_fixture_has_exact_event_order_and_full_text() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    h.peer.send_fixture(QA).await.unwrap();
    h.idle().await;
    assert_eq!(
        h.names(),
        [
            "working",
            "working",
            "delta",
            "delta",
            "text",
            "turn_done",
            "idle"
        ]
    );
    assert!(h.outputs.iter().any(|o| matches!(o.event(), Some(EventKind::AssistantText { item, text }) if item == "answer-1" && text == "你好，Tether")));
    h.stop().await;
}

#[tokio::test]
async fn tool_fixture_replays_all_tool_lifecycles() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    h.peer.send_fixture(TOOLS).await.unwrap();
    h.idle().await;
    assert_eq!(
        h.names(),
        [
            "working",
            "working",
            "thinking",
            "tool_start",
            "tool_end",
            "tool_start",
            "tool_end",
            "tool_start",
            "tool_end",
            "tool_start",
            "tool_end",
            "turn_done",
            "idle"
        ]
    );
    assert!(
        h.outputs
            .iter()
            .filter_map(AgentOutput::event)
            .filter(|e| matches!(e, EventKind::ToolEnd { ok: true, .. }))
            .count()
            == 4
    );
    h.stop().await;
}

#[tokio::test]
async fn approval_fixtures_route_string_and_numeric_ids_for_every_decision() {
    for (fixture, rpc_id) in [
        (COMMAND_APPROVAL, json!(41)),
        (FILE_APPROVAL, json!("file-callback")),
    ] {
        for decision in [
            Decision::AllowOnce,
            Decision::AllowSession,
            Decision::Deny,
            Decision::Abort,
        ] {
            let mut h = Harness::new(SpawnOptions::default());
            h.boot(false).await;
            h.begin().await;
            let (before, after) = fixture.split_once("// ANSWER").unwrap();
            h.peer.send_fixture(before).await.unwrap();
            h.wait(|o| {
                matches!(
                    o.event(),
                    Some(EventKind::State {
                        state: SessionState::NeedsYou,
                        ..
                    })
                )
            })
            .await;
            let approval = h
                .outputs
                .iter()
                .find_map(|o| match o.event() {
                    Some(EventKind::Approval { approval }) => Some(approval.clone()),
                    _ => None,
                })
                .unwrap();
            assert_eq!(approval.choices.len(), 4);
            if rpc_id.is_string() {
                assert!(approval.detail.contains("a.txt\n+hi"));
            }
            h.cmd
                .send(AgentCommand::Answer {
                    approval: approval.id.clone(),
                    decision,
                })
                .unwrap();
            assert_eq!(
                h.next().await,
                json!({"id":rpc_id,"result":{"decision":translate::decision(decision)}})
            );
            // The mock's completion must match the user's decision, not blindly
            // replay a successful execution after a decline/cancel.
            if matches!(decision, Decision::AllowOnce | Decision::AllowSession) {
                h.peer.send_fixture(after).await.unwrap();
            } else {
                h.peer.send_json(&completed("turn-1")).await.unwrap();
            }
            h.idle().await;
            let names = h.names();
            assert_eq!(
                &names[..7],
                [
                    "working",
                    "working",
                    "tool_start",
                    "approval",
                    "needs_you",
                    "approval_done",
                    "working"
                ]
            );
            assert_eq!(&names[names.len() - 2..], ["turn_done", "idle"]);
            assert!(h.outputs.iter().any(|o| matches!(o.event(), Some(EventKind::ApprovalDone { approval: id, decision: d }) if id == &approval.id && *d == decision)));
            h.stop().await;
        }
    }
}

#[tokio::test]
async fn unsupported_requests_are_answered_without_stalling_the_turn() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    for (i, method) in [
        "item/tool/requestUserInput",
        "mcpServer/elicitation/request",
        "item/permissions/requestApproval",
        "item/tool/call",
        "account/chatgptAuthTokens/refresh",
        "attestation/generate",
    ]
    .into_iter()
    .enumerate()
    {
        h.peer
            .send_json(&json!({"id":format!("r{i}"),"method":method,"params":{}}))
            .await
            .unwrap();
        let response = h.next().await;
        assert_eq!(response["id"], format!("r{i}"));
        assert_eq!(response["error"]["code"], -32601);
        h.wait(|o| matches!(o.event(), Some(EventKind::Notice { level: NoticeLevel::Warn, text }) if text == "手机端暂不支持此类请求")).await;
    }
    h.begin().await;
    h.peer.send_fixture(QA).await.unwrap();
    h.idle().await;
    h.stop().await;
}

#[tokio::test]
async fn send_before_ready_emits_working_immediately_and_is_not_lost() {
    let mut h = Harness::new(SpawnOptions::default());
    h.cmd
        .send(AgentCommand::Send {
            text: "early".into(),
            images: vec![r"C:\up\a.png".into()],
        })
        .unwrap();
    h.wait(|o| {
        matches!(
            o.event(),
            Some(EventKind::State {
                state: SessionState::Working,
                ..
            })
        )
    })
    .await;
    h.boot(false).await;
    let request = h.next().await;
    assert_eq!(request["method"], "turn/start");
    assert_eq!(
        request["params"]["input"],
        json!([
            {"type":"text","text":"early","text_elements":[]},
            {"type":"localImage","path":r"C:\up\a.png"}
        ])
    );
    h.stop().await;
}

#[tokio::test]
async fn sends_queued_while_starting_steer_using_expected_turn_id() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    h.cmd
        .send(AgentCommand::Send {
            text: "second".into(),
            images: Vec::new(),
        })
        .unwrap();
    h.cmd
        .send(AgentCommand::Send {
            text: "third".into(),
            images: Vec::new(),
        })
        .unwrap();
    h.peer
        .send_json(&json!({"id":3,"result":{"turn":{"id":"turn-1"}}}))
        .await
        .unwrap();
    for text in ["second", "third"] {
        let request = h.next().await;
        assert_eq!(request["method"], "turn/steer");
        assert_eq!(request["params"]["expectedTurnId"], "turn-1");
        assert_eq!(
            request["params"]["input"][0],
            json!({"type":"text","text":text,"text_elements":[]})
        );
        h.peer
            .send_json(&json!({"id":request["id"],"result":{"turnId":"turn-1"}}))
            .await
            .unwrap();
    }
    h.peer.send_json(&completed("turn-1")).await.unwrap();
    h.idle().await;
    h.stop().await;
}

#[tokio::test]
async fn interrupt_fixture_waits_for_completion_before_idle() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    let (before, after) = INTERRUPT.split_once("// INTERRUPT").unwrap();
    h.peer.send_fixture(before).await.unwrap();
    h.wait(|o| matches!(o.event(), Some(EventKind::AssistantDelta { .. })))
        .await;
    h.cmd.send(AgentCommand::Interrupt).unwrap();
    assert_eq!(
        h.next().await,
        json!({"id":4,"method":"turn/interrupt","params":{"threadId":"thread-1","turnId":"turn-1"}})
    );
    h.peer.send_fixture(after).await.unwrap();
    h.idle().await;
    assert_eq!(
        h.names(),
        ["working", "working", "delta", "turn_done", "idle"]
    );
    h.stop().await;
}

#[tokio::test]
async fn interrupt_before_turn_id_and_late_start_response_do_not_resurrect_turn() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    h.cmd.send(AgentCommand::Interrupt).unwrap();
    h.peer.send_json(&started("turn-1")).await.unwrap();
    let interrupt = h.next().await;
    assert_eq!(interrupt["method"], "turn/interrupt");
    h.peer.send_json(&completed("turn-1")).await.unwrap();
    h.idle().await;
    h.peer
        .send_json(&json!({"id":3,"result":{"turn":{"id":"turn-1"}}}))
        .await
        .unwrap();
    h.peer.send_json(&started("turn-1")).await.unwrap(); // stale, ignored
    h.peer.send_json(&completed("turn-1")).await.unwrap(); // duplicate, ignored
    h.cmd
        .send(AgentCommand::Send {
            text: "new turn".into(),
            images: Vec::new(),
        })
        .unwrap();
    assert_eq!(h.next().await["method"], "turn/start");
    h.stop().await;
}

#[tokio::test]
async fn interrupt_cancels_pending_approvals_and_stale_answer_only_warns() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    h.peer
        .send_fixture(COMMAND_APPROVAL.split_once("// ANSWER").unwrap().0)
        .await
        .unwrap();
    h.wait(|o| {
        matches!(
            o.event(),
            Some(EventKind::State {
                state: SessionState::NeedsYou,
                ..
            })
        )
    })
    .await;
    h.cmd.send(AgentCommand::Interrupt).unwrap();
    assert_eq!(
        h.next().await,
        json!({"id":41,"result":{"decision":"cancel"}})
    );
    assert_eq!(h.next().await["method"], "turn/interrupt");
    h.cmd
        .send(AgentCommand::Answer {
            approval: "codex:number:41".into(),
            decision: Decision::AllowOnce,
        })
        .unwrap();
    h.wait(|o| {
        matches!(
            o.event(),
            Some(EventKind::Notice {
                level: NoticeLevel::Warn,
                ..
            })
        )
    })
    .await;
    h.peer.send_json(&completed("turn-1")).await.unwrap();
    h.idle().await;
    assert!(h.outputs.iter().any(|o| matches!(
        o.event(),
        Some(EventKind::ApprovalDone {
            decision: Decision::Abort,
            ..
        })
    )));
    h.stop().await;
}

#[tokio::test]
async fn errors_fixture_reports_errors_then_turn_done_and_idle() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    h.peer.send_fixture(ERROR).await.unwrap();
    h.idle().await;
    assert_eq!(
        h.names(),
        [
            "working",
            "working",
            "notice",
            "notice",
            "turn_done",
            "idle"
        ]
    );
    h.stop().await;
}

#[tokio::test]
async fn other_threads_are_filtered_and_foreign_approvals_are_rejected() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    for line in QA.lines() {
        let mut value: Value = serde_json::from_str(line).unwrap();
        if value.get("method").is_some() {
            value["params"]["threadId"] = json!("subagent-thread");
            h.peer.send_json(&value).await.unwrap();
        }
    }
    h.peer.send_json(&json!({"id":"foreign","method":"item/commandExecution/requestApproval","params":{"threadId":"subagent-thread","itemId":"c"}})).await.unwrap();
    assert_eq!(h.next().await["error"]["code"], -32602);
    h.peer.send_fixture(QA).await.unwrap();
    h.idle().await;
    assert_eq!(
        h.names(),
        [
            "working",
            "working",
            "delta",
            "delta",
            "text",
            "turn_done",
            "idle"
        ]
    );
    h.stop().await;
}

#[tokio::test]
async fn resume_uses_native_id_and_can_continue() {
    let mut h = Harness::new(SpawnOptions {
        resume: Some("thread-1".into()),
        ..Default::default()
    });
    h.boot(true).await;
    h.begin().await;
    h.peer.send_fixture(QA).await.unwrap();
    h.idle().await;
    h.stop().await;
}

#[tokio::test]
async fn resume_running_turn_steers_instead_of_starting() {
    let mut h = Harness::new(SpawnOptions {
        resume: Some("thread-1".into()),
        ..Default::default()
    });
    h.next().await;
    h.peer
        .send_json(&json!({"id":1,"result":{}}))
        .await
        .unwrap();
    h.next().await;
    assert_eq!(h.next().await["method"], "thread/resume");
    h.peer.send_json(&json!({"id":2,"result":{"thread":{"id":"thread-1","turns":[{"id":"running","status":"inProgress"}]}}})).await.unwrap();
    h.wait(|o| matches!(o, AgentOutput::NativeId(_))).await;
    h.cmd
        .send(AgentCommand::Send {
            text: "continue".into(),
            images: Vec::new(),
        })
        .unwrap();
    let steer = h.next().await;
    assert_eq!(steer["method"], "turn/steer");
    assert_eq!(steer["params"]["expectedTurnId"], "running");
    h.stop().await;
}

#[tokio::test]
async fn rejected_turn_start_returns_idle_and_accepts_a_new_message() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    h.peer
        .send_json(&json!({"id":3,"error":{"code":-32000,"message":"busy"}}))
        .await
        .unwrap();
    h.idle().await;
    assert_eq!(h.names(), ["working", "notice", "turn_done", "idle"]);
    h.cmd
        .send(AgentCommand::Send {
            text: "retry".into(),
            images: Vec::new(),
        })
        .unwrap();
    assert_eq!(h.next().await["method"], "turn/start");
    h.stop().await;
}

#[tokio::test]
async fn malformed_messages_and_approvals_do_not_panic_or_block_reads() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    for bad in [
        "not JSON",
        "{}",
        r#"{"method":"item/agentMessage/delta","params":{"threadId":"thread-1"}}"#,
    ] {
        h.peer.send_line(bad).await.unwrap();
    }
    h.peer.send_json(&json!({"id":50,"method":"item/fileChange/requestApproval","params":{"threadId":"thread-1"}})).await.unwrap();
    assert_eq!(h.next().await["error"]["code"], -32602);
    h.begin().await;
    h.peer.send_fixture(QA).await.unwrap();
    h.idle().await;
    assert_eq!(h.names().iter().filter(|n| **n == "notice").count(), 4);
    h.stop().await;
}

#[tokio::test]
async fn initialization_errors_and_deadlines_end_the_session() {
    let mut h = Harness::new(SpawnOptions::default());
    h.next().await;
    h.peer
        .send_json(&json!({"id":1,"error":{"code":-32000,"message":"init failed"}}))
        .await
        .unwrap();
    let error = tokio::time::timeout(T, h.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("init failed"));
    let h = Harness::with_timeout(SpawnOptions::default(), Duration::from_millis(50));
    let error = tokio::time::timeout(T, h.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("initialize: response timed out"));
}

#[tokio::test]
async fn turn_request_deadline_recovers_to_idle() {
    let mut h = Harness::with_timeout(SpawnOptions::default(), Duration::from_millis(300));
    h.boot(false).await;
    h.begin().await;
    h.idle().await;
    assert_eq!(h.names(), ["working", "notice", "turn_done", "idle"]);
    h.stop().await;
}

#[tokio::test]
async fn eof_and_dropping_command_sender_finish_without_hanging() {
    let mut h = Harness::new(SpawnOptions::default());
    h.next().await; // let initialize reach stdin before closing the peer
    drop(h.peer);
    assert!(
        tokio::time::timeout(T, h.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("before thread initialization")
    );
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    drop(h.peer);
    assert_eq!(
        tokio::time::timeout(T, h.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Stop::Eof
    );
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    drop(h.peer);
    assert!(
        tokio::time::timeout(T, h.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("during a turn")
    );
    let h = Harness::new(SpawnOptions::default());
    drop(h.cmd);
    assert_eq!(
        tokio::time::timeout(T, h.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Stop::Shutdown
    );
}

#[tokio::test]
async fn missing_program_reports_one_exited_output_without_panicking() {
    let (out, mut rx) = mpsc::unbounded_channel();
    let _cmd = crate::CodexAdapter::default()
        .spawn(
            SpawnOptions {
                program: Some(PathBuf::from("tether-nonexistent-codex-T04")),
                env: vec![("TETHER_RECORD_DIR".into(), String::new())],
                ..Default::default()
            },
            out,
        )
        .unwrap();
    let output = tokio::time::timeout(T, rx.recv()).await.unwrap().unwrap();
    assert!(
        matches!(output, AgentOutput::Exited { code: None, error: Some(error) } if error.contains("find codex executable"))
    );
    assert!(rx.recv().await.is_none());
}

#[test]
fn process_configuration_honors_program_args_env_and_working_directory() {
    let cwd = tempfile::tempdir().unwrap();
    let opts = SpawnOptions {
        cwd: cwd.path().to_owned(),
        program: Some(std::env::current_exe().unwrap()),
        args: vec!["-c".into(), "key=value".into()],
        env: vec![
            ("T04_TEST".into(), "yes".into()),
            ("TETHER_RECORD_DIR".into(), "records".into()),
        ],
        ..Default::default()
    };
    let cmd = command(&opts).unwrap();
    assert_eq!(cmd.as_std().get_program(), opts.program.as_ref().unwrap());
    assert_eq!(
        cmd.as_std().get_args().collect::<Vec<_>>(),
        ["app-server", "-c", "key=value"]
    );
    assert_eq!(cmd.as_std().get_current_dir(), Some(cwd.path()));
    assert_eq!(record_dir(&opts), Some(PathBuf::from("records")));
    let opts = SpawnOptions {
        env: vec![
            ("TETHER_RECORD_DIR".into(), "records".into()),
            ("TETHER_RECORD_DIR".into(), "".into()),
        ],
        ..Default::default()
    };
    assert_eq!(record_dir(&opts), None);
}

#[cfg(unix)]
#[tokio::test]
async fn non_utf8_cwd_is_reported_as_an_error_instead_of_panicking() {
    use std::os::unix::ffi::OsStringExt;
    let mut h = Harness::new(SpawnOptions {
        cwd: PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff])),
        ..Default::default()
    });
    h.next().await;
    h.peer
        .send_json(&json!({"id":1,"result":{}}))
        .await
        .unwrap();
    assert_eq!(h.next().await["method"], "initialized");
    let error = tokio::time::timeout(T, h.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("encode codex cwd"));
}

#[tokio::test]
async fn stderr_is_drained_recorded_and_bounded() {
    let (mut a, b) = tokio::io::duplex(65536);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stderr.log");
    let file = tokio::fs::File::create(&path).await.unwrap();
    let input = format!("{}END", "好".repeat(10000));
    a.write_all(input.as_bytes()).await.unwrap();
    drop(a);
    let tail = drain_stderr(b, Some(file)).await.unwrap();
    assert!(tail.len() <= tether_proto::MAX_TOOL_OUTPUT);
    assert!(tail.ends_with("END"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), input);
}

#[tokio::test]
async fn child_exit_captures_actual_status_and_stderr_without_a_model() {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/D", "/C", "echo synthetic-failure 1>&2 & exit /B 7"])
            .creation_flags(0x0800_0000);
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo synthetic-failure >&2; exit 7"]);
        cmd
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().unwrap();
    let (_tx, mut rx) = mpsc::unbounded_channel();
    let (out, _outputs) = mpsc::unbounded_channel();
    let (code, error) = tokio::time::timeout(
        T,
        drive_child(
            child,
            &SpawnOptions::default(),
            Inbox::new(&mut rx),
            &out,
            None,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(code, Some(7));
    assert!(error.unwrap().contains("synthetic-failure"));
}

#[tokio::test]
async fn real_shell_wrapper_shuts_down_on_command_or_sender_drop_and_records() {
    for drop_sender in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join("recordings");
        #[cfg(windows)]
        let program = {
            let path = dir.path().join("fake codex.cmd");
            std::fs::write(&path, "@echo off\r\nmore >nul\r\n").unwrap();
            path
        };
        #[cfg(not(windows))]
        let program = {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join("fake codex");
            std::fs::write(&path, "#!/bin/sh\ncat >/dev/null\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let (out, mut rx) = mpsc::unbounded_channel();
        let tx = crate::CodexAdapter::default()
            .spawn(
                SpawnOptions {
                    cwd: dir.path().to_owned(),
                    program: Some(program),
                    env: vec![(
                        "TETHER_RECORD_DIR".into(),
                        records.to_string_lossy().into_owned(),
                    )],
                    ..Default::default()
                },
                out,
            )
            .unwrap();
        if drop_sender {
            drop(tx);
        } else {
            tx.send(AgentCommand::Shutdown).unwrap();
        }
        let output = tokio::time::timeout(T, rx.recv()).await.unwrap().unwrap();
        assert_eq!(
            output,
            AgentOutput::Exited {
                code: Some(0),
                error: None
            }
        );
        assert!(rx.recv().await.is_none());
        let files: Vec<_> = std::fs::read_dir(records)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 3);
        let stdin = files
            .iter()
            .find(|p| p.to_string_lossy().ends_with(".stdin.jsonl"))
            .unwrap();
        let input: Value =
            serde_json::from_str(std::fs::read_to_string(stdin).unwrap().trim()).unwrap();
        assert_eq!(input["method"], "initialize");
        assert!(input.get("jsonrpc").is_none());
    }
}

#[tokio::test]
async fn multiple_approvals_and_stale_turn_callbacks_do_not_cross_route() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    h.begin().await;
    h.peer.send_json(&started("turn-1")).await.unwrap();
    for id in [json!(42), json!("42")] {
        h.peer.send_json(&json!({"id":id,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"same-item","command":"echo hi"}})).await.unwrap();
        h.wait(|o| {
            matches!(
                o.event(),
                Some(EventKind::State {
                    state: SessionState::NeedsYou,
                    ..
                })
            )
        })
        .await;
    }
    for (index, id) in [Id::Number(42), Id::String("42".into())]
        .into_iter()
        .enumerate()
    {
        h.cmd
            .send(AgentCommand::Answer {
                approval: id.approval_id(),
                decision: Decision::Deny,
            })
            .unwrap();
        assert_eq!(
            h.next().await,
            json!({"id":id,"result":{"decision":"decline"}})
        );
        h.wait(|o| {
            matches!(
                o.event(),
                Some(EventKind::State {
                    state: SessionState::Working,
                    ..
                })
            )
        })
        .await;
        if index == 0 {
            h.wait(|o| {
                matches!(
                    o.event(),
                    Some(EventKind::State {
                        state: SessionState::NeedsYou,
                        ..
                    })
                )
            })
            .await;
        }
    }
    h.peer.send_json(&completed("turn-1")).await.unwrap();
    h.idle().await;
    h.peer.send_json(&json!({"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"old","delta":"stale"}})).await.unwrap();
    h.peer.send_json(&json!({"id":90,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"old"}})).await.unwrap();
    assert_eq!(h.next().await["error"]["code"], -32602);
    h.cmd
        .send(AgentCommand::Send {
            text: "new".into(),
            images: Vec::new(),
        })
        .unwrap();
    assert_eq!(h.next().await["method"], "turn/start");
    h.wait(|o| {
        matches!(
            o.event(),
            Some(EventKind::State {
                state: SessionState::Working,
                ..
            })
        )
    })
    .await;
    assert!(!h.names().contains(&"delta"));
    h.stop().await;
}

/// Up to the thread request, which is returned unanswered.
async fn handshake(h: &mut Harness) -> Value {
    h.next().await;
    h.peer
        .send_json(&json!({"id":1,"result":{}}))
        .await
        .unwrap();
    assert_eq!(h.next().await, json!({"method":"initialized"}));
    h.next().await
}

#[tokio::test]
async fn chosen_model_goes_to_the_thread_and_every_turn() {
    let mut h = Harness::new(SpawnOptions {
        model: Some("gpt-6.1-sol".into()),
        effort: Some("high".into()),
        ..Default::default()
    });
    let start = handshake(&mut h).await;
    assert_eq!(start["method"], "thread/start");
    assert_eq!(start["params"]["model"], "gpt-6.1-sol");
    h.peer
        .send_json(&json!({"id":start["id"],"result":{"thread":{"id":"thread-1","turns":[]}}}))
        .await
        .unwrap();
    h.cmd
        .send(AgentCommand::Send {
            text: "a".into(),
            images: Vec::new(),
        })
        .unwrap();
    let turn = h.next().await;
    assert_eq!(turn["method"], "turn/start");
    assert_eq!(
        (&turn["params"]["model"], &turn["params"]["effort"]),
        (&json!("gpt-6.1-sol"), &json!("high"))
    );
    h.peer
        .send_json(&json!({"id":turn["id"],"result":{"turn":{"id":"turn-1"}}}))
        .await
        .unwrap();
    h.peer
        .send_json(&json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","error":null}}}))
        .await
        .unwrap();
    h.idle().await;

    // Only the effort changes; the model stays.
    h.cmd
        .send(AgentCommand::SetModel {
            model: None,
            effort: Some("low".into()),
            speed: None,
            permission: None,
        })
        .unwrap();
    h.wait(|o| matches!(o.event(), Some(EventKind::Notice { text, .. }) if text.contains("gpt-6.1-sol") && text.contains("low")))
        .await;
    h.cmd
        .send(AgentCommand::Send {
            text: "b".into(),
            images: Vec::new(),
        })
        .unwrap();
    let turn = h.next().await;
    assert_eq!(
        (&turn["params"]["model"], &turn["params"]["effort"]),
        (&json!("gpt-6.1-sol"), &json!("low"))
    );
    h.stop().await;
}

#[tokio::test]
async fn without_a_choice_turns_name_no_model() {
    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    let turn = h.begin().await;
    assert!(turn["params"].get("model").is_none());
    h.stop().await;
}

#[tokio::test]
async fn a_chat_may_only_read() {
    let mut h = Harness::new(SpawnOptions {
        chat: true,
        ..Default::default()
    });
    let start = handshake(&mut h).await;
    assert_eq!(start["params"]["sandbox"], "read-only");
    assert_eq!(start["params"]["approvalPolicy"], "on-request");
    h.stop().await;
}

#[tokio::test]
async fn a_chosen_profile_replaces_the_sandbox_and_follows_each_turn() {
    let mut h = Harness::new(SpawnOptions {
        chat: true,
        permission: Some(":danger-full-access".into()),
        ..Default::default()
    });
    let start = handshake(&mut h).await;
    assert_eq!(start["params"]["permissions"], ":danger-full-access");
    assert!(
        start["params"].get("sandbox").is_none(),
        "profile and sandbox do not combine"
    );
    h.stop().await;

    let mut h = Harness::new(SpawnOptions::default());
    h.boot(false).await;
    let turn = h.begin().await;
    assert!(
        turn["params"].get("permissions").is_none(),
        "no choice, no profile"
    );
    h.peer
        .send_json(&json!({"id":turn["id"],"result":{"turn":{"id":"turn-1","status":"inProgress","items":[]}}}))
        .await
        .unwrap();
    h.peer
        .send_json(&json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","error":null}}}))
        .await
        .unwrap();
    h.idle().await;
    h.cmd
        .send(AgentCommand::SetModel {
            model: None,
            effort: None,
            speed: None,
            permission: Some(":read-only".into()),
        })
        .unwrap();
    h.wait(|o| matches!(o.event(), Some(EventKind::Notice { text, .. }) if text.contains("权限：只读")))
        .await;
    h.cmd
        .send(AgentCommand::Send {
            text: "b".into(),
            images: Vec::new(),
        })
        .unwrap();
    let turn = h.next().await;
    assert_eq!(turn["params"]["permissions"], ":read-only");
    h.stop().await;
}

#[tokio::test]
async fn resuming_a_running_turn_says_working() {
    let mut h = Harness::new(SpawnOptions {
        resume: Some("thread-1".into()),
        model: Some("m".into()),
        ..Default::default()
    });
    let resume = handshake(&mut h).await;
    assert_eq!(resume["method"], "thread/resume");
    assert!(
        resume["params"].get("model").is_none(),
        "resume keeps the thread's model"
    );
    h.peer
        .send_json(
            &json!({"id":resume["id"],"result":{"thread":{"id":"thread-1","turns":[
                {"id":"old","status":"completed"},{"id":"live","status":"inProgress"}
            ]}}}),
        )
        .await
        .unwrap();
    h.wait(|o| {
        matches!(
            o.event(),
            Some(EventKind::State {
                state: SessionState::Working,
                ..
            })
        )
    })
    .await;
    // A message now steers the running turn.
    h.cmd
        .send(AgentCommand::Send {
            text: "再来".into(),
            images: Vec::new(),
        })
        .unwrap();
    let steer = h.next().await;
    assert_eq!(steer["method"], "turn/steer");
    assert_eq!(steer["params"]["expectedTurnId"], "live");
    h.stop().await;
}

#[test]
fn provider_mismatch_is_explained() {
    let text = translate::error_text(
        &json!({"code":-32600,"data":{"type":"modelProviderMismatch","modelProvider":"custom","currentModelProvider":"openai"},"message":"This chat uses custom"}),
    );
    assert!(
        text.contains("「custom」") && text.contains("「openai」") && text.contains("CC Switch"),
        "{text}"
    );
}

#[test]
fn unknown_model_is_explained() {
    let text = translate::error_text(
        &json!({"message":"{\"error\":{\"message\":\"The 'x' model is not supported when using Codex with a ChatGPT account.\",\"code\":\"model_not_found\"}}"}),
    );
    assert!(
        text.starts_with("电脑上的 Codex 现在登录的是 ChatGPT 账号"),
        "{text}"
    );
    let text = translate::error_text(&json!({"message":"{\"code\":\"model_not_found\"}"}));
    assert!(text.starts_with("电脑上的 Codex 找不到这个模型"), "{text}");
}

#[tokio::test]
async fn a_project_conversation_names_its_project_speed_and_folders() {
    let roots = vec![
        std::path::PathBuf::from("/lib"),
        std::path::PathBuf::from("/meet"),
    ];
    let mut h = Harness::new(SpawnOptions {
        cwd: roots[0].clone(),
        project: Some("p1".into()),
        roots: roots.clone(),
        speed: Some("priority".into()),
        reveal: true,
        ..Default::default()
    });
    let start = handshake(&mut h).await;
    assert_eq!(start["method"], "thread/start");
    assert_eq!(start["params"]["projectId"], "p1");
    assert_eq!(start["params"]["serviceTier"], "priority");
    assert_eq!(
        start["params"]["runtimeWorkspaceRoots"],
        serde_json::to_value(&roots).unwrap()
    );
    h.peer
        .send_json(&json!({"id":start["id"],"result":{"thread":{"id":"thread-1","turns":[]}}}))
        .await
        .unwrap();
    h.wait(|o| matches!(o, AgentOutput::NativeId(_))).await;
    crate::desktop::REVEALED.with(|r| r.borrow_mut().clear());
    crate::desktop::ANNOUNCED.with(|r| r.borrow_mut().clear());

    h.cmd
        .send(AgentCommand::Send {
            text: "a".into(),
            images: Vec::new(),
        })
        .unwrap();
    let turn = h.next().await;
    assert_eq!(turn["params"]["serviceTier"], "priority");
    h.peer
        .send_json(&json!({"id":turn["id"],"result":{"turn":{"id":"turn-1"}}}))
        .await
        .unwrap();
    h.peer
        .send_json(&json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","error":null}}}))
        .await
        .unwrap();
    h.idle().await;
    // Shown in the desktop app once, after the first turn started.
    assert_eq!(
        crate::desktop::REVEALED.with(|r| r.borrow().clone()),
        vec!["thread-1".to_string()]
    );
    // And listed in its sidebar, which needs no reveal.
    assert_eq!(
        crate::desktop::ANNOUNCED.with(|r| r.borrow().clone()),
        vec!["thread-1".to_string()]
    );

    h.cmd
        .send(AgentCommand::SetModel {
            model: None,
            effort: None,
            speed: Some("default".into()),
            permission: None,
        })
        .unwrap();
    h.wait(|o| matches!(o.event(), Some(EventKind::Notice { text, .. }) if text.contains("标准")))
        .await;
    h.cmd
        .send(AgentCommand::Send {
            text: "b".into(),
            images: Vec::new(),
        })
        .unwrap();
    let turn = h.next().await;
    assert_eq!(turn["params"]["serviceTier"], "default");
    h.peer
        .send_json(&json!({"id":turn["id"],"result":{"turn":{"id":"turn-2"}}}))
        .await
        .unwrap();
    h.peer
        .send_json(&json!({"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-2","error":null}}}))
        .await
        .unwrap();
    h.idle().await;
    assert_eq!(crate::desktop::REVEALED.with(|r| r.borrow().len()), 1);
    assert_eq!(crate::desktop::ANNOUNCED.with(|r| r.borrow().len()), 1);
    h.stop().await;
}

#[tokio::test]
async fn a_single_folder_project_sends_no_extra_roots() {
    let mut h = Harness::new(SpawnOptions {
        cwd: "/w".into(),
        project: Some("p1".into()),
        roots: vec!["/w".into()],
        ..Default::default()
    });
    let start = handshake(&mut h).await;
    assert!(start["params"].get("runtimeWorkspaceRoots").is_none());
    assert!(start["params"].get("serviceTier").is_none());
    h.stop().await;
}

#[tokio::test]
async fn a_fork_copies_the_conversation_into_a_new_one_and_shows_it() {
    let mut h = Harness::new(SpawnOptions {
        cwd: "/w".into(),
        resume: Some("thread-old".into()),
        fork: true,
        model: Some("gpt-6.1-sol".into()),
        roots: vec!["/w".into(), "/x".into()],
        reveal: true,
        ..Default::default()
    });
    let fork = handshake(&mut h).await;
    assert_eq!(fork["method"], "thread/fork");
    assert_eq!(fork["params"]["threadId"], "thread-old");
    assert_eq!(fork["params"]["model"], "gpt-6.1-sol");
    assert!(fork["params"].get("runtimeWorkspaceRoots").is_none());
    h.peer
        .send_json(&json!({"id":fork["id"],"result":{"thread":{"id":"thread-new","turns":[]}}}))
        .await
        .unwrap();
    h.wait(|o| matches!(o, AgentOutput::NativeId(id) if id == "thread-new"))
        .await;
    crate::desktop::REVEALED.with(|r| r.borrow_mut().clear());
    crate::desktop::ANNOUNCED.with(|r| r.borrow_mut().clear());
    h.cmd
        .send(AgentCommand::Send {
            text: "a".into(),
            images: Vec::new(),
        })
        .unwrap();
    let turn = h.next().await;
    assert_eq!(turn["params"]["threadId"], "thread-new");
    h.peer
        .send_json(&json!({"id":turn["id"],"result":{"turn":{"id":"turn-1"}}}))
        .await
        .unwrap();
    h.peer
        .send_json(&json!({"method":"turn/completed","params":{"threadId":"thread-new","turn":{"id":"turn-1","error":null}}}))
        .await
        .unwrap();
    h.idle().await;
    assert_eq!(
        crate::desktop::REVEALED.with(|r| r.borrow().clone()),
        vec!["thread-new".to_string()]
    );
    assert_eq!(
        crate::desktop::ANNOUNCED.with(|r| r.borrow().clone()),
        vec!["thread-new".to_string()]
    );
    h.stop().await;
}

#[tokio::test]
async fn a_thread_another_program_has_open_is_explained() {
    let mut h = Harness::new(SpawnOptions {
        resume: Some("thread-1".into()),
        ..Default::default()
    });
    let resume = handshake(&mut h).await;
    assert_eq!(resume["method"], "thread/resume");
    h.peer
        .send_json(&json!({"id":resume["id"],"error":{"code":-32603,"message":"thread-store conflict: thread thread-1 already has an active writer"}}))
        .await
        .unwrap();
    let error = tokio::time::timeout(T, h.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.is::<ThreadBusy>());
    assert!(error.to_string().contains("分叉成新对话"));
}

#[tokio::test]
async fn a_thread_of_another_provider_comes_back_to_be_started_with_it() {
    let mut h = Harness::new(SpawnOptions {
        resume: Some("thread-1".into()),
        ..Default::default()
    });
    h.next().await;
    h.peer
        .send_json(&json!({"id":1,"result":{"userAgent":"fake"}}))
        .await
        .unwrap();
    assert_eq!(h.next().await, json!({"method":"initialized"}));
    let request = h.next().await;
    assert_eq!(request["method"], "thread/resume");
    // Sent while the thread loads: it must survive the restart.
    h.cmd
        .send(AgentCommand::Send {
            text: "接着聊".into(),
            images: Vec::new(),
        })
        .unwrap();
    h.wait(|o| {
        matches!(
            o.event(),
            Some(EventKind::State {
                state: SessionState::Working,
                ..
            })
        )
    })
    .await;
    h.peer
        .send_json(&json!({"id":request["id"],"error":{"code":-32600,"message":"This chat uses openai","data":{"type":"modelProviderMismatch","modelProvider":"openai","currentModelProvider":"custom"}}}))
        .await
        .unwrap();
    let error = timeout(T, h.task).await.unwrap().unwrap().unwrap_err();
    let mismatch = error.downcast::<ProviderMismatch>().unwrap();
    assert_eq!(
        (mismatch.provider.as_str(), mismatch.current.as_str()),
        ("openai", "custom")
    );
    assert_eq!(
        mismatch.queued,
        VecDeque::from([("接着聊".to_owned(), Vec::new())])
    );
}

#[test]
fn the_thread_provider_is_given_before_the_subcommand() {
    let opts = SpawnOptions {
        program: Some(std::env::current_exe().unwrap()),
        ..Default::default()
    };
    let cmd = command_with(&opts, Some("openai")).unwrap();
    let args: Vec<_> = cmd
        .as_std()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[..3], ["-c", "model_provider=\"openai\"", "app-server"]);
    assert!(provider_name("custom-1.relay"));
    assert!(!provider_name("x\" -c y"));
    assert!(!provider_name(""));
}

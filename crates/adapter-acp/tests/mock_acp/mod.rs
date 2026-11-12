//! A Rust ACP agent on the other end of an in-memory stdio connection. No
//! installed agent, network, model, credentials, or shell is needed.

use std::{path::PathBuf, time::Duration};

use serde_json::{Value, json};
use tether_agent_api::{
    AgentAdapter, AgentCommand, AgentOutput, SpawnOptions,
    testkit::{Driver, LineEnd, line_duplex},
};
use tether_proto::{Approval, Decision, EventKind, MAX_TOOL_OUTPUT, NoticeLevel, SessionState};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::{AcpAdapter, AcpAgent, adapter, session};

mod process_tests;

const T: Duration = Duration::from_secs(5);

struct MockAgent {
    io: LineEnd,
    task: JoinHandle<()>,
    cwd: PathBuf,
}

impl MockAgent {
    async fn next(&mut self) -> Value {
        let value = tokio::time::timeout(T, self.io.next_json())
            .await
            .expect("client did not send a frame")
            .expect("invalid JSON from client")
            .expect("client closed the stream");
        assert_eq!(value["jsonrpc"], "2.0");
        value
    }

    async fn send(&mut self, value: Value) {
        self.io.send_json(&value).await.unwrap();
    }

    async fn result(&mut self, request: &Value, result: Value) {
        self.send(json!({"jsonrpc":"2.0", "id":request["id"], "result":result}))
            .await;
    }

    async fn update(&mut self, update: Value) {
        self.send(json!({"jsonrpc":"2.0", "method":"session/update", "params":{"sessionId":"mock-session", "update":update}})).await;
    }

    async fn initialize(&mut self, load: bool, auth: bool) {
        let initialize = self.next().await;
        let mut expected: Value =
            serde_json::from_str(fixture("initialize.client.jsonl").trim()).unwrap();
        expected["params"]["clientInfo"]["version"] = json!(env!("CARGO_PKG_VERSION"));
        assert_eq!(initialize, expected);
        assert_eq!(initialize["method"], "initialize");
        assert_eq!(initialize["params"]["protocolVersion"], 1);
        assert_eq!(
            initialize["params"]["clientCapabilities"],
            json!({"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false})
        );
        assert_eq!(initialize["params"]["clientInfo"]["name"], "tether");
        let mut response: Value =
            serde_json::from_str(fixture("initialize.agent.jsonl").trim()).unwrap();
        response["result"]["agentCapabilities"]["loadSession"] = json!(load);
        response["result"]["authMethods"] = if auth {
            json!([{"id":"login","name":"Login"}])
        } else {
            json!([])
        };
        self.result(&initialize, response["result"].clone()).await;
    }

    async fn new_session(&mut self) {
        let request = self.next().await;
        assert_eq!(request["method"], "session/new");
        assert_eq!(request["params"]["cwd"], self.cwd.to_str().unwrap());
        assert_eq!(request["params"]["mcpServers"], json!([]));
        assert!(request["params"].get("sessionId").is_none());
        let response: Value =
            serde_json::from_str(fixture("new-session.agent.jsonl").trim()).unwrap();
        self.result(&request, response["result"].clone()).await;
    }

    async fn prompt(&mut self, text: &str) -> Value {
        let request = self.next().await;
        assert_eq!(request["method"], "session/prompt");
        assert_eq!(request["params"]["sessionId"], "mock-session");
        assert_eq!(
            request["params"]["prompt"],
            json!([{"type":"text","text":text}])
        );
        request
    }

    async fn replay(&mut self, name: &str) {
        self.io.send_fixture(&fixture(name)).await.unwrap();
    }

    async fn permission(&mut self, id: Value, options: Value) {
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":"session/request_permission", "params":{"sessionId":"mock-session", "toolCall":{"toolCallId":"call-1","title":"运行测试","kind":"execute","rawInput":{"command":"cargo test"}}, "options":options}})).await;
    }

    async fn no_frame(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(30), self.io.next_json())
                .await
                .is_err()
        );
    }

    async fn stopped(self) {
        tokio::time::timeout(T, self.task).await.unwrap().unwrap();
    }
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/acp")
            .join(name),
    )
    .unwrap()
}

fn memory(resume: Option<&str>) -> (Driver, MockAgent) {
    let (client, agent) = line_duplex();
    let (reader, writer) = client.into_parts();
    let (cmd, commands) = mpsc::unbounded_channel();
    let (out, rx) = mpsc::unbounded_channel();
    let cwd = std::env::current_dir().unwrap();
    let run_cwd = cwd.clone();
    let resume = resume.map(str::to_owned);
    let task = tokio::spawn(async move {
        let result = session::run(reader, writer, run_cwd, resume, commands, out.clone()).await;
        let error = result.err().map(|error| format!("{error:#}"));
        let _ = out.send(AgentOutput::Exited {
            code: Some(if error.is_none() { 0 } else { 1 }),
            error,
        });
    });
    (
        Driver {
            cmd,
            rx,
            outputs: Vec::new(),
        },
        MockAgent {
            io: agent,
            task,
            cwd,
        },
    )
}

async fn ready() -> (Driver, MockAgent) {
    let (mut driver, mut agent) = memory(None);
    agent.initialize(false, false).await;
    agent.new_session().await;
    driver
        .wait_for(
            |o| match o {
                AgentOutput::NativeId(id) => Some(id.clone()),
                _ => None,
            },
            T,
        )
        .await
        .unwrap();
    (driver, agent)
}

fn send(driver: &Driver, text: &str) {
    driver
        .send(AgentCommand::Send {
            text: text.into(),
            images: Vec::new(),
        })
        .unwrap();
}

async fn idle(driver: &mut Driver, turns: usize) {
    driver
        .wait_until(
            |out| {
                out.iter()
                    .filter(|o| {
                        matches!(
                            o.event(),
                            Some(EventKind::State {
                                state: SessionState::Idle,
                                ..
                            })
                        )
                    })
                    .count()
                    >= turns
            },
            T,
        )
        .await
        .unwrap();
    assert!(
        !driver
            .events()
            .iter()
            .any(|e| matches!(e, EventKind::UserText { .. }))
    );
}

async fn approval(driver: &mut Driver) -> Approval {
    driver
        .wait_for(
            |o| match o.event() {
                Some(EventKind::Approval { approval }) => Some(approval.clone()),
                _ => None,
            },
            T,
        )
        .await
        .unwrap()
}

async fn exit(driver: &mut Driver) -> (Option<i32>, Option<String>) {
    driver
        .wait_for(
            |o| match o {
                AgentOutput::Exited { code, error } => Some((*code, error.clone())),
                _ => None,
            },
            T,
        )
        .await
        .unwrap()
}

async fn shutdown(driver: &mut Driver, agent: MockAgent) {
    driver.send(AgentCommand::Shutdown).unwrap();
    assert_eq!(exit(driver).await, (Some(0), None));
    agent.stopped().await;
}

fn names(driver: &Driver) -> Vec<&'static str> {
    driver
        .events()
        .iter()
        .map(|e| match e {
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
            EventKind::State { .. } => "error",
            EventKind::AssistantDelta { .. } => "delta",
            EventKind::AssistantText { .. } => "text",
            EventKind::Thinking { .. } => "thinking",
            EventKind::ToolStart { .. } => "tool_start",
            EventKind::ToolEnd { .. } => "tool_end",
            EventKind::Approval { .. } => "approval",
            EventKind::ApprovalDone { .. } => "approval_done",
            EventKind::TurnDone { .. } => "turn_done",
            EventKind::Notice { .. } => "notice",
            EventKind::UserText { .. } => "user_text",
        })
        .collect()
}

fn all_options() -> Value {
    json!([
        {"optionId":"once","name":"Allow once","kind":"allow_once"},
        {"optionId":"always","name":"Remember for this project","kind":"allow_always"},
        {"optionId":"no","name":"Deny once","kind":"reject_once"},
        {"optionId":"never","name":"Deny always","kind":"reject_always"}
    ])
}

#[test]
fn info_preserves_identity_and_full_configured_command() {
    let adapter = adapter(AcpAgent {
        id: "sample".into(),
        name: "Mock ACP".into(),
        command: vec!["definitely-missing-tether-agent".into(), "--acp".into()],
    });
    let info = adapter.info();
    assert_eq!(info.id, "sample");
    assert_eq!(info.name, "Mock ACP");
    assert_eq!(info.kind, tether_proto::AgentKind::Acp);
    assert!(info.chat);
    assert!(!info.available);
    assert_eq!(
        info.terminal_command,
        ["definitely-missing-tether-agent", "--acp"]
    );
}

#[tokio::test]
async fn initializes_new_session_and_replays_stream_fixture() {
    let (mut d, mut a) = ready().await;
    send(&d, "你好");
    a.prompt("你好").await;
    a.replay("stream.jsonl").await;
    idle(&mut d, 1).await;
    assert_eq!(
        names(&d),
        ["working", "delta", "delta", "text", "turn_done", "idle"]
    );
    let deltas: Vec<_> = d
        .events()
        .iter()
        .filter_map(|e| match e {
            EventKind::AssistantDelta { item, text } => Some((item.clone(), text.clone())),
            _ => None,
        })
        .collect();
    let final_text = d
        .events()
        .iter()
        .find_map(|e| match e {
            EventKind::AssistantText { item, text } => Some((item.clone(), text.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(deltas[0].0, deltas[1].0);
    assert_eq!(deltas[0].0, final_text.0);
    assert_eq!(final_text.1, "你好，世界！");
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn thinking_tools_and_plan_follow_schema_and_keep_utf8_tail() {
    let (mut d, mut a) = ready().await;
    send(&d, "tools");
    a.prompt("tools").await;
    a.replay("tools.jsonl").await;
    idle(&mut d, 1).await;
    assert!(
        d.events()
            .iter()
            .any(|e| matches!(e, EventKind::Thinking {text,..} if text == "先检查"))
    );
    assert!(d.events().iter().any(|e| matches!(e, EventKind::Notice {level:NoticeLevel::Info,text} if text.contains("检查代码") && text.contains("pending"))));
    assert!(d.events().iter().any(|e| matches!(e, EventKind::ToolStart {item,tool,summary} if item == "read-1" && tool == "read" && summary == "读取文件")));
    assert!(d.events().iter().any(|e| matches!(e, EventKind::ToolEnd {item,ok:true,output} if item == "read-1" && output == "文件内容")));
    assert!(d.events().iter().any(|e| matches!(e, EventKind::ToolEnd {item,ok:false,output} if item == "exec-1" && output == "测试失败")));
    for text in ["x".repeat(MAX_TOOL_OUTPUT + 500), "中文🙂".repeat(2000)] {
        let result = crate::updates::tool_tail(&text);
        assert!(result.len() <= MAX_TOOL_OUTPUT);
        assert!(result.starts_with('…'));
        assert!(text.ends_with(result.trim_start_matches('…')));
    }
    assert_eq!(
        crate::updates::tool_tail(&"x".repeat(MAX_TOOL_OUTPUT)).len(),
        MAX_TOOL_OUTPUT
    );
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn tool_patches_preserve_prior_content_and_handle_initial_terminal_status() {
    let (mut d, mut a) = ready().await;
    send(&d, "patch");
    let prompt = a.prompt("patch").await;
    a.update(json!({"sessionUpdate":"tool_call","toolCallId":"p","title":"Patch","content":[{"type":"content","content":{"type":"text","text":"keep"}}]})).await;
    a.update(json!({"sessionUpdate":"tool_call_update","toolCallId":"p","content":null,"status":"completed"})).await;
    a.update(json!({"sessionUpdate":"tool_call_update","toolCallId":"p","status":"completed"}))
        .await;
    a.update(json!({"sessionUpdate":"tool_call","toolCallId":"initial","title":"Already done","status":"failed","rawOutput":{"error":"boom"}})).await;
    a.update(json!({"sessionUpdate":"tool_call","toolCallId":"clear","title":"Clear","content":[{"type":"content","content":{"type":"text","text":"old"}}]})).await;
    a.update(json!({"sessionUpdate":"tool_call_update","toolCallId":"clear","content":[]}))
        .await;
    a.update(json!({"sessionUpdate":"tool_call_update","toolCallId":"clear","status":"completed"}))
        .await;
    a.result(&prompt, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 1).await;
    let end: Vec<_> = d
        .events()
        .iter()
        .filter_map(|e| match e {
            EventKind::ToolEnd { item, output, ok } => Some((item.clone(), output.clone(), *ok)),
            _ => None,
        })
        .collect();
    assert_eq!(
        end,
        [
            ("p".into(), "keep".into(), true),
            ("initial".into(), "{\"error\":\"boom\"}".into(), false),
            ("clear".into(), "".into(), true)
        ]
    );
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn tool_output_is_bounded_over_the_wire() {
    let (mut d, mut a) = ready().await;
    send(&d, "long");
    let p = a.prompt("long").await;
    let text = format!("{}END", "中文🙂".repeat(2000));
    a.update(json!({"sessionUpdate":"tool_call","toolCallId":"long","title":"Long","status":"completed","rawOutput":text})).await;
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 1).await;
    let output = d
        .events()
        .iter()
        .find_map(|e| match e {
            EventKind::ToolEnd { output, .. } => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    assert!(output.len() <= MAX_TOOL_OUTPUT);
    assert!(output.ends_with("END"));
    assert!(text.ends_with(output.trim_start_matches('…')));
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn message_ids_and_separate_turns_never_merge_replies() {
    let (mut d, mut a) = ready().await;
    for round in 1..=2 {
        send(&d, "multi");
        let p = a.prompt("multi").await;
        for (id, text) in [("m1", "one"), ("m1", "!"), ("m2", "two")] {
            a.update(json!({"sessionUpdate":"agent_message_chunk","messageId":id,"content":{"type":"text","text":text}})).await;
        }
        a.result(&p, json!({"stopReason":"end_turn"})).await;
        idle(&mut d, round).await;
    }
    let texts: Vec<_> = d
        .events()
        .iter()
        .filter_map(|e| match e {
            EventKind::AssistantText { item, text } => Some((item.clone(), text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(),
        ["one!", "two", "one!", "two"]
    );
    assert_eq!(
        texts
            .iter()
            .map(|(i, _)| i)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn permission_allow_once_allow_session_and_deny_use_actual_option_ids() {
    for (decision, option) in [
        (Decision::AllowOnce, "once"),
        (Decision::AllowSession, "always"),
        (Decision::Deny, "no"),
    ] {
        let (mut d, mut a) = ready().await;
        send(&d, "permission");
        let p = a.prompt("permission").await;
        a.replay("permission.jsonl").await;
        let ap = approval(&mut d).await;
        assert_eq!(
            ap.choices,
            [
                Decision::AllowOnce,
                Decision::AllowSession,
                Decision::Deny,
                Decision::Abort
            ]
        );
        assert_eq!(ap.kind, tether_proto::ApprovalKind::Command);
        assert!(ap.detail.contains("cargo test"));
        assert!(ap.detail.contains("Remember for this project"));
        assert_eq!(ap.cwd, Some(a.cwd.to_string_lossy().into_owned()));
        d.send(AgentCommand::Answer {
            approval: ap.id.clone(),
            decision,
        })
        .unwrap();
        let response = a.next().await;
        let expected = fixture("decisions.client.jsonl")
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|v| v["result"]["outcome"]["optionId"] == option)
            .unwrap();
        assert_eq!(response, expected);
        a.result(&p, json!({"stopReason":"end_turn"})).await;
        idle(&mut d, 1).await;
        let n = names(&d);
        let i = n.iter().position(|n| *n == "approval").unwrap();
        assert_eq!(
            &n[i..i + 4],
            ["approval", "needs_you", "approval_done", "working"]
        );
        assert!(d.events().iter().any(|e|matches!(e,EventKind::ApprovalDone{approval,decision:d} if approval==&ap.id && d==&decision)));
        shutdown(&mut d, a).await;
    }
}

#[tokio::test]
async fn only_advertised_choices_are_offered_and_invalid_answers_do_not_consume_request() {
    let (mut d, mut a) = ready().await;
    send(&d, "limited");
    let p = a.prompt("limited").await;
    a.permission(
        json!(9),
        json!([{"optionId":"no","kind":"reject_once","name":"No"}]),
    )
    .await;
    let ap = approval(&mut d).await;
    assert_eq!(ap.choices, [Decision::Deny, Decision::Abort]);
    for (id, decision) in [
        ("wrong".into(), Decision::Deny),
        (ap.id.clone(), Decision::AllowSession),
    ] {
        d.send(AgentCommand::Answer {
            approval: id,
            decision,
        })
        .unwrap();
    }
    a.no_frame().await;
    d.send(AgentCommand::Answer {
        approval: ap.id.clone(),
        decision: Decision::Deny,
    })
    .unwrap();
    assert_eq!(a.next().await["result"]["outcome"]["optionId"], "no");
    d.send(AgentCommand::Answer {
        approval: ap.id,
        decision: Decision::Deny,
    })
    .unwrap();
    a.no_frame().await;
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 1).await;
    assert_eq!(
        d.events()
            .iter()
            .filter(|e| matches!(e, EventKind::ApprovalDone { .. }))
            .count(),
        1
    );
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn reject_always_and_unknown_kinds_are_not_misrepresented_as_deny_once() {
    let (mut d, mut a) = ready().await;
    send(&d, "unsupported choice");
    a.prompt("unsupported choice").await;
    a.permission(
        json!(99),
        json!([
            {"optionId":"never","kind":"reject_always","name":"Never"},
            {"optionId":"future","kind":"_future","name":"Future"}
        ]),
    )
    .await;
    let ap = approval(&mut d).await;
    assert_eq!(ap.choices, [Decision::Abort]);
    d.send(AgentCommand::Answer {
        approval: ap.id,
        decision: Decision::Abort,
    })
    .unwrap();
    assert_eq!(a.next().await["result"]["outcome"]["outcome"], "cancelled");
    assert_eq!(a.next().await["method"], "session/cancel");
    a.replay("cancelled.jsonl").await;
    idle(&mut d, 1).await;
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn abort_and_interrupt_cancel_all_pending_permissions_and_wait_for_stop_reason() {
    for answer_abort in [true, false] {
        let (mut d, mut a) = ready().await;
        send(&d, "cancel");
        a.prompt("cancel").await;
        a.permission(json!(1), all_options()).await;
        let first = approval(&mut d).await;
        a.permission(json!("1"), all_options()).await;
        let second = approval(&mut d).await;
        if answer_abort {
            d.send(AgentCommand::Answer {
                approval: first.id.clone(),
                decision: Decision::Abort,
            })
            .unwrap();
        } else {
            d.send(AgentCommand::Interrupt).unwrap();
        }
        for id in [json!(1), json!("1")] {
            assert_eq!(
                a.next().await,
                json!({"jsonrpc":"2.0","id":id,"result":{"outcome":{"outcome":"cancelled"}}})
            );
        }
        let cancel = a.next().await;
        assert_eq!(
            cancel,
            serde_json::from_str::<Value>(fixture("cancel.client.jsonl").trim()).unwrap()
        );
        assert!(
            !d.events()
                .iter()
                .any(|e| matches!(e, EventKind::TurnDone { .. }))
        );
        // Permissions that race with cancellation must not re-open the UI.
        a.permission(json!("late"), all_options()).await;
        assert_eq!(a.next().await["result"]["outcome"]["outcome"], "cancelled");
        a.replay("cancelled.jsonl").await;
        idle(&mut d, 1).await;
        assert_eq!(
            d.events()
                .iter()
                .filter(|e| matches!(e, EventKind::Approval { .. }))
                .count(),
            2
        );
        for id in [first.id, second.id] {
            assert!(d.events().iter().any(|e|matches!(e,EventKind::ApprovalDone{approval,decision:Decision::Abort} if approval==&id)));
        }
        shutdown(&mut d, a).await;
    }
}

#[tokio::test]
async fn streaming_interrupt_accepts_final_updates_before_prompt_response() {
    let (mut d, mut a) = ready().await;
    send(&d, "interrupt");
    a.prompt("interrupt").await;
    a.update(
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"before "}}),
    )
    .await;
    d.wait_until(
        |o| {
            o.iter()
                .any(|o| matches!(o.event(), Some(EventKind::AssistantDelta { .. })))
        },
        T,
    )
    .await
    .unwrap();
    d.send(AgentCommand::Interrupt).unwrap();
    assert_eq!(a.next().await["method"], "session/cancel");
    a.update(
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"after"}}),
    )
    .await;
    a.replay("cancelled.jsonl").await;
    idle(&mut d, 1).await;
    assert!(
        d.events()
            .iter()
            .any(|e| matches!(e,EventKind::AssistantText{text,..} if text=="before after"))
    );
    assert!(
        d.events()
            .iter()
            .any(|e| matches!(e,EventKind::ToolEnd{item,..} if item=="cancel-tool"))
    );
    send(&d, "next");
    let p = a.prompt("next").await;
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 2).await;
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn agent_cancel_request_cancels_only_its_permission() {
    let (mut d, mut a) = ready().await;
    send(&d, "rpc cancel");
    let p = a.prompt("rpc cancel").await;
    a.permission(json!("a"), all_options()).await;
    let ap = approval(&mut d).await;
    a.send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":"a"}}))
        .await;
    assert_eq!(a.next().await["result"]["outcome"]["outcome"], "cancelled");
    a.no_frame().await;
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 1).await;
    assert!(d.events().iter().any(|e|matches!(e,EventKind::ApprovalDone{approval,decision:Decision::Abort} if approval==&ap.id)));
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn filesystem_terminal_and_unknown_requests_get_method_not_found() {
    let (mut d, mut a) = ready().await;
    for (id, method) in [
        (json!(7), "fs/read_text_file"),
        (json!("7"), "fs/write_text_file"),
        (json!(-9), "terminal/create"),
        (json!("output"), "terminal/output"),
        (json!("x"), "_custom/method"),
    ] {
        a.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":{}}))
            .await;
        assert_eq!(
            a.next().await,
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}})
        );
    }
    a.send(json!({"jsonrpc":"2.0","method":"_unknown","params":{}}))
        .await;
    a.no_frame().await;
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn malformed_permissions_and_foreign_sessions_do_not_create_approvals() {
    let (mut d, mut a) = ready().await;
    send(&d, "invalid");
    let p = a.prompt("invalid").await;
    for params in [
        json!({"sessionId":"mock-session","toolCall":{},"options":[]}),
        json!({"sessionId":"mock-session","toolCall":{"toolCallId":"x"},"options":[{"optionId":"x","kind":"allow_once"}]}),
        json!({"sessionId":"mock-session","toolCall":{"toolCallId":"x"},"options":[{"optionId":"x","name":"A","kind":"allow_once"},{"optionId":"x","name":"B","kind":"reject_once"}]}),
    ] {
        a.send(
            json!({"jsonrpc":"2.0","id":30,"method":"session/request_permission","params":params}),
        )
        .await;
        assert_eq!(a.next().await["error"]["code"], -32602);
    }
    a.send(json!({"jsonrpc":"2.0","id":"foreign","method":"session/request_permission","params":{"sessionId":"other","toolCall":{"toolCallId":"x"},"options":[]}})).await;
    assert_eq!(a.next().await["error"]["code"], -32002);
    a.send(json!({"jsonrpc":"2.0","id":null,"method":"session/request_permission","params":{}}))
        .await;
    assert_eq!(a.next().await["error"]["code"], -32600);
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 1).await;
    assert!(!names(&d).contains(&"approval"));
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn unsupported_load_session_starts_new_session_with_exact_notice() {
    let (mut d, mut a) = memory(Some("old-session"));
    let init = a.next().await;
    let fixture: Vec<Value> = fixture("resume-unsupported.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    a.result(&init, fixture[0]["result"].clone()).await;
    let request = a.next().await;
    assert_eq!(request["method"], "session/new");
    a.result(&request, fixture[1]["result"].clone()).await;
    d.wait_until(
        |o| {
            o.iter()
                .any(|o| matches!(o,AgentOutput::NativeId(id) if id=="mock-session"))
        },
        T,
    )
    .await
    .unwrap();
    assert!(d.events().iter().any(|e|matches!(e,EventKind::Notice{level:NoticeLevel::Info,text} if text=="该 agent 不支持恢复，已开始新会话")));
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn load_session_replays_history_before_becoming_ready() {
    let (mut d, mut a) = memory(Some("mock-session"));
    a.initialize(true, false).await;
    let load = a.next().await;
    assert_eq!(load["method"], "session/load");
    assert_eq!(
        load["params"],
        json!({"sessionId":"mock-session","cwd":a.cwd.to_str().unwrap(),"mcpServers":[]})
    );
    a.replay("load-session.jsonl").await;
    d.wait_until(
        |o| {
            o.iter()
                .any(|o| matches!(o,AgentOutput::NativeId(id) if id=="mock-session"))
        },
        T,
    )
    .await
    .unwrap();
    assert!(
        d.events()
            .iter()
            .any(|e| matches!(e,EventKind::AssistantText{text,..} if text=="历史回复"))
    );
    assert!(!names(&d).contains(&"user_text"));
    assert!(!names(&d).contains(&"turn_done"));
    assert!(!names(&d).contains(&"idle"));
    send(&d, "continue");
    let p = a.prompt("continue").await;
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 1).await;
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn authentication_error_emits_login_notice_and_does_not_start_authenticate() {
    let (mut d, mut a) = memory(None);
    a.initialize(false, true).await;
    a.new_session().await;
    send(&d, "auth");
    a.prompt("auth").await;
    a.replay("auth-error.jsonl").await;
    idle(&mut d, 1).await;
    assert!(d.events().iter().any(|e|matches!(e,EventKind::Notice{level:NoticeLevel::Error,text} if text=="请先在电脑上登录该 agent")));
    a.no_frame().await;
    send(&d, "retry");
    let p = a.prompt("retry").await;
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 2).await;
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn authentication_can_fail_during_session_creation_too() {
    let (mut d, mut a) = memory(None);
    a.initialize(false, true).await;
    let new = a.next().await;
    a.send(json!({"jsonrpc":"2.0","id":new["id"],"error":{"code":-32000,"message":"Authentication required"}})).await;
    assert!(
        exit(&mut d)
            .await
            .1
            .unwrap()
            .contains("请先在电脑上登录该 agent")
    );
    assert!(names(&d).contains(&"error"));
    a.stopped().await;
}

#[tokio::test]
async fn response_ids_are_typed_and_protocol_version_is_negotiated() {
    let (mut d, mut a) = memory(None);
    let init = a.next().await;
    a.send(json!({"jsonrpc":"2.0","id":"1","result":{"protocolVersion":1}}))
        .await;
    a.send(json!({"jsonrpc":"2.0","id":200,"result":{"protocolVersion":1}}))
        .await;
    a.no_frame().await;
    a.result(&init, json!({"protocolVersion":2})).await;
    assert!(exit(&mut d).await.1.unwrap().contains("protocolVersion"));
    a.stopped().await;
}

#[tokio::test]
async fn generic_prompt_errors_and_cancelled_errors_complete_the_turn() {
    let (mut d, mut a) = ready().await;
    for (round, code) in [(1, -32603), (2, -32800), (3, -32000)] {
        send(&d, "error");
        let p = a.prompt("error").await;
        a.send(json!({"jsonrpc":"2.0","id":p["id"],"error":{"code":code,"message":"failure"}}))
            .await;
        idle(&mut d, round).await;
    }
    assert_eq!(
        d.events()
            .iter()
            .filter(|e| matches!(
                e,
                EventKind::Notice {
                    level: NoticeLevel::Error,
                    ..
                }
            ))
            .count(),
        2
    );
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn send_is_reported_immediately_during_setup_and_prompts_are_serialized() {
    let (mut d, mut a) = memory(None);
    send(&d, "one");
    send(&d, "two");
    d.wait_until(
        |o| {
            o.iter()
                .filter(|o| {
                    matches!(
                        o.event(),
                        Some(EventKind::State {
                            state: SessionState::Working,
                            ..
                        })
                    )
                })
                .count()
                == 2
        },
        T,
    )
    .await
    .unwrap();
    a.initialize(false, false).await;
    a.new_session().await;
    let p = a.prompt("one").await;
    a.no_frame().await;
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    let p = a.prompt("two").await;
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 2).await;
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn interrupt_before_initialization_cancels_queued_send_without_prompting() {
    let (mut d, mut a) = memory(None);
    send(&d, "queued");
    d.send(AgentCommand::Interrupt).unwrap();
    idle(&mut d, 1).await;
    a.initialize(false, false).await;
    a.new_session().await;
    d.wait_until(
        |o| o.iter().any(|o| matches!(o, AgentOutput::NativeId(_))),
        T,
    )
    .await
    .unwrap();
    a.no_frame().await;
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn shutdown_and_dropping_command_sender_stop_even_during_initialization() {
    for drop_sender in [false, true] {
        let (mut d, mut a) = memory(None);
        assert_eq!(a.next().await["method"], "initialize");
        if drop_sender {
            let (replacement, _) = mpsc::unbounded_channel();
            drop(std::mem::replace(&mut d.cmd, replacement));
        } else {
            d.send(AgentCommand::Shutdown).unwrap();
        }
        assert_eq!(exit(&mut d).await, (Some(0), None));
        a.stopped().await;
    }
}

#[tokio::test]
async fn dropping_output_receiver_stops_connection() {
    let (d, mut a) = memory(None);
    a.next().await;
    drop(d.rx);
    a.stopped().await;
}

#[tokio::test]
async fn shutdown_cancels_outstanding_permission_before_closing_transport() {
    let (mut d, mut a) = ready().await;
    send(&d, "shutdown");
    a.prompt("shutdown").await;
    a.permission(json!(33), all_options()).await;
    approval(&mut d).await;
    d.send(AgentCommand::Shutdown).unwrap();
    assert_eq!(a.next().await["result"]["outcome"]["outcome"], "cancelled");
    assert_eq!(a.next().await["method"], "session/cancel");
    assert_eq!(exit(&mut d).await, (Some(0), None));
    a.stopped().await;
}

#[tokio::test]
async fn ignores_unknown_updates_foreign_sessions_and_non_text_content() {
    let (mut d, mut a) = ready().await;
    send(&d, "ignore");
    let p = a.prompt("ignore").await;
    a.send(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"other","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"foreign"}}}})).await;
    a.update(json!({"sessionUpdate":"_future","content":{"type":"text","text":"unknown"}}))
        .await;
    a.update(json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"user"}}))
        .await;
    a.update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"image","data":"AA==","mimeType":"image/png"}})).await;
    a.result(&p, json!({"stopReason":"end_turn"})).await;
    idle(&mut d, 1).await;
    assert_eq!(names(&d), ["working", "turn_done", "idle"]);
    shutdown(&mut d, a).await;
}

#[tokio::test]
async fn malformed_stdout_and_missing_required_fields_exit_instead_of_panicking() {
    for frame in [
        "not json",
        "[]",
        "{\"id\":1,\"result\":{}}",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{},\"error\":{}}",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}",
    ] {
        let (mut d, mut a) = memory(None);
        a.next().await;
        a.io.send_line(frame).await.unwrap();
        assert!(exit(&mut d).await.1.is_some());
        a.stopped().await;
    }
}

#[tokio::test]
async fn oversized_stdout_frame_is_bounded_and_reported() {
    let (mut d, mut a) = memory(None);
    a.next().await;
    let frame = "x".repeat(tether_proto::MAX_FRAME + 1);
    // The reader stops at MAX_FRAME+1, without accumulating an unbounded line.
    let _ = a.io.send_line(&frame).await;
    assert!(exit(&mut d).await.1.unwrap().contains("exceeds 8 MiB"));
    a.stopped().await;
}

#[tokio::test]
async fn eof_with_pending_request_is_an_error_and_exited_is_last() {
    let (mut d, a) = ready().await;
    send(&d, "eof");
    let MockAgent { io, task, .. } = a;
    drop(io);
    let (_, error) = exit(&mut d).await;
    assert!(error.is_some());
    task.await.unwrap();
    assert!(d.rx.recv().await.is_none());
}

#[tokio::test]
async fn transport_accepts_fragmented_crlf_and_rejects_invalid_utf8() {
    use tokio::io::{AsyncWriteExt, BufReader};
    let (client, mut agent) = tokio::io::duplex(128);
    let (tx, mut rx) = mpsc::channel(2);
    let task = tokio::spawn(crate::rpc::read(BufReader::new(client), tx));
    agent.write_all(b"{\"jsonrpc\":\"2.0\",").await.unwrap();
    tokio::task::yield_now().await;
    agent
        .write_all(b"\"id\":1,\"result\":{}}\r\n")
        .await
        .unwrap();
    assert_eq!(rx.recv().await.unwrap().unwrap()["id"], 1);
    agent.write_all(b"\xff\n").await.unwrap();
    assert!(rx.recv().await.unwrap().is_err());
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn initialization_and_session_setup_have_bounded_deadlines() {
    for after_initialize in [false, true] {
        let (mut d, mut a) = memory(None);
        if after_initialize {
            a.initialize(false, false).await;
            assert_eq!(a.next().await["method"], "session/new");
        } else {
            assert_eq!(a.next().await["method"], "initialize");
        }
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(exit(&mut d).await.1.unwrap().contains("timed out"));
        a.stopped().await;
    }
}

#[tokio::test(start_paused = true)]
async fn cancellation_has_bounded_deadline_but_normal_prompts_do_not() {
    let (mut d, mut a) = ready().await;
    send(&d, "slow");
    a.prompt("slow").await;
    tokio::time::advance(Duration::from_secs(3600)).await;
    // A model may work for a long time. Only cancellation adds a deadline.
    d.send(AgentCommand::Interrupt).unwrap();
    assert_eq!(a.next().await["method"], "session/cancel");
    tokio::time::advance(Duration::from_secs(6)).await;
    assert!(exit(&mut d).await.1.unwrap().contains("timed out"));
    a.stopped().await;
}

#[tokio::test(start_paused = true)]
async fn blocked_stdin_has_bounded_deadline_and_oversize_input_is_rejected() {
    let (mut writer, _reader) = tokio::io::duplex(1);
    let task =
        tokio::spawn(async move { crate::rpc::write(&mut writer, json!({"hello":"world"})).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(6)).await;
    assert!(
        task.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    let mut sink = tokio::io::sink();
    assert!(
        crate::rpc::write(
            &mut sink,
            json!({"text":"x".repeat(tether_proto::MAX_FRAME)})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("exceeds 8 MiB")
    );
}

#[tokio::test]
async fn failed_transport_retires_outstanding_approvals() {
    let (mut d, mut a) = ready().await;
    send(&d, "crash awaiting permission");
    a.prompt("crash awaiting permission").await;
    a.permission(json!(8), all_options()).await;
    let ap = approval(&mut d).await;
    a.io.send_line("broken stdout").await.unwrap();
    assert!(exit(&mut d).await.1.is_some());
    assert!(d.events().iter().any(|e|matches!(e,EventKind::ApprovalDone{approval,decision:Decision::Abort} if approval==&ap.id)));
    a.stopped().await;
}

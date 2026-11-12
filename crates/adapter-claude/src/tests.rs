//! End-to-end protocol replays. No test starts Claude (or any other process).

use super::*;
use serde_json::{Value, json};
use tether_agent_api::{
    AgentAdapter,
    testkit::{LineEnd, line_duplex},
};
use tether_proto::{Approval, Decision};
use tokio::task::JoinHandle;

const T: Duration = Duration::from_secs(5);
const ACK: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_1","response":{}}}"#;
const QA: &str = include_str!("../../../fixtures/claude/qa.jsonl");
const BASH: &str = include_str!("../../../fixtures/claude/bash-allow.jsonl");
const EDIT: &str = include_str!("../../../fixtures/claude/edit-deny.jsonl");
const SESSION: &str = include_str!("../../../fixtures/claude/session-allow.jsonl");
const ABORT: &str = include_str!("../../../fixtures/claude/abort.jsonl");
const CANCEL: &str = include_str!("../../../fixtures/claude/cancel.jsonl");

struct Harness {
    cmd: UnboundedSender<AgentCommand>,
    rx: UnboundedReceiver<AgentOutput>,
    peer: LineEnd,
    task: JoinHandle<Result<Stop>>,
    outputs: Vec<AgentOutput>,
}

impl Harness {
    async fn new() -> Self {
        Self::with_timeout(INIT_TIMEOUT).await
    }

    async fn with_timeout(timeout: Duration) -> Self {
        let (transport, peer) = line_duplex();
        let (reader, writer) = transport.into_parts();
        let (cmd, commands) = mpsc::unbounded_channel();
        let (out, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let result = run_io(
                reader,
                writer,
                commands,
                &out,
                Some("fixture-cwd".into()),
                timeout,
            )
            .await;
            // A replay has no OS exit code. Use 0 solely for the in-memory driver.
            let _ = out.send(AgentOutput::Exited {
                code: result.as_ref().ok().map(|_| 0),
                error: result.as_ref().err().map(|e| format!("{e:#}")),
            });
            result
        });
        let mut h = Self {
            cmd,
            rx,
            peer,
            task,
            outputs: Vec::new(),
        };
        assert_eq!(
            h.wire().await,
            json!({"type":"control_request", "request_id":"req_1",
            "request":{"subtype":"initialize", "hooks":null}})
        );
        h
    }

    fn send(&self, cmd: AgentCommand) {
        self.cmd.send(cmd).unwrap();
    }

    async fn wire(&mut self) -> Value {
        tokio::time::timeout(T, self.peer.next_json())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }

    async fn no_wire(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(30), self.peer.next_line())
                .await
                .is_err()
        );
    }

    async fn wait(&mut self, pick: impl Fn(&AgentOutput) -> bool) -> AgentOutput {
        tokio::time::timeout(T, async {
            loop {
                let output = self.rx.recv().await.expect("output channel closed");
                self.outputs.push(output.clone());
                if pick(&output) {
                    break output;
                }
            }
        })
        .await
        .expect("replay timed out")
    }

    async fn event(&mut self, pick: impl Fn(&EventKind) -> bool) -> EventKind {
        self.wait(|output| output.event().is_some_and(&pick))
            .await
            .event()
            .unwrap()
            .clone()
    }

    async fn state(&mut self, wanted: SessionState) {
        self.event(|e| matches!(e, EventKind::State { state, .. } if *state == wanted))
            .await;
    }

    async fn begin(&mut self, fixture: &str, text: &str) {
        self.send(AgentCommand::Send {
            text: text.into(),
            images: Vec::new(),
        });
        self.state(SessionState::Working).await;
        self.peer.send_fixture(fixture).await.unwrap();
        assert_eq!(
            self.wire().await,
            json!({"type":"user", "message":{"role":"user", "content":text},
            "parent_tool_use_id":null, "session_id":""})
        );
    }

    async fn permission(&mut self, fixture: &str) -> (Approval, String) {
        let (head, tail) = fixture.split_once("// after_answer").unwrap();
        self.begin(head, "fixture prompt").await;
        let EventKind::Approval { approval } = self
            .event(|e| matches!(e, EventKind::Approval { .. }))
            .await
        else {
            unreachable!()
        };
        self.state(SessionState::NeedsYou).await;
        (approval, tail.into())
    }

    async fn answer(&mut self, id: &str, decision: Decision, expected: Value) {
        self.send(AgentCommand::Answer {
            approval: id.into(),
            decision,
        });
        assert_eq!(self.wire().await, expected);
        assert_eq!(
            self.event(|e| matches!(e, EventKind::ApprovalDone { .. }))
                .await,
            EventKind::ApprovalDone {
                approval: id.into(),
                decision
            }
        );
        self.state(SessionState::Working).await;
    }

    async fn finish(mut self) -> Vec<AgentOutput> {
        self.send(AgentCommand::Shutdown);
        let exit = self.wait(|o| matches!(o, AgentOutput::Exited { .. })).await;
        assert_eq!(
            exit,
            AgentOutput::Exited {
                code: Some(0),
                error: None
            }
        );
        assert_eq!(
            tokio::time::timeout(T, self.task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Stop::Shutdown
        );
        assert!(
            !self
                .outputs
                .iter()
                .any(|o| matches!(o.event(), Some(EventKind::UserText { .. })))
        );
        self.outputs
    }
}

fn response(id: &str, body: Value) -> Value {
    json!({"type":"control_response", "response":{"subtype":"success", "request_id":id, "response":body}})
}

fn permission_request(id: &str, command: &str) -> Value {
    json!({"type":"control_request", "request_id":id, "request":{"subtype":"can_use_tool",
        "tool_name":"Bash", "input":{"command":command}, "permission_suggestions":[]}})
}

#[tokio::test]
async fn qa_replay_has_exact_event_order_and_no_user_text() {
    let mut h = Harness::new().await;
    h.begin(QA, "你好").await;
    h.state(SessionState::Idle).await;
    assert_eq!(
        h.outputs,
        [
            state(SessionState::Working),
            AgentOutput::NativeId("claude-qa".into()),
            AgentOutput::Event(EventKind::AssistantDelta {
                item: "msg-qa:0".into(),
                text: "你好".into()
            }),
            AgentOutput::Event(EventKind::AssistantDelta {
                item: "msg-qa:0".into(),
                text: "，手机".into()
            }),
            AgentOutput::Event(EventKind::AssistantText {
                item: "msg-qa:0".into(),
                text: "你好，手机".into()
            }),
            AgentOutput::Event(EventKind::Thinking {
                item: "msg-qa:1".into(),
                text: "保持简短回答".into()
            }),
            AgentOutput::Event(EventKind::TurnDone {
                summary: Some("耗时 250 ms，费用 $0.001000".into())
            }),
            state(SessionState::Idle),
        ]
    );
    h.finish().await;
}

#[tokio::test]
async fn bash_allow_once_replay_preserves_exact_input_and_envelope() {
    let mut h = Harness::new().await;
    let (card, tail) = h.permission(BASH).await;
    assert_eq!(card.title, "运行：cargo test --offline");
    assert_eq!(card.detail, "cargo test --offline\n运行本地测试");
    assert_eq!(card.cwd.as_deref(), Some("fixture-cwd"));
    assert_eq!(
        card.choices,
        [Decision::AllowOnce, Decision::Deny, Decision::Abort]
    );
    h.answer(
        &card.id,
        Decision::AllowOnce,
        response(
            "perm-bash",
            json!({"behavior":"allow",
        "updatedInput":{"command":"cargo test --offline", "description":"运行本地测试"}}),
        ),
    )
    .await;
    h.peer.send_fixture(&tail).await.unwrap();
    h.state(SessionState::Idle).await;
    assert!(h.outputs.iter().any(|o| o.event()
        == Some(&EventKind::ToolStart {
            item: "tool-bash".into(),
            tool: "Bash".into(),
            summary: "cargo test --offline".into()
        })));
    assert!(h.outputs.iter().any(|o| o.event()
        == Some(&EventKind::ToolEnd {
            item: "tool-bash".into(),
            ok: true,
            output: "test result: ok".into()
        })));
    h.finish().await;
}

#[tokio::test]
async fn edit_deny_replay_uses_exact_chinese_message() {
    let mut h = Harness::new().await;
    let (card, tail) = h.permission(EDIT).await;
    assert_eq!(card.title, "修改 src/main.rs");
    assert!(card.detail.contains("old_string:\nold\nnew_string:\nnew"));
    h.answer(
        &card.id,
        Decision::Deny,
        response(
            "perm-edit",
            json!({"behavior":"deny", "message":"用户在手机上拒绝"}),
        ),
    )
    .await;
    h.peer.send_fixture(&tail).await.unwrap();
    h.state(SessionState::Idle).await;
    assert!(h.outputs.iter().any(|o| matches!(o.event(), Some(EventKind::ToolEnd { ok:false, output, .. }) if output == "用户拒绝修改")));
    h.finish().await;
}

#[tokio::test]
async fn session_allow_replay_echoes_original_permission_suggestions() {
    let mut h = Harness::new().await;
    let (card, tail) = h.permission(SESSION).await;
    assert_eq!(
        card.choices,
        [
            Decision::AllowOnce,
            Decision::AllowSession,
            Decision::Deny,
            Decision::Abort
        ]
    );
    h.answer(&card.id, Decision::AllowSession, response("perm-session", json!({"behavior":"allow",
        "updatedInput":{"command":"git status", "description":"查看工作区"},
        "updatedPermissions":[{"type":"addRules", "rules":[{"toolName":"Bash", "ruleContent":"git status"}],
            "behavior":"allow", "destination":"session"}]}))).await;
    h.peer.send_fixture(&tail).await.unwrap();
    h.state(SessionState::Idle).await;
    h.finish().await;
}

#[tokio::test]
async fn abort_replay_sets_interrupt_inside_deny_not_an_extra_request() {
    let mut h = Harness::new().await;
    let (card, tail) = h.permission(ABORT).await;
    assert!(!card.choices.contains(&Decision::AllowSession));
    h.answer(
        &card.id,
        Decision::Abort,
        response(
            "perm-abort",
            json!({"behavior":"deny", "message":"用户在手机上拒绝并停止", "interrupt":true}),
        ),
    )
    .await;
    h.no_wire().await;
    h.peer.send_fixture(&tail).await.unwrap();
    h.state(SessionState::Idle).await;
    assert!(h.outputs.iter().any(|o| matches!(
        o.event(),
        Some(EventKind::Notice {
            level: NoticeLevel::Error,
            ..
        })
    )));
    h.finish().await;
}

#[tokio::test]
async fn unsupported_session_decision_does_not_consume_the_request() {
    let mut h = Harness::new().await;
    let (card, tail) = h.permission(EDIT).await;
    h.send(AgentCommand::Answer {
        approval: card.id.clone(),
        decision: Decision::AllowSession,
    });
    h.event(|e| {
        matches!(
            e,
            EventKind::Notice {
                level: NoticeLevel::Warn,
                ..
            }
        )
    })
    .await;
    h.no_wire().await;
    assert!(
        !h.outputs
            .iter()
            .any(|o| matches!(o.event(), Some(EventKind::ApprovalDone { .. })))
    );
    h.answer(
        &card.id,
        Decision::Deny,
        response(
            "perm-edit",
            json!({"behavior":"deny", "message":"用户在手机上拒绝"}),
        ),
    )
    .await;
    h.peer.send_fixture(&tail).await.unwrap();
    h.state(SessionState::Idle).await;
    h.finish().await;
}

#[tokio::test]
async fn cancelled_approval_is_removed_and_late_answer_is_not_written() {
    let mut h = Harness::new().await;
    let (head, rest) = CANCEL.split_once("// after_cancel").unwrap();
    let (cancel, tail) = rest.split_once("// after_answer").unwrap();
    h.begin(head, "cancel").await;
    h.event(|e| matches!(e, EventKind::Approval { .. })).await;
    h.state(SessionState::NeedsYou).await;
    h.peer.send_fixture(cancel).await.unwrap();
    assert_eq!(
        h.event(|e| matches!(
            e,
            EventKind::Notice {
                level: NoticeLevel::Info,
                ..
            }
        ))
        .await,
        EventKind::Notice {
            level: NoticeLevel::Info,
            text: "审批已取消：perm-cancel".into()
        }
    );
    h.state(SessionState::Working).await;
    h.send(AgentCommand::Answer {
        approval: "perm-cancel".into(),
        decision: Decision::AllowOnce,
    });
    h.event(|e| {
        matches!(
            e,
            EventKind::Notice {
                level: NoticeLevel::Warn,
                ..
            }
        )
    })
    .await;
    h.no_wire().await;
    h.peer.send_fixture(tail).await.unwrap();
    h.state(SessionState::Idle).await;
    assert!(
        !h.outputs
            .iter()
            .any(|o| matches!(o.event(), Some(EventKind::ApprovalDone { .. })))
    );
    h.finish().await;
}

#[tokio::test]
async fn interrupt_replay_matches_control_ids_and_waits_for_result() {
    let fixture = include_str!("../../../fixtures/claude/interrupt.jsonl");
    let (head, tail) = fixture.split_once("// after_interrupt").unwrap();
    let mut h = Harness::new().await;
    h.begin(head, "long task").await;
    h.event(|e| matches!(e, EventKind::AssistantDelta { .. }))
        .await;
    h.send(AgentCommand::Interrupt);
    assert_eq!(
        h.wire().await,
        json!({"type":"control_request", "request_id":"req_2", "request":{"subtype":"interrupt"}})
    );
    assert!(
        !h.outputs
            .iter()
            .any(|o| matches!(o.event(), Some(EventKind::TurnDone { .. })))
    );
    h.peer.send_fixture(tail).await.unwrap();
    h.state(SessionState::Idle).await;
    h.send(AgentCommand::Interrupt);
    assert_eq!(
        h.wire().await,
        json!({"type":"control_request", "request_id":"req_3", "request":{"subtype":"interrupt"}})
    );
    h.peer.send_json(&json!({"type":"control_response", "response":{"subtype":"error", "request_id":"req_3", "error":"fixture rejected interrupt"}})).await.unwrap();
    h.event(|e| matches!(e, EventKind::Notice { level:NoticeLevel::Error, text } if text.contains("fixture rejected interrupt"))).await;
    h.finish().await;
}

#[tokio::test]
async fn error_result_replay_emits_error_then_turn_done_then_idle() {
    let mut h = Harness::new().await;
    h.begin(
        include_str!("../../../fixtures/claude/result-error.jsonl"),
        "error",
    )
    .await;
    h.state(SessionState::Idle).await;
    assert_eq!(
        &h.outputs[2..],
        [
            notice(NoticeLevel::Error, "Maximum turns reached\nNo final answer"),
            AgentOutput::Event(EventKind::TurnDone {
                summary: Some("耗时 2000 ms，费用 $0.004000".into())
            }),
            state(SessionState::Idle),
        ]
    );
    h.finish().await;
}

#[tokio::test]
async fn resume_replay_preserves_native_id_and_command_configuration() {
    let opts = SpawnOptions {
        cwd: std::path::PathBuf::from("fixture-cwd"),
        resume: Some("claude-saved-session".into()),
        args: vec!["--model".into(), "fixture-model".into()],
        env: vec![("TETHER_FIXTURE".into(), "value".into())],
        ..Default::default()
    };
    let command = command(Path::new("fixture-program-never-run"), &opts);
    let std = command.as_std();
    assert_eq!(std.get_program(), "fixture-program-never-run");
    assert_eq!(std.get_current_dir(), Some(opts.cwd.as_path()));
    assert_eq!(
        std.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--resume=claude-saved-session",
            "--model",
            "fixture-model"
        ]
    );
    assert!(std.get_envs().any(
        |(key, value)| key == "TETHER_FIXTURE" && value == Some(std::ffi::OsStr::new("value"))
    ));
    let mut h = Harness::new().await;
    h.begin(
        include_str!("../../../fixtures/claude/resume.jsonl"),
        "continue",
    )
    .await;
    h.state(SessionState::Idle).await;
    assert!(
        h.outputs
            .contains(&AgentOutput::NativeId("claude-saved-session".into()))
    );
    h.finish().await;
}

#[tokio::test]
async fn initialize_gates_user_and_interrupt_until_matching_success() {
    let mut h = Harness::new().await;
    h.send(AgentCommand::Send {
        text: "one".into(),
        images: Vec::new(),
    });
    h.state(SessionState::Working).await;
    h.send(AgentCommand::Interrupt);
    h.send(AgentCommand::Send {
        text: "two".into(),
        images: Vec::new(),
    });
    h.state(SessionState::Working).await;
    h.no_wire().await;
    h.peer.send_json(&json!({"type":"control_response", "response":{"subtype":"success", "request_id":"wrong"}})).await.unwrap();
    h.event(|e| {
        matches!(
            e,
            EventKind::Notice {
                level: NoticeLevel::Warn,
                ..
            }
        )
    })
    .await;
    h.no_wire().await;
    h.peer.send_line(ACK).await.unwrap();
    assert_eq!(h.wire().await, protocol::user("one"));
    assert_eq!(h.wire().await, protocol::interrupt("req_2"));
    assert_eq!(h.wire().await, protocol::user("two"));
    h.finish().await;
}

#[tokio::test]
async fn initialization_failure_exits_without_sending_queued_user() {
    let mut h = Harness::new().await;
    h.send(AgentCommand::Send {
        text: "must not be written".into(),
        images: Vec::new(),
    });
    h.state(SessionState::Working).await;
    h.peer
        .send_fixture(include_str!(
            "../../../fixtures/claude/initialize-error.jsonl"
        ))
        .await
        .unwrap();
    let exit = h.wait(|o| matches!(o, AgentOutput::Exited { .. })).await;
    assert!(
        matches!(exit, AgentOutput::Exited { code:None, error:Some(error) } if error.contains("fixture initialization rejected"))
    );
    assert!(h.peer.next_line().await.unwrap().is_none());
    assert!(h.task.await.unwrap().is_err());
}

#[tokio::test]
async fn initialization_timeout_is_bounded() {
    let mut h = Harness::with_timeout(Duration::from_millis(50)).await;
    let exit = h.wait(|o| matches!(o, AgentOutput::Exited { .. })).await;
    assert!(
        matches!(exit, AgentOutput::Exited { error:Some(error), .. } if error == "Claude initialize timed out")
    );
    assert!(h.task.await.unwrap().is_err());
}

#[tokio::test]
async fn eof_before_initialize_is_an_error_after_initialize_is_clean() {
    for initialized in [false, true] {
        let mut h = Harness::new().await;
        if initialized {
            h.peer.send_line(ACK).await.unwrap();
            h.send(AgentCommand::Send {
                text: "one".into(),
                images: Vec::new(),
            });
            h.state(SessionState::Working).await;
            assert_eq!(h.wire().await, protocol::user("one"));
        }
        drop(h.peer);
        let exit = tokio::time::timeout(T, h.rx.recv()).await.unwrap().unwrap();
        if initialized {
            assert_eq!(
                exit,
                AgentOutput::Exited {
                    code: Some(0),
                    error: None
                }
            );
            assert_eq!(h.task.await.unwrap().unwrap(), Stop::Eof);
        } else {
            assert!(
                matches!(exit, AgentOutput::Exited { error:Some(error), .. } if error.contains("before initialize"))
            );
            assert!(h.task.await.unwrap().is_err());
        }
        assert!(h.rx.recv().await.is_none());
    }
}

#[tokio::test]
async fn shutdown_or_dropped_commands_stops_even_during_initialization() {
    for explicit in [true, false] {
        let mut h = Harness::new().await;
        if explicit {
            h.send(AgentCommand::Shutdown);
        } else {
            drop(h.cmd);
        }
        assert_eq!(
            tokio::time::timeout(T, h.rx.recv()).await.unwrap().unwrap(),
            AgentOutput::Exited {
                code: Some(0),
                error: None
            }
        );
        assert_eq!(h.task.await.unwrap().unwrap(), Stop::Shutdown);
        assert!(h.peer.next_line().await.unwrap().is_none());
        assert!(h.rx.recv().await.is_none());
    }
}

#[tokio::test]
async fn dropped_output_receiver_stops_the_io_loop() {
    let h = Harness::new().await;
    drop(h.rx);
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
async fn broken_writer_returns_an_error_instead_of_panicking() {
    let (local, _reading_peer) = line_duplex();
    let (reader, _) = local.into_parts();
    let (failed_writer, peer) = line_duplex();
    drop(peer);
    let (_, writer) = failed_writer.into_parts();
    let (_cmd, rx) = mpsc::unbounded_channel();
    let (out, _rx) = mpsc::unbounded_channel();
    assert!(
        run_io(reader, writer, rx, &out, None, T)
            .await
            .unwrap_err()
            .to_string()
            .contains("write Claude JSONL")
    );
}

#[tokio::test]
async fn backpressured_stdin_does_not_block_shutdown_or_sender_drop() {
    for explicit in [true, false] {
        let mut h = Harness::new().await;
        h.peer.send_line(ACK).await.unwrap();
        h.send(AgentCommand::Send {
            text: "x".repeat(2 << 20),
            images: Vec::new(),
        });
        h.state(SessionState::Working).await;
        // The JSON record exceeds the duplex's 1 MiB capacity. Never read it.
        tokio::time::sleep(Duration::from_millis(10)).await;
        h.send(AgentCommand::Send {
            text: "still responsive".into(),
            images: Vec::new(),
        });
        h.state(SessionState::Working).await;
        if explicit {
            h.send(AgentCommand::Shutdown);
        } else {
            drop(h.cmd);
        }
        assert_eq!(
            tokio::time::timeout(T, h.rx.recv()).await.unwrap().unwrap(),
            AgentOutput::Exited {
                code: Some(0),
                error: None
            }
        );
        assert_eq!(
            tokio::time::timeout(T, h.task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Stop::Shutdown
        );
    }
}

#[tokio::test]
async fn invalid_json_and_unknown_messages_do_not_prevent_next_valid_turn() {
    let mut h = Harness::new().await;
    h.peer.send_line("not-json").await.unwrap();
    h.event(|e| {
        matches!(
            e,
            EventKind::Notice {
                level: NoticeLevel::Warn,
                ..
            }
        )
    })
    .await;
    h.peer
        .send_line(r#"{"type":"stream_event"}"#)
        .await
        .unwrap();
    h.event(|e| {
        matches!(
            e,
            EventKind::Notice {
                level: NoticeLevel::Warn,
                ..
            }
        )
    })
    .await;
    h.peer
        .send_line(r#"{"type":"future_message","anything":true}"#)
        .await
        .unwrap();
    h.peer.send_line("").await.unwrap();
    h.begin(QA, "valid").await;
    h.state(SessionState::Idle).await;
    h.finish().await;
}

#[tokio::test]
async fn simultaneous_permissions_match_ids_and_keep_their_own_original_inputs() {
    let mut h = Harness::new().await;
    h.begin(ACK, "two permissions").await;
    for id in ["one", "two"] {
        h.peer.send_json(&permission_request(id, id)).await.unwrap();
        h.event(|e| matches!(e, EventKind::Approval { .. })).await;
        h.state(SessionState::NeedsYou).await;
    }
    h.answer(
        "two",
        Decision::AllowOnce,
        response(
            "two",
            json!({"behavior":"allow", "updatedInput":{"command":"two"}}),
        ),
    )
    .await;
    h.answer(
        "one",
        Decision::AllowOnce,
        response(
            "one",
            json!({"behavior":"allow", "updatedInput":{"command":"one"}}),
        ),
    )
    .await;
    h.send(AgentCommand::Answer {
        approval: "two".into(),
        decision: Decision::Deny,
    });
    h.event(|e| {
        matches!(
            e,
            EventKind::Notice {
                level: NoticeLevel::Warn,
                ..
            }
        )
    })
    .await;
    h.no_wire().await;
    h.finish().await;
}

#[tokio::test]
async fn duplicate_permission_does_not_replace_original_input() {
    let mut h = Harness::new().await;
    let (card, tail) = h.permission(BASH).await;
    h.peer
        .send_json(&permission_request(&card.id, "different input"))
        .await
        .unwrap();
    h.event(|e| matches!(e, EventKind::Notice { level:NoticeLevel::Warn, text } if text.contains("重复审批"))).await;
    h.answer(
        &card.id,
        Decision::AllowOnce,
        response(
            "perm-bash",
            json!({"behavior":"allow",
        "updatedInput":{"command":"cargo test --offline", "description":"运行本地测试"}}),
        ),
    )
    .await;
    h.peer.send_fixture(&tail).await.unwrap();
    h.state(SessionState::Idle).await;
    h.finish().await;
}

#[tokio::test]
async fn result_invalidates_unanswered_permissions() {
    let mut h = Harness::new().await;
    let (_, tail) = h.permission(BASH).await;
    h.peer.send_fixture(&tail).await.unwrap();
    h.state(SessionState::Idle).await;
    h.send(AgentCommand::Answer {
        approval: "perm-bash".into(),
        decision: Decision::AllowOnce,
    });
    h.event(|e| {
        matches!(
            e,
            EventKind::Notice {
                level: NoticeLevel::Warn,
                ..
            }
        )
    })
    .await;
    h.no_wire().await;
    h.finish().await;
}

#[tokio::test]
async fn malformed_or_unsupported_control_requests_receive_error_not_approval() {
    let mut h = Harness::new().await;
    h.begin(ACK, "controls").await;
    for request in [
        json!({"subtype":"can_use_tool", "tool_name":"Bash"}),
        json!({"subtype":"unsupported"}),
    ] {
        h.peer
            .send_json(&json!({"type":"control_request", "request_id":"bad", "request":request}))
            .await
            .unwrap();
        let reply = h.wire().await;
        assert_eq!(reply["type"], "control_response");
        assert_eq!(reply["response"]["subtype"], "error");
        assert_eq!(reply["response"]["request_id"], "bad");
        assert!(reply["response"]["error"].is_string());
        h.event(|e| {
            matches!(
                e,
                EventKind::Notice {
                    level: NoticeLevel::Warn,
                    ..
                }
            )
        })
        .await;
    }
    assert!(
        !h.outputs
            .iter()
            .any(|o| matches!(o.event(), Some(EventKind::Approval { .. })))
    );
    h.finish().await;
}

#[tokio::test]
async fn stderr_is_drained_with_bounded_storage_using_memory_only() {
    let (local, mut peer) = line_duplex();
    let (reader, writer) = local.into_parts();
    drop(writer);
    let task = tokio::spawn(stderr_tail(reader));
    peer.send_line(&format!("{}END", "x".repeat(20000)))
        .await
        .unwrap();
    drop(peer);
    let tail = task.await.unwrap();
    assert_eq!(tail.len(), 8192);
    assert!(tail.ends_with("END\n"));
}

#[test]
fn public_adapter_info_preserves_the_frozen_contract() {
    let info = crate::adapter().info();
    assert_eq!(info.id, "claude");
    assert_eq!(info.name, "Claude Agent");
    assert_eq!(info.kind, tether_proto::AgentKind::Claude);
    assert!(info.chat);
    assert_eq!(info.terminal_command, ["claude"]);
    assert!(
        crate::ClaudeAdapter::default()
            .spawn(SpawnOptions::default(), mpsc::unbounded_channel().0)
            .is_err()
    );
}

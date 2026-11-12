use super::*;
use serde_json::json;
use tether_proto::ApprovalKind;

/// A turn the phone started in the desktop app's 「你好」 thread: follow
/// snapshot, the turn's patches, two mid-turn snapshots another window asked for.
const TURN: &str = include_str!("../../../fixtures/codex/desktop-turn.jsonl");
const THREAD: &str = "01a1bf37-26ff-7040-8161-fe2910fb9d02";
const SENT: &str = "测试：手机经桌面版发送。请只回复“收到”两个字。";

fn feed(mirror: &mut Mirror, change: &Value) -> Vec<EventKind> {
    match change["type"].as_str() {
        Some("snapshot") => {
            let first = mirror.revision.is_none();
            mirror.snapshot(
                change["conversationState"].clone(),
                change["revision"].as_u64().unwrap(),
                first,
            )
        }
        _ => match mirror.patch(
            change["baseRevision"].as_u64().unwrap(),
            change["revision"].as_u64().unwrap(),
            change["patches"].as_array().unwrap(),
        ) {
            Applied::Events(events) => events,
            Applied::Stale => panic!("stale patch {change}"),
        },
    }
}

fn replay(mirror: &mut Mirror, fixture: &str) -> Vec<EventKind> {
    fixture
        .lines()
        .flat_map(|line| {
            let message: Value = serde_json::from_str(line).unwrap();
            assert_eq!(message["params"]["conversationId"], THREAD);
            feed(mirror, &message["params"]["change"])
        })
        .collect()
}

fn working() -> EventKind {
    translate::state(SessionState::Working)
}

fn idle() -> EventKind {
    translate::state(SessionState::Idle)
}

#[test]
fn a_turn_in_the_app_reads_as_the_phone_expects() {
    let mut mirror = Mirror::new(THREAD);
    // The phone said it is working, and its own words are on screen already.
    mirror.sent(SENT);
    mirror.set_working(true);
    let events = replay(&mut mirror, TURN);
    let reasoning = "rs_0067737af50d29e4016af1dc55221887d0aaf1effc4b60c58b";
    let reply = "msg_0067737af50d29e4016af1dc55da7087d0a5c49ee84ffb0571";
    assert_eq!(
        events,
        vec![
            EventKind::Thinking {
                item: reasoning.into(),
                text: String::new()
            },
            EventKind::AssistantDelta {
                item: reply.into(),
                text: "收到".into()
            },
            EventKind::AssistantText {
                item: reply.into(),
                text: "收到".into()
            },
            EventKind::TurnDone { summary: None },
            idle(),
        ]
    );
    assert!(!mirror.busy());
    assert!(mirror.sent.is_empty());
}

#[test]
fn history_already_there_is_not_told_again() {
    let mut mirror = Mirror::new(THREAD);
    let first: Value = serde_json::from_str(TURN.lines().last().unwrap()).unwrap();
    let change = &first["params"]["change"];
    assert_eq!(change["type"], "snapshot");
    let events = mirror.snapshot(change["conversationState"].clone(), 23, true);
    assert_eq!(events, vec![]);
    assert!(!mirror.busy());
}

#[test]
fn words_typed_at_the_computer_are_shown_and_its_turns_followed() {
    let mut mirror = Mirror::new(THREAD);
    let events = replay(&mut mirror, TURN);
    assert_eq!(
        events[..2],
        [
            working(),
            EventKind::UserText {
                text: SENT.into(),
                images: None
            }
        ]
    );
    assert_eq!(events.last(), Some(&idle()));
}

fn base() -> Value {
    json!({"requests":[],"turnHistory":{"history":{"entitiesByKey":{}}}})
}

fn entities_path(rest: &[Value]) -> Value {
    let mut path = vec![
        json!("turnHistory"),
        json!("history"),
        json!("entitiesByKey"),
    ];
    path.extend_from_slice(rest);
    Value::Array(path)
}

fn events(mirror: &mut Mirror, base: u64, patches: Value) -> Vec<EventKind> {
    match mirror.patch(base, base + 1, patches.as_array().unwrap()) {
        Applied::Events(events) => events,
        Applied::Stale => panic!("stale"),
    }
}

#[test]
fn an_approval_is_asked_and_settled_where_it_was_answered() {
    let mut mirror = Mirror::new("t1");
    assert_eq!(mirror.snapshot(base(), 1, true), vec![]);
    let command = json!({"type":"commandExecution","id":"c1","command":"cargo test","cwd":"/w","status":"inProgress","startedAtMs":2,"completedAtMs":null});
    let got = events(
        &mut mirror,
        1,
        json!([{"op":"add","path":entities_path(&[json!("tail:1")]),"value":{"turnId":"u1","status":"inProgress","turnStartedAtMs":1,"items":[command]}}]),
    );
    assert_eq!(got[0], working());
    assert!(matches!(&got[1], EventKind::ToolStart { item, .. } if item == "c1"));
    let request = json!({"id":7,"method":COMMAND_APPROVAL,"params":{"threadId":"t1","turnId":"u1","itemId":"c1","reason":"tests"}});
    let got = events(
        &mut mirror,
        2,
        json!([{"op":"replace","path":["requests"],"value":[request.clone()]}]),
    );
    let EventKind::Approval { approval } = &got[0] else {
        panic!("{got:?}")
    };
    assert_eq!(approval.id, "7");
    assert_eq!(approval.kind, ApprovalKind::Command);
    assert_eq!(approval.title, "运行：cargo test");
    assert_eq!(got[1], translate::state(SessionState::NeedsYou));
    assert!(mirror.waiting());

    // Answered at the computer, declined: the item says so.
    let got = events(
        &mut mirror,
        3,
        json!([
            {"op":"replace","path":["requests"],"value":[]},
            {"op":"replace","path":entities_path(&[json!("tail:1"),json!("items"),json!(0),json!("status")]),"value":"declined"},
        ]),
    );
    assert_eq!(
        got[0],
        EventKind::ApprovalDone {
            approval: "7".into(),
            decision: Decision::Deny
        }
    );
    assert!(matches!(&got[1], EventKind::ToolEnd { item, ok: false, .. } if item == "c1"));
    assert_eq!(got[2], working());

    // Asked again, answered from the phone: not settled twice.
    let got = events(
        &mut mirror,
        4,
        json!([{"op":"replace","path":["requests"],"value":[request]}]),
    );
    assert!(matches!(&got[0], EventKind::Approval { .. }));
    let asked = mirror.answer("7").unwrap();
    assert_eq!(asked.id, json!(7));
    assert_eq!(asked.method, COMMAND_APPROVAL);
    let got = events(
        &mut mirror,
        5,
        json!([{"op":"replace","path":["requests"],"value":[]}]),
    );
    assert_eq!(got, vec![]);
}

#[test]
fn a_failed_turn_says_why() {
    let mut mirror = Mirror::new("t1");
    mirror.snapshot(base(), 1, true);
    mirror.set_working(true);
    events(
        &mut mirror,
        1,
        json!([{"op":"add","path":entities_path(&[json!("tail:1")]),"value":{"turnId":null,"status":"inProgress","items":[]}}]),
    );
    assert!(mirror.busy());
    let got = events(
        &mut mirror,
        2,
        json!([
            {"op":"replace","path":entities_path(&[json!("tail:1"),json!("status")]),"value":"failed"},
            {"op":"replace","path":entities_path(&[json!("tail:1"),json!("error")]),"value":{"message":"额度用完"}},
        ]),
    );
    assert_eq!(
        got,
        vec![
            translate::notice(NoticeLevel::Error, "额度用完"),
            EventKind::TurnDone { summary: None },
            idle()
        ]
    );
}

#[test]
fn a_turn_already_running_is_told_on_the_first_snapshot() {
    let mut mirror = Mirror::new("t1");
    let mut state = base();
    state["turnHistory"]["history"]["entitiesByKey"]["tail:1"] = json!({"turnId":"u1","status":"inProgress","items":[
        {"type":"agentMessage","id":"m1","text":"一半","completedAtMs":null}
    ]});
    assert_eq!(
        mirror.snapshot(state, 1, true),
        vec![
            working(),
            EventKind::AssistantDelta {
                item: "m1".into(),
                text: "一半".into()
            }
        ]
    );
    assert_eq!(mirror.active_turn().as_deref(), Some("u1"));
    let got = events(
        &mut mirror,
        1,
        json!([{"op":"replace","path":entities_path(&[json!("tail:1"),json!("items"),json!(0),json!("text")]),"value":"一半，另一半"}]),
    );
    assert_eq!(
        got,
        vec![EventKind::AssistantDelta {
            item: "m1".into(),
            text: "，另一半".into()
        }]
    );
}

#[test]
fn patches_out_of_step_ask_for_a_snapshot() {
    let mut mirror = Mirror::new("t1");
    assert!(matches!(mirror.patch(1, 2, &[]), Applied::Stale));
    mirror.snapshot(base(), 1, true);
    assert!(matches!(mirror.patch(5, 6, &[]), Applied::Stale));
    let bad = [json!({"op":"replace","path":["nowhere","deep"],"value":1})];
    assert!(matches!(mirror.patch(1, 2, &bad), Applied::Stale));
    // Then only a snapshot gets it back in step.
    assert!(matches!(mirror.patch(1, 2, &[]), Applied::Stale));
}

#[test]
fn immer_patches_apply() {
    let mut doc = json!({"a":{"list":[1,2,3]}});
    for patch in [
        json!({"op":"add","path":["a","list",1],"value":9}),
        json!({"op":"remove","path":["a","list",0]}),
        json!({"op":"replace","path":["a","list","0"],"value":8}),
        json!({"op":"add","path":["a","list","-"],"value":4}),
        json!({"op":"add","path":["a","new"],"value":{"x":1}}),
        json!({"op":"remove","path":["a","new","x"]}),
    ] {
        apply(&mut doc, &patch).unwrap();
    }
    assert_eq!(doc, json!({"a":{"list":[8,2,3,4],"new":{}}}));
    apply(
        &mut doc,
        &json!({"op":"replace","path":["a","list","length"],"value":2}),
    )
    .unwrap();
    assert_eq!(doc["a"]["list"], json!([8, 2]));
    apply(&mut doc, &json!({"op":"replace","path":[],"value":{"b":1}})).unwrap();
    assert_eq!(doc, json!({"b":1}));
    assert!(apply(&mut doc, &json!({"op":"remove","path":[]})).is_err());
    assert!(apply(&mut doc, &json!({"op":"add","path":["b","c"],"value":1})).is_err());
    assert!(apply(&mut doc, &json!({"op":"move","path":["b"]})).is_err());
}

//! DSH 0.2 (`fixtures/dsh/v2/`, DSH 0.2.2-rc.1 driven by a scripted model):
//! the call mapping, the mux router and a whole session over [`V2Api`].
//! No DSH and no model run here.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use tether_agent_api::{AgentCommand, AgentOutput, BoxFuture, SpawnOptions};
use tether_proto::{ApprovalKind, Decision, EventKind, NoticeLevel, SessionState};
use tokio::sync::{broadcast, mpsc};

use crate::{
    api::Api,
    hub::Frame,
    session,
    translate::Translator,
    v2::{self, Router, Rpc, V2Api},
};

const CONVERSATION: &str = include_str!("../../../fixtures/dsh/v2/conversation.jsonl");
const WAIT: Duration = Duration::from_secs(5);

fn lines() -> Vec<(String, Value)> {
    CONVERSATION
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("fixture line is JSON");
            (v["dir"].as_str().unwrap().to_owned(), v["v"].clone())
        })
        .collect()
}

/// The value DSH answered `endpoint` with, the `nth` time.
fn response(endpoint: &str, nth: usize) -> Value {
    let all = lines();
    let ids: Vec<String> = all
        .iter()
        .filter(|(dir, v)| dir == "req" && v["method"] == endpoint)
        .map(|(_, v)| v["rpcId"].as_str().unwrap().to_owned())
        .collect();
    let id = &ids[nth];
    all.iter()
        .find(|(dir, v)| dir == "res" && v["rpcId"] == id.as_str())
        .map(|(_, v)| v.clone())
        .expect("response recorded")
}

fn session_id() -> String {
    response("session/create", 0)["result"]["value"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The recording's stream ids by endpoint.
fn opened() -> HashMap<String, String> {
    lines()
        .into_iter()
        .filter(|(dir, _)| dir == "open")
        .map(|(_, v)| {
            (
                v["streamId"].as_str().unwrap().to_owned(),
                v["endpoint"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Both downlinks in recorded order, re-addressed to `events` / `follow`.
fn both(events: &str, follow: &str) -> Vec<Value> {
    let opened = opened();
    lines()
        .into_iter()
        .filter(|(dir, _)| dir == "mux")
        .filter_map(|(_, mut v)| {
            let to = match opened.get(v["streamId"].as_str().unwrap())?.as_str() {
                "$events" => events,
                "session/follow" => follow,
                _ => return None,
            };
            v["streamId"] = json!(to);
            Some(v)
        })
        .collect()
}

fn stream_of(open: &Value) -> String {
    open["streamId"].as_str().unwrap().to_owned()
}

// ------------------------------------------------------------------ mapping

#[test]
fn urls() {
    assert_eq!(
        v2::split_url("http://127.0.0.1:50328/?token=abc"),
        Some(("http://127.0.0.1:50328".into(), Some("abc".into())))
    );
    assert_eq!(
        v2::split_url(" http://127.0.0.1:50328?token=abc#x "),
        Some(("http://127.0.0.1:50328".into(), Some("abc".into())))
    );
    assert_eq!(
        v2::split_url("http://127.0.0.1:3080/"),
        Some(("http://127.0.0.1:3080".into(), None))
    );
    assert_eq!(v2::split_url("https://x"), None);
    assert_eq!(v2::split_url("http://"), None);
}

#[test]
fn calls_map_to_the_recorded_requests() {
    let sid = session_id();
    // Every request this adapter makes, against what DSH's own client sent.
    let recorded = |endpoint: &str| {
        lines()
            .into_iter()
            .find(|(dir, v)| dir == "req" && v["method"] == endpoint)
            .map(|(_, v)| v["payload"]["args"].clone())
            .unwrap()
    };
    let (ep, args) = v2::request("session.create", json!({ "cwd": r"C:\work\demo" }));
    assert_eq!(
        (ep.as_str(), &args),
        ("session/create", &recorded("session/create"))
    );
    let (ep, args) = v2::request(
        "session.prompt",
        json!({ "sessionId": sid, "mode": "queue", "content": [{"type": "text", "text": "hello there"}] }),
    );
    assert_eq!(ep, "session/prompt");
    assert!(args["request"]["requestId"].is_string());
    let mut want = recorded("session/prompt");
    want["request"]["requestId"] = args["request"]["requestId"].clone();
    assert_eq!(args, want);
    let (ep, args) = v2::request("session.cancel", json!({ "sessionId": sid }));
    assert_eq!(
        (ep.as_str(), &args),
        ("session/cancel", &recorded("session/cancel"))
    );
    let (ep, args) = v2::request(
        "session.selectModel",
        json!({ "sessionId": sid, "provider": "scripted", "model": "scripted-1" }),
    );
    assert_eq!(
        (ep.as_str(), &args),
        ("session/selectModel", &recorded("session/selectModel"))
    );
    let (ep, args) = v2::request(
        "commands/execute",
        json!({ "args": { "agentId": sid, "line": "/permission read-only", "images": [] } }),
    );
    assert_eq!(
        (ep.as_str(), &args),
        ("commands/execute", &recorded("commands/execute"))
    );
    assert_eq!(
        v2::request("session.list", json!({})),
        ("session/list".into(), recorded("session/list"))
    );
    let (ep, args) = v2::request(
        "session.history",
        json!({ "sessionId": sid, "maxMessages": 400 }),
    );
    assert_eq!(
        (ep.as_str(), &args),
        ("session/events", &recorded("session/events"))
    );
    assert_eq!(
        v2::request("llm.models", json!({})).0,
        "session/modelCatalog"
    );
    assert_eq!(
        v2::request("host.describe", json!({})).0,
        "session/modelCatalog"
    );
    assert_eq!(
        v2::request("credentials.describe", json!({ "refs": ["A"] })),
        ("credentials/describe".into(), json!({ "refs": ["A"] }))
    );
}

#[test]
fn results_take_the_old_shapes() {
    let list = v2::reshape(
        "session.list",
        response("session/list", 0)["result"]["value"].clone(),
    );
    let item = &list["items"][0];
    assert_eq!(item["sessionId"], session_id().as_str());
    assert_eq!(
        item["projections"]["values"]["title"],
        "Hello from the scripted model."
    );
    assert!(item.get("origin").is_none(), "not a subagent");
    let child = v2::reshape(
        "session.list",
        json!({ "items": [{ "sessionId": "c", "projections": { "values": { "subagent": { "parent": "p" } } } }] }),
    );
    assert_eq!(child["items"][0]["origin"], "subagent");

    let page = v2::reshape(
        "session.history",
        response("session/events", 0)["result"]["value"].clone(),
    );
    let types: Vec<&str> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["type"].as_str())
        .collect();
    assert!(types.contains(&"user/message") && types.contains(&"turn/end"));
    assert_eq!(page["hasMore"], false);

    let catalog =
        json!({ "default": { "provider": "scripted", "model": "scripted-1" }, "groups": [] });
    assert_eq!(
        v2::reshape("host.describe", catalog.clone())["model"],
        "scripted-1"
    );
    assert_eq!(v2::reshape("llm.models", catalog.clone()), catalog);
    let projections = json!({ "values": { "modelSelection": { "lastUsed": { "provider": "a", "model": "b" }, "next": null } } });
    assert_eq!(
        v2::reshape("session.models", projections)["current"]["model"],
        "b"
    );
    assert_eq!(
        v2::reshape(
            "credentials.describe",
            json!({ "K": { "configured": false } })
        ),
        json!({ "credentials": { "K": { "configured": false } } })
    );
    assert_eq!(
        v2::reshape("llm.providers", json!([{ "provider": "p" }])),
        json!({ "providers": [{ "provider": "p" }] })
    );
}

#[test]
fn answers_as_event_results() {
    let approval = crate::api::approval_answer("e1", "s", "e1", "rejected");
    assert_eq!(
        v2::outcome_of(&approval),
        json!({ "kind": "result", "value": "rejected" })
    );
    let question =
        crate::api::question_answer("e2", "s", json!([{ "id": "color", "selected": ["Blue"] }]));
    // What DSH's own client answered in the recording.
    let recorded = lines()
        .into_iter()
        .filter(|(dir, v)| dir == "req" && v["method"] == "$events/result")
        .map(|(_, v)| v["payload"]["args"]["outcome"].clone())
        .find(|o| o["value"].is_object())
        .unwrap();
    assert_eq!(v2::outcome_of(&question), recorded);
    let cancel = crate::api::question_cancel("e3");
    assert_eq!(v2::outcome_of(&cancel)["kind"], "rejected");
}

// ------------------------------------------------------------------- router

fn routed_events(answer: bool) -> (Vec<EventKind>, Vec<Value>) {
    let sid = session_id();
    let mut router = Router::default();
    let events = stream_of(&router.open_events());
    let (open, follow) = router.follow(&sid);
    assert!(open.is_some());
    let mut tr = Translator::live(Some(r"C:\work\demo".into()));
    let mut out = Vec::new();
    let mut opened = Vec::new();
    for message in both(&events, &follow) {
        let routed = router.route(&message);
        opened.extend(routed.opened);
        for frame in routed.frames {
            let Frame::Mux { rpc_id, payload } = frame else {
                continue;
            };
            assert_eq!(payload["sessionId"], sid.as_str());
            if answer && payload["type"] == "approval/requested" {
                // As the session loop does: answer, which resolves it here.
                out.extend(tr.frame(&payload));
                let outcome = if out
                    .iter()
                    .filter(|e| matches!(e, EventKind::Approval { .. }))
                    .count()
                    == 1
                {
                    "rejected"
                } else {
                    "allowed-once"
                };
                if let Some(Frame::Mux { payload, .. }) = router.answered(&rpc_id, Some(outcome)) {
                    out.extend(tr.frame(&payload));
                }
                continue;
            }
            out.extend(tr.frame(&payload));
        }
    }
    assert_eq!(opened.len(), 1);
    assert!(opened[0].1.is_ok());
    (out, opened.into_iter().map(|(s, _)| json!(s)).collect())
}

#[test]
fn a_recorded_conversation_through_the_router() {
    let (events, _) = routed_events(true);
    // Streamed pieces, then the whole reply under the same item.
    let deltas: String = events
        .iter()
        .filter_map(|e| match e {
            EventKind::AssistantDelta { item, text } if item == "a1.1" => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "Hello from the scripted model. ");
    assert!(events.contains(&EventKind::AssistantText {
        item: "a1.1".into(),
        text: "Hello from the scripted model. ".into()
    }));
    // The command and its output.
    let start = events
        .iter()
        .find_map(|e| match e {
            EventKind::ToolStart {
                item,
                tool,
                summary,
            } if tool == "pwsh" => Some((item.clone(), summary.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(start.1, "echo hello-from-tool");
    let ends: Vec<(bool, String)> = events
        .iter()
        .filter_map(|e| match e {
            EventKind::ToolEnd { ok, output, .. } => Some((*ok, output.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(ends[0], (true, "hello-from-tool".into()));
    // Refused escalation: a failed card with DSH's words.
    assert!(
        !ends[1].0 && ends[1].1.contains("rejected"),
        "{:?}",
        ends[1]
    );
    assert_eq!(ends[2], (true, "outside".into()));
    // Two approvals, each a command, answered no then yes.
    let approvals: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EventKind::Approval { approval } => Some(approval.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(approvals.len(), 2);
    assert_eq!(approvals[0].kind, ApprovalKind::Command);
    assert_eq!(approvals[0].title, "运行：echo outside");
    assert!(
        approvals[0].detail.contains("danger-full-access"),
        "{}",
        approvals[0].detail
    );
    let done: Vec<Decision> = events
        .iter()
        .filter_map(|e| match e {
            EventKind::ApprovalDone { decision, .. } => Some(*decision),
            _ => None,
        })
        .collect();
    assert_eq!(done, vec![Decision::Deny, Decision::AllowOnce]);
    // The question, as a message to answer.
    assert!(events.iter().any(|e| matches!(e,
        EventKind::AssistantText { item, text } if item == "q1" && text.contains("Which color?") && text.contains("2. Blue"))));
    // Six turns; the last one stopped.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, EventKind::TurnDone { .. }))
            .count(),
        6
    );
    assert!(events.contains(&EventKind::Notice {
        level: NoticeLevel::Info,
        text: "已停止".into()
    }));
}

#[test]
fn approvals_decided_elsewhere_say_how() {
    // Nobody here answers: DSH's own window did. `approval/decided` says how.
    let (events, _) = routed_events(false);
    let done: Vec<Decision> = events
        .iter()
        .filter_map(|e| match e {
            EventKind::ApprovalDone { decision, .. } => Some(*decision),
            _ => None,
        })
        .collect();
    assert_eq!(done, vec![Decision::Deny, Decision::AllowOnce]);
}

#[test]
fn follows_are_shared_and_errors_reported() {
    let mut router = Router::default();
    let (first, id) = router.follow("s");
    assert!(first.is_some());
    let (again, same) = router.follow("s");
    assert!(again.is_none());
    assert_eq!(id, same);
    assert!(router.unfollow("s").is_none(), "still used once");
    let cancel = router.unfollow("s").unwrap();
    assert_eq!(cancel, json!({ "type": "cancel", "streamId": id }));
    // A follow that fails to open reports DSH's error, in 0.1's code.
    let (_, id) = router.follow("gone");
    let routed = router.route(&json!({
        "type": "error", "streamId": id,
        "error": { "code": "session/not-found", "message": "session \"gone\" not found" },
    }));
    let (stream, result) = &routed.opened[0];
    assert_eq!(stream, &id);
    let error = result.clone().unwrap_err();
    assert_eq!(error.code, "session-not-found");
    // Losing `$events` loses the connection.
    let events = stream_of(&router.open_events());
    let routed = router.route(&json!({ "type": "end", "streamId": events }));
    assert!(matches!(routed.frames[..], [Frame::Down(_)]));
    // Host notices.
    let events = stream_of(&router.open_events());
    let routed = router.route(&json!({ "type": "item", "streamId": events,
        "value": { "type": "emit", "event": "api-session/removed", "args": ["s1"] } }));
    assert!(
        matches!(&routed.frames[..], [Frame::Host(v)] if v["type"] == "host/session-removed" && v["sessionId"] == "s1")
    );
    let routed = router.route(&json!({ "type": "item", "streamId": events,
        "value": { "type": "emit", "event": "api-session/error", "args": ["s1", "boom"] } }));
    assert!(matches!(&routed.frames[..], [Frame::Host(v)] if v["message"] == "boom"));
    // Waterfalls nobody here handles are passed on.
    router.route(&json!({ "type": "item", "streamId": events, "value": { "type": "ready", "clientId": "c1" } }));
    let routed = router.route(&json!({ "type": "item", "streamId": events,
        "value": { "type": "waterfall", "event": "other/thing", "eventId": "e9", "agentId": "s1", "request": {} } }));
    assert_eq!(
        routed.answers,
        vec![json!({ "clientId": "c1", "eventId": "e9", "outcome": { "kind": "next" } })]
    );
}

// ----------------------------------------------------------- whole session

#[derive(Default)]
struct FakeRpc {
    posted: Mutex<Vec<(String, Value)>>,
    replies: Mutex<HashMap<String, VecDeque<Value>>>,
}

impl FakeRpc {
    fn reply(&self, endpoint: &str, response: Value) {
        self.replies
            .lock()
            .unwrap()
            .entry(endpoint.into())
            .or_default()
            .push_back(response);
    }

    fn posted(&self, endpoint: &str) -> Vec<Value> {
        self.posted
            .lock()
            .unwrap()
            .iter()
            .filter(|(e, _)| e == endpoint)
            .map(|(_, v)| v["payload"]["args"].clone())
            .collect()
    }
}

impl Rpc for FakeRpc {
    fn post(&self, endpoint: &str, envelope: Value) -> BoxFuture<Value> {
        self.posted
            .lock()
            .unwrap()
            .push((endpoint.to_owned(), envelope));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .get_mut(endpoint)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| json!({ "type": "server-response", "result": { "ok": true } }));
        Box::pin(async move { Ok(reply) })
    }
}

async fn until<T>(
    rx: &mut mpsc::UnboundedReceiver<AgentOutput>,
    seen: &mut Vec<AgentOutput>,
    pick: impl Fn(&AgentOutput) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let o = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out; got {seen:#?}"))
            .unwrap_or_else(|| panic!("output closed; got {seen:#?}"));
        let got = pick(&o);
        seen.push(o);
        if let Some(got) = got {
            return got;
        }
    }
}

async fn next_up(up: &mut mpsc::UnboundedReceiver<String>) -> Value {
    let text = tokio::time::timeout(WAIT, up.recv())
        .await
        .expect("an uplink message")
        .expect("uplink open");
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn a_session_over_the_v2_client() {
    let sid = session_id();
    let rpc = Arc::new(FakeRpc::default());
    rpc.reply("session/create", response("session/create", 0));
    rpc.reply("session/prompt", response("session/prompt", 0));
    let (up_tx, mut up) = mpsc::unbounded_channel();
    let (frames, rx_frames) = broadcast::channel(4096);
    let api = V2Api::new(rpc.clone(), up_tx, frames);
    api.start();
    let events_open = next_up(&mut up).await;
    assert_eq!(events_open["endpoint"], "$events");
    let events = stream_of(&events_open);

    let (cmd, commands) = mpsc::unbounded_channel();
    let (out, mut rx) = mpsc::unbounded_channel();
    let mut seen = Vec::new();
    let shared: Arc<dyn Api> = Arc::new(api.clone());
    tokio::spawn(session::run(
        shared,
        rx_frames,
        SpawnOptions {
            cwd: r"C:\work\demo".into(),
            ..Default::default()
        },
        commands,
        out,
    ));
    // The session is created, then followed.
    let follow_open = next_up(&mut up).await;
    assert_eq!(follow_open["endpoint"], "session/follow");
    assert_eq!(
        follow_open["payload"]["args"]["request"]["address"]["sessionId"],
        sid.as_str()
    );
    assert_eq!(
        follow_open["payload"]["args"]["request"]["assistantStream"],
        true
    );
    let follow = stream_of(&follow_open);
    let recorded = both(&events, &follow);
    let feed = |from: usize, to: usize| {
        for m in &recorded[from..to] {
            api.receive(&m.to_string());
        }
    };
    // `ready` and the opening snapshot.
    let snapshot = recorded
        .iter()
        .position(|m| m["value"]["type"] == "snapshot")
        .unwrap();
    feed(0, snapshot + 1);
    let id = until(&mut rx, &mut seen, |o| match o {
        AgentOutput::NativeId(id) => Some(id.clone()),
        _ => None,
    })
    .await;
    assert_eq!(id, sid);
    assert_eq!(
        rpc.posted("session/create"),
        vec![json!({ "request": { "cwd": r"C:\work\demo" } })]
    );

    // First turn: a streamed reply.
    cmd.send(AgentCommand::Send {
        text: "hello there".into(),
        images: vec![],
    })
    .unwrap();
    let first_end = recorded
        .iter()
        .position(|m| m["value"]["event"]["type"] == "turn/end")
        .unwrap();
    until(&mut rx, &mut seen, |o| {
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
    feed(snapshot + 1, first_end + 1);
    let text = until(&mut rx, &mut seen, |o| match o {
        AgentOutput::Event(EventKind::AssistantText { text, .. }) => Some(text.clone()),
        _ => None,
    })
    .await;
    assert_eq!(text, "Hello from the scripted model. ");
    until(&mut rx, &mut seen, |o| {
        matches!(o, AgentOutput::Event(EventKind::TurnDone { .. })).then_some(())
    })
    .await;
    let prompt = &rpc.posted("session/prompt")[0]["request"];
    assert_eq!(prompt["content"][0]["text"], "hello there");
    assert_eq!(prompt["mode"], "queue");

    // Up to the first approval: answered from the phone with "deny".
    let ask = recorded
        .iter()
        .position(|m| m["value"]["event"] == "approval/request")
        .unwrap();
    feed(first_end + 1, ask + 1);
    let approval = until(&mut rx, &mut seen, |o| match o {
        AgentOutput::Event(EventKind::Approval { approval }) => Some(approval.id.clone()),
        _ => None,
    })
    .await;
    cmd.send(AgentCommand::Answer {
        approval: approval.clone(),
        decision: Decision::Deny,
    })
    .unwrap();
    let decision = until(&mut rx, &mut seen, |o| match o {
        AgentOutput::Event(EventKind::ApprovalDone { decision, .. }) => Some(*decision),
        _ => None,
    })
    .await;
    assert_eq!(decision, Decision::Deny);
    let result = &rpc.posted("$events/result")[0];
    assert_eq!(result["eventId"], recorded[ask]["value"]["eventId"]);
    assert_eq!(result["clientId"], recorded[0]["value"]["clientId"]);
    assert_eq!(
        result["outcome"],
        json!({ "kind": "result", "value": "rejected" })
    );
    // The rest of that turn: DSH's own record of the decision is not shown twice.
    let second_end = recorded
        .iter()
        .enumerate()
        .skip(ask)
        .find(|(_, m)| m["value"]["event"]["type"] == "turn/end")
        .unwrap()
        .0;
    feed(ask + 1, second_end + 1);
    until(&mut rx, &mut seen, |o| {
        matches!(o, AgentOutput::Event(EventKind::TurnDone { .. })).then_some(())
    })
    .await;
    let decided = seen
        .iter()
        .filter(|o| matches!(o, AgentOutput::Event(EventKind::ApprovalDone { .. })))
        .count();
    assert_eq!(decided, 1);

    // Ending the task stops following without touching the DSH session.
    cmd.send(AgentCommand::Shutdown).unwrap();
    until(&mut rx, &mut seen, |o| {
        matches!(o, AgentOutput::Exited { .. }).then_some(())
    })
    .await;
    let cancel = next_up(&mut up).await;
    assert_eq!(cancel, json!({ "type": "cancel", "streamId": follow }));
    assert!(rpc.posted("session/cancel").is_empty());
}

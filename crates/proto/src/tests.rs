use super::*;
use serde_json::{Value, json};

fn round_trip<T>(v: &T) -> Value
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let j = serde_json::to_value(v).unwrap();
    let back: T = serde_json::from_value(j.clone()).unwrap();
    assert_eq!(&back, v);
    j
}

fn session() -> SessionInfo {
    SessionInfo {
        id: "s1".into(),
        agent: "codex".into(),
        title: "修 bug".into(),
        cwd: "C:\\work".into(),
        state: SessionState::NeedsYou,
        status_line: Some("运行：npm test".into()),
        created_at: 1,
        updated_at: 2,
        last_seq: 9,
        model: Some("gpt-6.1-sol".into()),
        effort: Some("high".into()),
        chat: false,
        speed: Some("priority".into()),
        thread: Some("t1".into()),
        permission: Some(":workspace".into()),
    }
}

fn approval() -> Approval {
    Approval {
        id: "a1".into(),
        kind: ApprovalKind::Command,
        title: "运行：npm test".into(),
        detail: "npm test".into(),
        cwd: None,
        choices: vec![
            Decision::AllowOnce,
            Decision::AllowSession,
            Decision::Deny,
            Decision::Abort,
        ],
    }
}

#[test]
fn every_client_message_round_trips() {
    let msgs = vec![
        ClientMsg::Pair {
            version: 1,
            secret: "s".into(),
            device_name: "Pixel".into(),
        },
        ClientMsg::Hello {
            version: 1,
            token: "t".into(),
            device_name: "Pixel".into(),
        },
        ClientMsg::ListAgents { req: 1 },
        ClientMsg::ListSessions { req: 2 },
        ClientMsg::CreateSession {
            req: 3,
            agent: "codex".into(),
            cwd: Some("/w".into()),
            title: None,
            resume: None,
            model: Some("gpt-6.1-sol".into()),
            effort: Some("low".into()),
            chat: Some(true),
            project: Some("p1".into()),
            roots: Some(vec!["/a".into(), "/b".into()]),
            speed: Some("default".into()),
            permission: Some(":read-only".into()),
            reveal: Some(true),
            fork: None,
        },
        ClientMsg::ListHistory {
            req: 30,
            agent: "codex".into(),
            search: Some("微信".into()),
            cursor: None,
        },
        ClientMsg::ReadHistory {
            req: 31,
            agent: "codex".into(),
            thread: "t1".into(),
        },
        ClientMsg::ListModels {
            req: 32,
            agent: "codex".into(),
        },
        ClientMsg::SetModel {
            req: 33,
            session: "s1".into(),
            model: Some("gpt-6.1-sol".into()),
            effort: None,
            speed: Some("priority".into()),
            permission: None,
        },
        ClientMsg::Reveal {
            req: 34,
            agent: "codex".into(),
            thread: "t1".into(),
        },
        ClientMsg::Subscribe {
            req: 4,
            session: "s1".into(),
            after: 7,
        },
        ClientMsg::Unsubscribe {
            req: 5,
            session: "s1".into(),
        },
        ClientMsg::SendMessage {
            req: 6,
            session: "s1".into(),
            text: "你好".into(),
            images: Some(vec!["C:/up/a.png".into()]),
        },
        ClientMsg::PutFile {
            req: 35,
            name: "a.png".into(),
            data: "iVBORw0KGgo=".into(),
        },
        ClientMsg::GetFile {
            req: 36,
            path: "C:/up/a.png".into(),
        },
        ClientMsg::Answer {
            req: 7,
            session: "s1".into(),
            approval: "a1".into(),
            decision: Decision::AllowSession,
        },
        ClientMsg::Interrupt {
            req: 8,
            session: "s1".into(),
        },
        ClientMsg::CloseSession {
            req: 9,
            session: "s1".into(),
        },
        ClientMsg::TermOpen {
            req: 10,
            command: Some(vec!["pwsh".into()]),
            agent: None,
            cwd: None,
            cols: 80,
            rows: 24,
        },
        ClientMsg::TermAttach {
            req: 11,
            term: "t1".into(),
        },
        ClientMsg::TermDetach {
            req: 12,
            term: "t1".into(),
        },
        ClientMsg::TermResize {
            req: 13,
            term: "t1".into(),
            cols: 100,
            rows: 30,
        },
        ClientMsg::TermKill {
            req: 14,
            term: "t1".into(),
        },
        ClientMsg::ListTerms { req: 15 },
        ClientMsg::Ping { n: 16 },
    ];
    for m in &msgs {
        let j = round_trip(m);
        assert!(j["t"].is_string(), "{j}");
        assert_eq!(m.req(), j.get("req").and_then(Value::as_u64));
    }
}

#[test]
fn every_server_message_round_trips() {
    let msgs = vec![
        ServerMsg::Paired {
            device_id: "d".into(),
            token: "t".into(),
        },
        ServerMsg::Welcome {
            version: 1,
            host_name: "家里的电脑".into(),
            device_id: "d".into(),
            server_time: 5,
        },
        ServerMsg::Rejected {
            reason: RejectReason::Locked,
        },
        ServerMsg::ok(
            1,
            &SessionsData {
                sessions: vec![session()],
            },
        ),
        ServerMsg::ok(2, &Empty {}),
        ServerMsg::error(None, ErrorCode::AgentUnavailable, "codex 未安装"),
        ServerMsg::Event {
            session: "s1".into(),
            event: Event {
                seq: 1,
                at: 2,
                kind: EventKind::Approval {
                    approval: approval(),
                },
            },
        },
        ServerMsg::SessionUpdate { session: session() },
        ServerMsg::SessionClosed {
            session: "s1".into(),
        },
        ServerMsg::TermExit {
            term: "t1".into(),
            code: Some(0),
        },
        ServerMsg::Notify {
            kind: NotifyKind::NeedsYou,
            session: Some("s1".into()),
            term: None,
            title: "Codex 等你回复".into(),
            body: "运行：npm test".into(),
        },
        ServerMsg::Pong { n: 3 },
    ];
    for m in &msgs {
        round_trip(m);
    }
}

#[test]
fn wire_shapes_match_protocol_doc() {
    let j = serde_json::to_value(ClientMsg::SendMessage {
        req: 7,
        session: "s".into(),
        text: "x".into(),
        images: None,
    })
    .unwrap();
    assert_eq!(
        j,
        json!({"t":"send_message","req":7,"session":"s","text":"x"})
    );

    let j = serde_json::to_value(ServerMsg::ok(7, &Empty {})).unwrap();
    assert_eq!(j, json!({"t":"ok","req":7,"data":{}}));

    let j = serde_json::to_value(ServerMsg::error(Some(7), ErrorCode::NotFound, "no")).unwrap();
    assert_eq!(
        j,
        json!({"t":"error","req":7,"code":"not_found","message":"no"})
    );

    let j = serde_json::to_value(ServerMsg::Rejected {
        reason: RejectReason::InvalidToken,
    })
    .unwrap();
    assert_eq!(j, json!({"t":"rejected","reason":"invalid_token"}));
}

#[test]
fn event_json_is_flat() {
    let e = Event {
        seq: 3,
        at: 10,
        kind: EventKind::UserText {
            text: "hi".into(),
            images: None,
        },
    };
    let j = round_trip(&e);
    assert_eq!(j, json!({"seq":3,"at":10,"e":"user_text","text":"hi"}));

    let e = Event {
        seq: 4,
        at: 11,
        kind: EventKind::State {
            state: SessionState::NeedsYou,
            status_line: None,
        },
    };
    let j = round_trip(&e);
    assert_eq!(j["e"], "state");
    assert_eq!(j["state"], "needs_you");
}

#[test]
fn optional_fields_may_be_missing_or_null() {
    let a: ClientMsg =
        serde_json::from_value(json!({"t":"create_session","req":1,"agent":"codex"})).unwrap();
    let b: ClientMsg = serde_json::from_value(
        json!({"t":"create_session","req":1,"agent":"codex","cwd":null,"title":null}),
    )
    .unwrap();
    assert_eq!(a, b);

    let e: Event = serde_json::from_value(json!({"seq":1,"at":1,"e":"turn_done"})).unwrap();
    assert_eq!(e.kind, EventKind::TurnDone { summary: None });
}

#[test]
fn unknown_message_is_an_error() {
    assert!(serde_json::from_value::<ClientMsg>(json!({"t":"rm_rf","req":1})).is_err());
    assert!(serde_json::from_value::<ClientMsg>(json!({"req":1})).is_err());
}

#[test]
fn transient_events() {
    assert!(
        EventKind::AssistantDelta {
            item: "i".into(),
            text: "x".into()
        }
        .is_transient()
    );
    assert!(
        !EventKind::AssistantText {
            item: "i".into(),
            text: "x".into()
        }
        .is_transient()
    );
}

#[test]
fn binary_frames() {
    let f = binary::encode(binary::TERM_OUTPUT, "t1", b"hello");
    assert_eq!(f, [&[0x01, 2][..], b"t1", b"hello"].concat());
    assert_eq!(
        binary::decode(&f).unwrap(),
        (binary::TERM_OUTPUT, "t1", &b"hello"[..])
    );

    // empty data
    let f = binary::encode(binary::TERM_INPUT, "t", b"");
    assert_eq!(
        binary::decode(&f).unwrap(),
        (binary::TERM_INPUT, "t", &b""[..])
    );

    // 255-byte id
    let id = "x".repeat(255);
    let f = binary::encode(binary::TERM_OUTPUT, &id, b"\x1b[0m");
    let (_, got, data) = binary::decode(&f).unwrap();
    assert_eq!((got, data), (id.as_str(), &b"\x1b[0m"[..]));

    // malformed
    assert!(binary::decode(&[]).is_err());
    assert!(binary::decode(&[0x01]).is_err());
    assert!(binary::decode(&[0x01, 0]).is_err());
    assert!(binary::decode(&[0x01, 5, b'a']).is_err());
    assert!(binary::decode(&[0x07, 1, b'a']).is_err());
    assert!(binary::decode(&[0x01, 1, 0xff]).is_err());
}

#[test]
#[should_panic]
fn binary_encode_rejects_long_ids() {
    binary::encode(binary::TERM_OUTPUT, &"x".repeat(256), b"");
}

#[test]
fn truncate_keeps_the_end() {
    assert_eq!(truncate_tail("abc", 10), "abc");
    assert_eq!(truncate_tail("abcdef", 3), "…def");
    // never splits a multi-byte character
    let t = truncate_tail("一二三四", 7);
    assert!(t.ends_with("三四"), "{t}");
}

/// Writes `index.ts` re-exporting every generated type, so the client can
/// `import { … } from "./generated"`. Runs alongside ts-rs' own
/// `export_bindings_*` tests.
#[test]
fn export_bindings_index() {
    const TYPES: &[&str] = &[
        "ClientMsg",
        "ServerMsg",
        "RejectReason",
        "ErrorCode",
        "NotifyKind",
        "Empty",
        "AgentsData",
        "SessionsData",
        "SessionData",
        "TermData",
        "TermsData",
        "AgentKind",
        "AgentInfo",
        "SessionState",
        "SessionInfo",
        "TermInfo",
        "Event",
        "EventKind",
        "NoticeLevel",
        "Approval",
        "ApprovalKind",
        "Decision",
        "HistoryData",
        "HistoryReadData",
        "HistoryThread",
        "ModelsData",
        "ModelInfo",
        "SpeedInfo",
        "PermissionInfo",
        "ProjectInfo",
        "FileData",
        "FileContent",
    ];
    let Ok(dir) = std::env::var("TS_RS_EXPORT_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    let mut out = String::from(
        "// Generated by `cargo test -p tether-proto export_bindings`. Do not edit.\n",
    );
    for t in TYPES {
        out.push_str(&format!("export type {{ {t} }} from \"./{t}\";\n"));
    }
    out.push_str(&format!(
        "\nexport const PROTOCOL_VERSION = {PROTOCOL_VERSION};\n"
    ));
    std::fs::write(dir.join("index.ts"), out).unwrap();
}

#[test]
fn projects_and_speeds_are_optional_on_the_wire() {
    let page: HistoryData = serde_json::from_value(json!({
        "threads": [{"id":"t","title":"x","preview":"","cwd":"/w","origin":null,"created_at":1,"updated_at":2}],
        "next_cursor": null
    }))
    .unwrap();
    assert!(page.projects.is_empty());
    assert_eq!(page.threads[0].project, None);
    let model: ModelInfo = serde_json::from_value(json!({
        "id":"m","name":"M","description":null,"efforts":[],"default_effort":null,"is_default":true
    }))
    .unwrap();
    assert!(model.speeds.is_empty());
    let create: ClientMsg =
        serde_json::from_value(json!({"t":"create_session","req":1,"agent":"codex"})).unwrap();
    assert!(matches!(
        create,
        ClientMsg::CreateSession {
            project: None,
            speed: None,
            permission: None,
            reveal: None,
            ..
        }
    ));
    // An older phone or computer: no permissions on either side.
    let set: ClientMsg = serde_json::from_value(
        json!({"t":"set_model","req":1,"session":"s","model":null,"effort":null,"speed":null}),
    )
    .unwrap();
    assert!(matches!(
        set,
        ClientMsg::SetModel {
            permission: None,
            ..
        }
    ));
    let data: ModelsData = serde_json::from_value(json!({"models":[]})).unwrap();
    assert!(data.permissions.is_empty());
    assert_eq!(data.default_permission, None);
    let mut info = serde_json::to_value(session()).unwrap();
    info.as_object_mut().unwrap().remove("permission");
    let info: SessionInfo = serde_json::from_value(info).unwrap();
    assert_eq!(info.permission, None);
    round_trip(&ModelsData {
        models: Vec::new(),
        permissions: vec![PermissionInfo {
            id: ":workspace".into(),
            name: "工作区可写".into(),
            description: None,
        }],
        default_permission: Some(":workspace".into()),
    });
    let project = ProjectInfo {
        id: "p".into(),
        name: "社团".into(),
        roots: vec![r"C:\爱极客AIGREEK".into(), r"C:\社团".into()],
    };
    assert_eq!(round_trip(&project)["roots"][1], r"C:\社团");
}

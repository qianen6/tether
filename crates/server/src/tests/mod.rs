//! All gateway scenarios use real loopback WebSockets, EchoAdapter, and private
//! memory backends. Only the explicitly ignored integration tests use T02/T07.
mod edges;
mod fakes;
mod picks;
mod ws;

use crate::{Server, ServerConfig, backends::StoreBackend, lock};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fakes::{MemoryStore, MemoryTerms};
use serde_json::json;
use std::{
    sync::{Arc, Barrier, atomic::Ordering},
    time::Duration,
};
use tether_agent_api::EchoAdapter;
use tether_proto::*;
use tether_store::{SessionRecord, Store};
use tether_terminals::{TermChunk, TermManager};
use tokio_tungstenite::tungstenite::Message;
use ws::{Phone, Wire};

fn config() -> ServerConfig {
    ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        host_name: "test-host".into(),
    }
}
async fn start(store: Arc<MemoryStore>, terms: Arc<MemoryTerms>) -> Server {
    Server::start_with(config(), store, terms, vec![Arc::new(EchoAdapter)])
        .await
        .unwrap()
}
async fn setup() -> (Server, Arc<MemoryStore>, Arc<MemoryTerms>, Phone) {
    let store = Arc::new(MemoryStore::default());
    let terms = Arc::new(MemoryTerms::default());
    let server = start(store.clone(), terms.clone()).await;
    let phone = Phone::pair(&server).await;
    (server, store, terms, phone)
}
async fn rejected(server: &Server, value: serde_json::Value, reason: RejectReason, code: u16) {
    let mut phone = Phone::connect(server).await;
    phone.send(value).await;
    assert!(
        matches!(phone.read().await, Wire::Json(ServerMsg::Rejected { reason: actual }) if actual == reason)
    );
    phone.close_code(code).await;
}
async fn notify(phone: &mut Phone, kind: NotifyKind, session: Option<&str>, term: Option<&str>) {
    phone.wait(|w| matches!(w, Wire::Json(ServerMsg::Notify { kind: k, session: s, term: t, .. }) if *k == kind && s.as_deref() == session && t.as_deref() == term)).await;
}
fn seeded(id: &str, state: SessionState, seq: u64) -> SessionRecord {
    SessionRecord {
        info: SessionInfo {
            id: id.into(),
            agent: "echo".into(),
            title: "restored".into(),
            cwd: std::env::current_dir().unwrap().to_string_lossy().into(),
            state,
            status_line: Some("old tool".into()),
            created_at: now_ms(),
            updated_at: now_ms(),
            last_seq: seq,
            model: None,
            effort: None,
            chat: false,
            speed: None,
            thread: Some("echo-resumable".into()),
            permission: None,
        },
        native_id: Some("echo-resumable".into()),
    }
}

#[tokio::test]
async fn pair_is_one_time_broadcasts_and_enters_authenticated_state() {
    let store = Arc::new(MemoryStore::default());
    let server = start(store.clone(), Arc::new(MemoryTerms::default())).await;
    let mut paired = server.on_paired();
    let secret = server.new_pair_secret(Duration::from_secs(600));
    assert_eq!(URL_SAFE_NO_PAD.decode(&secret).unwrap().len(), 32);
    let mut phone = Phone::pair_secret(&server, secret.clone()).await;
    assert_eq!(URL_SAFE_NO_PAD.decode(&phone.token).unwrap().len(), 32);
    let device = paired.recv().await.unwrap();
    assert_eq!(device.id, phone.device);
    assert_eq!(device.name, "Pixel 测试");
    assert_eq!(server.connected_devices(), vec![device.id.clone()]);
    assert!(
        store
            .device_by_token(&phone.token)
            .unwrap()
            .unwrap()
            .last_seen
            .is_some()
    );
    phone.call("list_agents", json!({})).await;
    rejected(
        &server,
        json!({"t":"pair","version":1,"secret":secret,"device_name":"again"}),
        RejectReason::InvalidSecret,
        4401,
    )
    .await;
    let other = Phone::hello(&server, &phone.token).await;
    assert_eq!(other.device, phone.device);
    assert_eq!(server.connected_devices().len(), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn pairing_expiry_and_fifteen_minute_cap() {
    let server = start(
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryTerms::default()),
    )
    .await;
    let expired = server.new_pair_secret(Duration::ZERO);
    rejected(
        &server,
        json!({"t":"pair","version":1,"secret":expired,"device_name":"x"}),
        RejectReason::InvalidSecret,
        4401,
    )
    .await;
    let capped = server.new_pair_secret(Duration::from_secs(3600));
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(901)).await;
    tokio::time::resume();
    rejected(
        &server,
        json!({"t":"pair","version":1,"secret":capped,"device_name":"x"}),
        RejectReason::InvalidSecret,
        4401,
    )
    .await;
    server.shutdown().await;
}

#[tokio::test]
async fn pairing_locks_after_five_failures_then_unlocks_and_success_resets_counter() {
    let server = start(
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryTerms::default()),
    )
    .await;
    for _ in 0..5 {
        rejected(
            &server,
            json!({"t":"pair","version":1,"secret":"wrong","device_name":"x"}),
            RejectReason::InvalidSecret,
            4401,
        )
        .await;
    }
    let secret = server.new_pair_secret(Duration::from_secs(600));
    rejected(
        &server,
        json!({"t":"pair","version":1,"secret":secret,"device_name":"x"}),
        RejectReason::Locked,
        4401,
    )
    .await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    let _phone = Phone::pair_secret(&server, secret).await;
    for _ in 0..4 {
        rejected(
            &server,
            json!({"t":"pair","version":1,"secret":"wrong","device_name":"x"}),
            RejectReason::InvalidSecret,
            4401,
        )
        .await;
    }
    let _phone = Phone::pair(&server).await;
    for _ in 0..4 {
        rejected(
            &server,
            json!({"t":"pair","version":1,"secret":"wrong","device_name":"x"}),
            RejectReason::InvalidSecret,
            4401,
        )
        .await;
    }
    let _phone = Phone::pair(&server).await;
    server.shutdown().await;
}

#[tokio::test]
async fn simultaneous_claims_issue_only_one_device() {
    let server = start(
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryTerms::default()),
    )
    .await;
    let secret = server.new_pair_secret(Duration::from_secs(600));
    let mut a = Phone::connect(&server).await;
    let mut b = Phone::connect(&server).await;
    let request = json!({"t":"pair","version":1,"secret":secret,"device_name":"x"});
    tokio::join!(a.send(request.clone()), b.send(request));
    let (a, b) = tokio::join!(a.read(), b.read());
    assert_eq!(
        [a, b]
            .iter()
            .filter(|w| matches!(w, Wire::Json(ServerMsg::Paired { .. })))
            .count(),
        1
    );
    server.shutdown().await;
}

#[tokio::test]
async fn invalid_tokens_and_versions_get_exact_close_codes() {
    let (server, _, _, _) = setup().await;
    rejected(
        &server,
        json!({"t":"hello","version":1,"token":"wrong","device_name":"x"}),
        RejectReason::InvalidToken,
        4401,
    )
    .await;
    rejected(
        &server,
        json!({"t":"hello","version":2,"token":"wrong","device_name":"x"}),
        RejectReason::VersionMismatch,
        4426,
    )
    .await;
    let secret = server.new_pair_secret(Duration::from_secs(600));
    rejected(
        &server,
        json!({"t":"pair","version":0,"secret":secret,"device_name":"x"}),
        RejectReason::VersionMismatch,
        4426,
    )
    .await;
    let _phone = Phone::pair_secret(&server, secret).await;
    server.shutdown().await;
}

#[tokio::test]
async fn revoke_closes_all_connections_and_rejects_old_token_without_affecting_other_devices() {
    let (server, _, _, mut a) = setup().await;
    let mut b = Phone::hello(&server, &a.token).await;
    let mut other = Phone::pair(&server).await;
    let deleted = server.revoke(&a.device).await.unwrap().unwrap();
    assert_eq!(deleted.phone_key.as_deref(), Some("fixture-phone-key"));
    a.close_code(4401).await;
    b.close_code(4401).await;
    assert_eq!(server.connected_devices(), vec![other.device.clone()]);
    other.send(json!({"t":"ping","n":42})).await;
    other
        .wait(|w| matches!(w, Wire::Json(ServerMsg::Pong { n: 42 })))
        .await;
    rejected(
        &server,
        json!({"t":"hello","version":1,"token":a.token,"device_name":"x"}),
        RejectReason::Revoked,
        4401,
    )
    .await;
    assert!(server.revoke(&a.device).await.unwrap().is_none());
    server.shutdown().await;
}

#[tokio::test]
async fn unauthenticated_requests_and_malformed_first_frames_close_4400() {
    let (server, _, _, _) = setup().await;
    let mut phone = Phone::connect(&server).await;
    phone.send(json!({"t":"list_agents","req":7})).await;
    phone.error(Some(7), ErrorCode::Unauthorized).await;
    phone.close_code(4400).await;
    for frame in [
        Message::Text("{bad".into()),
        Message::Binary(vec![2, 0].into()),
    ] {
        let mut phone = Phone::connect(&server).await;
        phone.raw(frame).await;
        phone.error(None, ErrorCode::BadRequest).await;
        phone.close_code(4400).await;
    }
    server.shutdown().await;
}

#[tokio::test]
async fn handshake_and_idle_timeouts_and_any_frame_keepalive() {
    let (server, _, _, mut phone) = setup().await;
    let mut silent = Phone::connect(&server).await;
    tokio::task::yield_now().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::time::resume();
    silent.close_code(4400).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::time::resume();
    phone.raw(Message::Ping(vec![1, 2, 3].into())).await;
    // JSON ping provides an observable barrier after the control frame.
    phone.send(json!({"t":"ping","n":123})).await;
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::Pong { n: 123 })))
        .await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(40)).await;
    tokio::time::resume();
    phone.send(json!({"t":"ping","n":124})).await;
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::Pong { n: 124 })))
        .await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    phone.close_code(1000).await;
    server.shutdown().await;
}

#[tokio::test]
async fn malformed_oversized_and_wrong_direction_frames_are_discarded() {
    let (server, _, _, mut phone) = setup().await;
    for text in [
        "{",
        "[]",
        "{\"t\":\"unknown\",\"req\":99}",
        "{\"t\":\"list_agents\",\"req\":-1}",
    ] {
        phone.raw(Message::Text(text.into())).await;
        phone
            .wait(|w| {
                matches!(
                    w,
                    Wire::Json(ServerMsg::Error {
                        code: ErrorCode::BadRequest,
                        ..
                    })
                )
            })
            .await;
    }
    for bytes in [
        vec![],
        vec![2, 0],
        vec![2, 2, 0xff, 0xff],
        binary::encode(binary::TERM_OUTPUT, "x", b"bad"),
    ] {
        phone.raw(Message::Binary(bytes.into())).await;
        phone.error(None, ErrorCode::BadRequest).await;
    }
    phone
        .raw(Message::Text("x".repeat(MAX_FRAME + 1).into()))
        .await;
    phone.error(None, ErrorCode::BadRequest).await;
    phone
        .raw(Message::Binary(vec![0; MAX_FRAME + 1].into()))
        .await;
    phone.error(None, ErrorCode::BadRequest).await;
    phone.send(json!({"t":"ping","n":u64::MAX})).await;
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::Pong { n }) if *n == u64::MAX))
        .await;
    phone.call("list_agents", json!({})).await;
    phone.send(json!({"t":"list_sessions","req":1})).await;
    phone.error(Some(1), ErrorCode::BadRequest).await;
    phone
        .send(json!({"t":"hello","version":1,"token":"x","device_name":"x"}))
        .await;
    phone.error(None, ErrorCode::BadRequest).await;
    server.shutdown().await;
}

#[tokio::test]
async fn echo_session_streams_all_events_persists_only_durable_and_broadcasts_updates() {
    let (server, store, _, mut phone) = setup().await;
    let mut observer = Phone::pair(&server).await;
    let agents = phone.call("list_agents", json!({})).await;
    assert_eq!(agents["agents"][0]["id"], "echo");
    assert_eq!(
        phone.call("list_sessions", json!({})).await["sessions"],
        json!([])
    );
    let session = phone.create().await;
    assert_eq!(session.last_seq, 0);
    observer.wait(|w| matches!(w, Wire::Json(ServerMsg::SessionUpdate { session: s }) if s.id == session.id)).await;
    phone.subscribe(&session.id, 0).await;
    phone.send_text(&session.id, "你好 🌏").await;
    let mut events = Vec::new();
    loop {
        let event = phone.event(&session.id, |_| true).await;
        let ended = matches!(
            event.kind,
            EventKind::State {
                state: SessionState::Idle,
                ..
            }
        );
        events.push(event);
        if ended {
            break;
        }
    }
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
    assert_eq!(events[0].seq, 1);
    assert!(matches!(&events[0].kind, EventKind::UserText { text, .. } if text == "你好 🌏"));
    assert!(events.iter().any(|e| matches!(
        e.kind,
        EventKind::State {
            state: SessionState::Working,
            ..
        }
    )));
    let pieces: String = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::AssistantDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(pieces, "echo: 你好 🌏");
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::AssistantText { text, .. } if text == &pieces))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, EventKind::TurnDone { .. }))
    );
    assert_eq!(
        store.history(&session.id),
        events
            .iter()
            .filter(|e| !e.kind.is_transient())
            .cloned()
            .collect::<Vec<_>>()
    );
    let last_seq = events.last().unwrap().seq;
    observer.wait(|w| matches!(w, Wire::Json(ServerMsg::SessionUpdate { session: s }) if s.id == session.id && s.last_seq == last_seq)).await;
    assert!(!observer.inbox.iter().any(|w| matches!(
        w,
        Wire::Json(ServerMsg::Notify {
            kind: NotifyKind::Done,
            ..
        })
    )));
    assert!(
        !observer
            .inbox
            .iter()
            .any(|w| matches!(w, Wire::Json(ServerMsg::Event { .. })))
    );
    assert_eq!(
        phone.call("list_sessions", json!({})).await["sessions"][0]["last_seq"],
        last_seq
    );
    server.shutdown().await;
}

#[tokio::test]
async fn approvals_all_decisions_busy_stale_answers_and_interrupt() {
    let (server, _, _, mut phone) = setup().await;
    for decision in [
        Decision::AllowOnce,
        Decision::AllowSession,
        Decision::Deny,
        Decision::Abort,
    ] {
        let session = phone.create().await;
        phone.subscribe(&session.id, 0).await;
        phone.send_text(&session.id, "approve:xx").await;
        let event = phone
            .event(&session.id, |e| matches!(e, EventKind::Approval { .. }))
            .await;
        let approval = match event.kind {
            EventKind::Approval { approval } => approval,
            _ => unreachable!(),
        };
        notify(&mut phone, NotifyKind::NeedsYou, Some(&session.id), None).await;
        let req = phone
            .request("send_message", json!({"session":session.id,"text":"busy"}))
            .await;
        phone.error(Some(req), ErrorCode::Busy).await;
        let req = phone
            .request(
                "answer",
                json!({"session":session.id,"approval":"stale","decision":"deny"}),
            )
            .await;
        phone.error(Some(req), ErrorCode::NotFound).await;
        phone
            .call(
                "answer",
                json!({"session":session.id,"approval":approval.id,"decision":decision}),
            )
            .await;
        phone
            .event(
                &session.id,
                |e| matches!(e, EventKind::ApprovalDone { decision: d, .. } if *d == decision),
            )
            .await;
        phone.idle(&session.id).await;
        let req = phone
            .request(
                "answer",
                json!({"session":session.id,"approval":approval.id,"decision":"deny"}),
            )
            .await;
        phone.error(Some(req), ErrorCode::NotFound).await;
    }
    let session = phone.create().await;
    phone.subscribe(&session.id, 0).await;
    phone.send_text(&session.id, "approve:interrupt").await;
    phone
        .event(&session.id, |e| matches!(e, EventKind::Approval { .. }))
        .await;
    phone.call("interrupt", json!({"session":session.id})).await;
    phone
        .event(
            &session.id,
            |e| matches!(e, EventKind::Notice { text, .. } if text == "已打断"),
        )
        .await;
    phone.idle(&session.id).await;
    server.shutdown().await;
}

#[tokio::test]
async fn process_exit_crash_and_respawn_reuse_native_id() {
    let (server, store, _, mut phone) = setup().await;
    let session = phone.create().await;
    phone.subscribe(&session.id, 0).await;
    phone.send_text(&session.id, "first").await;
    phone.idle(&session.id).await;
    let native = store.records()[0].native_id.clone().unwrap();
    for (text, state) in [
        ("/exit", SessionState::Exited),
        ("/crash", SessionState::Error),
    ] {
        phone.send_text(&session.id, text).await;
        phone
            .event(
                &session.id,
                |e| matches!(e, EventKind::State { state: s, .. } if *s == state),
            )
            .await;
        if state == SessionState::Error {
            notify(&mut phone, NotifyKind::Failed, Some(&session.id), None).await;
        }
        phone.send_text(&session.id, "resumed").await;
        phone.idle(&session.id).await;
        assert_eq!(
            store.records()[0].native_id.as_deref(),
            Some(native.as_str())
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn unsubscribe_and_disconnect_update_cross_connection_notification_counts() {
    let (server, _, _, mut a) = setup().await;
    let mut b = Phone::hello(&server, &a.token).await;
    let session = a.create().await;
    a.subscribe(&session.id, 0).await;
    b.subscribe(&session.id, 0).await;
    a.call("unsubscribe", json!({"session":session.id})).await;
    a.send_text(&session.id, "only b watches").await;
    let end = b.idle(&session.id).await;
    a.wait(|w| matches!(w, Wire::Json(ServerMsg::SessionUpdate { session: s }) if s.id == session.id && s.last_seq == end.seq)).await;
    assert!(!a.inbox.iter().any(|w| matches!(
        w,
        Wire::Json(
            ServerMsg::Event { .. }
                | ServerMsg::Notify {
                    kind: NotifyKind::Done,
                    ..
                }
        )
    )));
    b.disconnect().await;
    // A new authenticated request after closing b is not itself a disconnect
    // barrier; wait until the server has processed its close frame.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    a.send_text(&session.id, "nobody watches").await;
    notify(&mut a, NotifyKind::Done, Some(&session.id), None).await;
    server.shutdown().await;
}

#[tokio::test]
async fn reconnect_after_partial_stream_replays_exactly_the_missing_durable_events() {
    let (server, store, _, mut phone) = setup().await;
    let session = phone.create().await;
    phone.subscribe(&session.id, 0).await;
    phone.send_text(&session.id, "partial stream").await;
    let mut prefix = Vec::new();
    let after = loop {
        let event = phone.event(&session.id, |_| true).await;
        let delta = event.kind.is_transient();
        let seq = event.seq;
        prefix.push(event);
        if delta {
            break seq;
        }
    };
    let token = phone.token.clone();
    phone.disconnect().await;
    let mut phone = Phone::hello(&server, &token).await;
    phone.subscribe(&session.id, after).await;
    let mut replayed = Vec::new();
    loop {
        let event = phone.event(&session.id, |_| true).await;
        assert!(event.seq > after);
        let end = matches!(
            event.kind,
            EventKind::State {
                state: SessionState::Idle,
                ..
            }
        );
        replayed.push(event);
        if end {
            break;
        }
    }
    assert!(replayed.windows(2).all(|w| w[0].seq < w[1].seq));
    let durable: Vec<_> = prefix
        .into_iter()
        .chain(replayed)
        .filter(|e| !e.kind.is_transient())
        .collect();
    assert_eq!(durable, store.history(&session.id));
    server.shutdown().await;
}

#[tokio::test]
async fn history_is_paginated_and_after_is_exclusive() {
    let store = Arc::new(MemoryStore::default());
    let history: Vec<_> = (1..=350)
        .map(|seq| Event {
            seq,
            at: now_ms(),
            kind: EventKind::UserText {
                text: seq.to_string(),
                images: None,
            },
        })
        .collect();
    store.seed(seeded("history", SessionState::Idle, 350), history.clone());
    let server = start(store.clone(), Arc::new(MemoryTerms::default())).await;
    let mut phone = Phone::pair(&server).await;
    let before = store.reads.load(Ordering::SeqCst);
    phone.subscribe("history", 5).await;
    for expected in &history[5..] {
        assert_eq!(&phone.event("history", |_| true).await, expected);
    }
    assert!(store.reads.load(Ordering::SeqCst) >= before + 3);
    phone.send_text("history", "live").await;
    let event = phone.event("history", |_| true).await;
    assert_eq!(event.seq, 351);
    phone.idle("history").await;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribe_before_history_read_closes_the_snapshot_live_race() {
    let (server, store, _, mut subscriber) = setup().await;
    let mut sender = Phone::pair(&server).await;
    let session = subscriber.create().await;
    let barrier = Arc::new(Barrier::new(2));
    let release = barrier.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    *lock(&store.history_hook) = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release.wait();
    }));
    subscriber.subscribe(&session.id, 0).await;
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    sender.send_text(&session.id, "during snapshot").await;
    sender.wait(|w| matches!(w, Wire::Json(ServerMsg::SessionUpdate { session: s }) if s.id == session.id && s.last_seq > 0 && s.state == SessionState::Idle)).await;
    barrier.wait();
    let mut seen = Vec::new();
    loop {
        let event = subscriber.event(&session.id, |_| true).await;
        let end = matches!(
            event.kind,
            EventKind::State {
                state: SessionState::Idle,
                ..
            }
        );
        seen.push(event);
        if end {
            break;
        }
    }
    assert!(seen.windows(2).all(|w| w[0].seq < w[1].seq));
    assert_eq!(
        seen.into_iter()
            .filter(|e| !e.kind.is_transient())
            .collect::<Vec<_>>(),
        store.history(&session.id)
    );
    server.shutdown().await;
}

#[tokio::test]
async fn lagged_subscription_recovers_more_than_broadcast_capacity_without_duplicates() {
    let (server, store, _, mut phone) = setup().await;
    let session = phone.create().await;
    phone.subscribe(&session.id, 0).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let before = store.reads.load(Ordering::SeqCst);
    let events = (0..400)
        .map(|n| EventKind::UserText {
            text: format!("burst {n}"),
            images: None,
        })
        .collect();
    // One actor turn deliberately overflows both the 64-event session channel
    // and the global channel while a real WebSocket subscription is active.
    server
        .state
        .sessions
        .get(&session.id)
        .unwrap()
        .inject(events)
        .await;
    for seq in 1..=400 {
        assert_eq!(phone.event(&session.id, |_| true).await.seq, seq);
    }
    assert!(store.reads.load(Ordering::SeqCst) > before);
    phone.send_text(&session.id, "after lag").await;
    assert_eq!(phone.event(&session.id, |_| true).await.seq, 401);
    phone.idle(&session.id).await;
    server.shutdown().await;
}

#[tokio::test]
async fn restart_preserves_history_resets_state_adds_notice_and_lazily_resumes() {
    let (server, store, _, mut phone) = setup().await;
    let session = phone.create().await;
    phone.subscribe(&session.id, 0).await;
    phone.send_text(&session.id, "before restart").await;
    phone.idle(&session.id).await;
    let native = store.records()[0].native_id.clone();
    server
        .state
        .sessions
        .get(&session.id)
        .unwrap()
        .inject(vec![EventKind::State {
            state: SessionState::Working,
            status_line: None,
        }])
        .await;
    let seq = store.records()[0].info.last_seq;
    let token = phone.token.clone();
    server.shutdown().await;
    let server = start(store.clone(), Arc::new(MemoryTerms::default())).await;
    let mut phone = Phone::hello(&server, &token).await;
    let list = phone.call("list_sessions", json!({})).await;
    assert_eq!(list["sessions"][0]["state"], "idle");
    assert_eq!(list["sessions"][0]["last_seq"], seq + 1);
    assert_eq!(store.records()[0].native_id, native);
    phone.subscribe(&session.id, seq).await;
    phone
        .event(
            &session.id,
            |e| matches!(e, EventKind::Notice { text, .. } if text == "电脑重启，任务已中断"),
        )
        .await;
    phone.send_text(&session.id, "after restart").await;
    phone.idle(&session.id).await;
    assert_eq!(store.records()[0].native_id, native);
    server.shutdown().await;
}

#[tokio::test]
async fn restart_recovers_tail_after_append_before_metadata_write() {
    let store = Arc::new(MemoryStore::default());
    store.seed(
        seeded("torn", SessionState::Working, 0),
        vec![Event {
            seq: 9,
            at: now_ms(),
            kind: EventKind::UserText {
                text: "durable tail".into(),
                images: None,
            },
        }],
    );
    let server = start(store.clone(), Arc::new(MemoryTerms::default())).await;
    let mut phone = Phone::pair(&server).await;
    phone.subscribe("torn", 0).await;
    assert_eq!(phone.event("torn", |_| true).await.seq, 9);
    assert_eq!(phone.event("torn", |_| true).await.seq, 10);
    phone.send_text("torn", "continue").await;
    assert_eq!(phone.event("torn", |_| true).await.seq, 11);
    phone.idle("torn").await;
    server.shutdown().await;
}

#[tokio::test]
async fn status_lines_utf8_tool_output_cap_and_failed_notifications() {
    let (server, _, _, mut phone) = setup().await;
    let session = phone.create().await;
    phone.subscribe(&session.id, 0).await;
    let handle = server.state.sessions.get(&session.id).unwrap();
    handle
        .inject(vec![
            EventKind::Thinking {
                item: "thinking".into(),
                text: "thought".into(),
            },
            EventKind::ToolStart {
                item: "tool".into(),
                tool: "shell".into(),
                summary: "running tests".into(),
            },
            EventKind::ToolEnd {
                item: "tool".into(),
                ok: true,
                output: "你".repeat(MAX_TOOL_OUTPUT),
            },
            EventKind::Notice {
                level: NoticeLevel::Error,
                text: "fixture error".into(),
            },
            EventKind::State {
                state: SessionState::Error,
                status_line: None,
            },
        ])
        .await;
    phone.wait(|w| matches!(w, Wire::Json(ServerMsg::SessionUpdate { session: s }) if s.status_line.as_deref() == Some("running tests"))).await;
    let event = phone
        .event(&session.id, |e| matches!(e, EventKind::ToolEnd { .. }))
        .await;
    if let EventKind::ToolEnd { output, .. } = event.kind {
        assert!(output.len() <= MAX_TOOL_OUTPUT);
        assert!(output.ends_with('你'));
    }
    notify(&mut phone, NotifyKind::Failed, Some(&session.id), None).await;
    notify(&mut phone, NotifyKind::Failed, Some(&session.id), None).await;
    assert_eq!(handle.info().state, SessionState::Error);
    server.shutdown().await;
}

#[tokio::test]
async fn close_session_deletes_history_and_broadcasts_to_every_connection() {
    let (server, store, _, mut phone) = setup().await;
    let mut other = Phone::pair(&server).await;
    let session = phone.create().await;
    phone.subscribe(&session.id, 0).await;
    phone.send_text(&session.id, "close").await;
    phone.idle(&session.id).await;
    phone
        .call("close_session", json!({"session":session.id}))
        .await;
    other.wait(|w| matches!(w, Wire::Json(ServerMsg::SessionClosed { session: id }) if id == &session.id)).await;
    assert!(store.records().is_empty());
    assert!(store.history(&session.id).is_empty());
    for (t, fields) in [
        ("subscribe", json!({"session":session.id,"after":0})),
        ("unsubscribe", json!({"session":session.id})),
        ("send_message", json!({"session":session.id,"text":"x"})),
        ("interrupt", json!({"session":session.id})),
        (
            "answer",
            json!({"session":session.id,"approval":"x","decision":"deny"}),
        ),
        ("close_session", json!({"session":session.id})),
    ] {
        let req = phone.request(t, fields).await;
        phone.error(Some(req), ErrorCode::NotFound).await;
    }
    server.shutdown().await;
}

#[tokio::test]
async fn backend_failures_return_internal_and_do_not_ack_unpersisted_work() {
    let (server, store, terms, mut phone) = setup().await;
    store.fail_upsert.store(true, Ordering::SeqCst);
    let req = phone
        .request("create_session", json!({"agent":"echo"}))
        .await;
    phone.error(Some(req), ErrorCode::Internal).await;
    store.fail_upsert.store(false, Ordering::SeqCst);
    let session = phone.create().await;
    store.fail_append.store(true, Ordering::SeqCst);
    let req = phone
        .request(
            "send_message",
            json!({"session":session.id,"text":"must not send"}),
        )
        .await;
    phone.error(Some(req), ErrorCode::Internal).await;
    assert!(store.history(&session.id).is_empty());
    store.fail_append.store(false, Ordering::SeqCst);
    phone.subscribe(&session.id, 0).await;
    phone.send_text(&session.id, "works").await;
    phone.idle(&session.id).await;
    store.fail_delete.store(true, Ordering::SeqCst);
    let req = phone
        .request("close_session", json!({"session":session.id}))
        .await;
    phone.error(Some(req), ErrorCode::Internal).await;
    assert!(server.state.sessions.get(&session.id).is_ok());
    store.fail_delete.store(false, Ordering::SeqCst);
    terms.fail_open.store(true, Ordering::SeqCst);
    let req = phone
        .request(
            "term_open",
            json!({"command":["fixture"],"cols":80,"rows":24}),
        )
        .await;
    phone.error(Some(req), ErrorCode::Internal).await;
    server.shutdown().await;
}

#[tokio::test]
async fn invalid_paths_agents_and_terminal_parameters_get_typed_errors() {
    let (server, _, _, mut phone) = setup().await;
    for (t, fields, code) in [
        (
            "create_session",
            json!({"agent":"missing"}),
            ErrorCode::AgentUnavailable,
        ),
        (
            "create_session",
            json!({"agent":"echo","cwd":"/does-not-exist-t03"}),
            ErrorCode::BadRequest,
        ),
        (
            "term_open",
            json!({"cols":80,"rows":24}),
            ErrorCode::BadRequest,
        ),
        (
            "term_open",
            json!({"command":[],"cols":80,"rows":24}),
            ErrorCode::BadRequest,
        ),
        (
            "term_open",
            json!({"command":["x"],"agent":"echo","cols":80,"rows":24}),
            ErrorCode::BadRequest,
        ),
        (
            "term_open",
            json!({"agent":"echo","cols":80,"rows":24}),
            ErrorCode::AgentUnavailable,
        ),
        (
            "term_open",
            json!({"command":["x"],"cols":0,"rows":24}),
            ErrorCode::BadRequest,
        ),
        (
            "term_attach",
            json!({"term":"missing"}),
            ErrorCode::NotFound,
        ),
        (
            "term_detach",
            json!({"term":"missing"}),
            ErrorCode::NotFound,
        ),
        ("term_kill", json!({"term":"missing"}), ErrorCode::NotFound),
        (
            "term_resize",
            json!({"term":"missing","cols":80,"rows":24}),
            ErrorCode::NotFound,
        ),
    ] {
        let req = phone.request(t, fields).await;
        phone.error(Some(req), code).await;
    }
    let data = phone
        .call(
            "create_session",
            json!({"agent":"echo","cwd":null,"title":"custom"}),
        )
        .await;
    assert_eq!(data["session"]["title"], "custom");
    server.shutdown().await;
}

#[tokio::test]
async fn terminal_open_binary_roundtrip_resize_detach_scrollback_reattach_and_exit() {
    let (server, _, _, mut phone) = setup().await;
    let mut observer = Phone::pair(&server).await;
    let term = phone.term().await;
    assert_eq!(phone.bytes(&term.id).await, b"ready\n");
    let data = [0, 255, 42, b'\n'];
    phone
        .raw(Message::Binary(
            binary::encode(binary::TERM_INPUT, &term.id, &data).into(),
        ))
        .await;
    assert_eq!(phone.bytes(&term.id).await, data);
    phone
        .call("term_resize", json!({"term":term.id,"cols":120,"rows":40}))
        .await;
    let list = phone.call("list_terms", json!({})).await;
    assert_eq!(list["terms"][0]["cols"], 120);
    assert_eq!(list["terms"][0]["rows"], 40);
    phone.call("term_detach", json!({"term":term.id})).await;
    phone
        .raw(Message::Binary(
            binary::encode(binary::TERM_INPUT, &term.id, b"no").into(),
        ))
        .await;
    phone.error(None, ErrorCode::Unauthorized).await;
    phone.call("term_attach", json!({"term":term.id})).await;
    let mut expected = b"ready\n".to_vec();
    expected.extend(data);
    assert_eq!(phone.bytes(&term.id).await, expected);
    phone.call("term_kill", json!({"term":term.id})).await;
    phone.wait(|w| matches!(w, Wire::Json(ServerMsg::TermExit { term: id, code: Some(0) }) if id == &term.id)).await;
    observer.call("list_terms", json!({})).await;
    assert!(
        !observer
            .inbox
            .iter()
            .any(|w| matches!(w, Wire::Json(ServerMsg::TermExit { .. })))
    );
    assert_eq!(
        phone.call("list_terms", json!({})).await["terms"][0]["running"],
        false
    );
    server.shutdown().await;
}

#[tokio::test]
async fn terminal_attention_is_global_works_detached_and_has_shared_per_terminal_cooldown() {
    let (server, _, terms, mut phone) = setup().await;
    let mut other = Phone::pair(&server).await;
    let term = phone.term().await;
    phone.bytes(&term.id).await;
    phone.call("term_detach", json!({"term":term.id})).await;
    terms.emit(&term.id, TermChunk::Attention);
    notify(&mut phone, NotifyKind::NeedsYou, None, Some(&term.id)).await;
    notify(&mut other, NotifyKind::NeedsYou, None, Some(&term.id)).await;
    terms.emit(&term.id, TermChunk::Attention);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    // Another terminal must not inherit this terminal's cooldown.
    let second = phone.term().await;
    terms.emit(&second.id, TermChunk::Attention);
    notify(&mut phone, NotifyKind::NeedsYou, None, Some(&second.id)).await;
    assert!(!phone.inbox.iter().any(
        |w| matches!(w, Wire::Json(ServerMsg::Notify { term: Some(t), .. }) if t == &term.id)
    ));
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::time::resume();
    terms.emit(&term.id, TermChunk::Attention);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    // Keep sockets alive before crossing the cooldown boundary.
    phone.send(json!({"t":"ping","n":59})).await;
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::Pong { n: 59 })))
        .await;
    other.send(json!({"t":"ping","n":59})).await;
    other
        .wait(|w| matches!(w, Wire::Json(ServerMsg::Pong { n: 59 })))
        .await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    terms.emit(&term.id, TermChunk::Attention);
    notify(&mut phone, NotifyKind::NeedsYou, None, Some(&term.id)).await;
    notify(&mut other, NotifyKind::NeedsYou, None, Some(&term.id)).await;
    server.shutdown().await;
}

#[tokio::test]
async fn shutdown_closes_unauthenticated_and_authenticated_sockets_and_releases_port() {
    let (server, _, _, mut phone) = setup().await;
    let mut unauth = Phone::connect(&server).await;
    let addr = server.local_addr();
    server.shutdown().await;
    phone.close_code(1001).await;
    unauth.close_code(1001).await;
    let _listener = tokio::net::TcpListener::bind(addr).await.unwrap();
}

#[tokio::test]
async fn non_loopback_and_ipv6_bind_addresses_are_rejected() {
    for bind in ["0.0.0.0:0", "[::1]:0", "127.0.0.2:0"] {
        let mut cfg = config();
        cfg.bind = bind.parse().unwrap();
        assert!(
            Server::start_with(
                cfg,
                Arc::new(MemoryStore::default()),
                Arc::new(MemoryTerms::default()),
                vec![Arc::new(EchoAdapter)]
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn real_backends_pair_chat_persistence_and_revoke() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let server = Server::start(
        config(),
        store.clone(),
        Arc::new(TermManager::new(512 << 10)),
        vec![Arc::new(EchoAdapter)],
    )
    .await
    .unwrap();
    let mut phone = Phone::pair(&server).await;
    let session = phone.create().await;
    phone.subscribe(&session.id, 0).await;
    phone.send_text(&session.id, "integration").await;
    phone.idle(&session.id).await;
    assert!(!store.events_after(&session.id, 0, 100).unwrap().is_empty());
    server.revoke(&phone.device).await.unwrap();
    phone.close_code(4401).await;
    server.shutdown().await;
}

#[tokio::test]
async fn real_backends_terminal_scrollback_and_exit() {
    let server = Server::start(
        config(),
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(TermManager::new(512 << 10)),
        vec![Arc::new(EchoAdapter)],
    )
    .await
    .unwrap();
    let mut phone = Phone::pair(&server).await;
    let command = if cfg!(windows) {
        vec![
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()),
            "/C".into(),
            "echo tether-integration".into(),
        ]
    } else {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf tether-integration".into(),
        ]
    };
    let term: TermInfo = serde_json::from_value(
        phone
            .call("term_open", json!({"command":command,"cols":80,"rows":24}))
            .await["term"]
            .clone(),
    )
    .unwrap();
    let mut bytes = Vec::new();
    while !String::from_utf8_lossy(&bytes).contains("tether-integration") {
        bytes.extend(phone.bytes(&term.id).await);
    }
    phone
        .wait(|w| matches!(w, Wire::Json(ServerMsg::TermExit { term: id, .. }) if id == &term.id))
        .await;
    server.shutdown().await;
}

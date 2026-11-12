use super::*;
use tether_agent_api::{AgentAdapter, AgentCommand, AgentOutput, SpawnOptions};
use tokio::sync::mpsc;

/// Only metadata is customized; all agent execution still uses EchoAdapter.
struct DescribedEcho(AgentInfo);
impl AgentAdapter for DescribedEcho {
    fn info(&self) -> AgentInfo {
        self.0.clone()
    }
    fn spawn(
        &self,
        opts: SpawnOptions,
        out: mpsc::UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<mpsc::UnboundedSender<AgentCommand>> {
        EchoAdapter.spawn(opts, out)
    }
}

#[tokio::test]
async fn terminal_agent_resolution_and_unavailable_or_terminal_only_chat() {
    let mut terminal = EchoAdapter.info();
    terminal.id = "terminal-echo".into();
    terminal.chat = false;
    terminal.terminal_command = vec!["fixture-shell".into(), "--fixture".into()];
    let mut missing = EchoAdapter.info();
    missing.id = "unavailable".into();
    missing.available = false;
    let server = Server::start_with(
        config(),
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryTerms::default()),
        vec![
            Arc::new(EchoAdapter),
            Arc::new(DescribedEcho(terminal)),
            Arc::new(DescribedEcho(missing)),
        ],
    )
    .await
    .unwrap();
    let mut phone = Phone::pair(&server).await;
    for agent in ["terminal-echo", "unavailable"] {
        let req = phone
            .request("create_session", json!({"agent":agent}))
            .await;
        phone.error(Some(req), ErrorCode::AgentUnavailable).await;
    }
    let term: TermInfo = serde_json::from_value(
        phone
            .call(
                "term_open",
                json!({"agent":"terminal-echo","command":null,"cols":80,"rows":24}),
            )
            .await["term"]
            .clone(),
    )
    .unwrap();
    assert_eq!(term.command, vec!["fixture-shell", "--fixture"]);
    assert_eq!(phone.bytes(&term.id).await, b"ready\n");
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_removes_the_actor_even_when_its_requesting_connection_is_revoked() {
    let (server, store, _, mut phone) = setup().await;
    let mut observer = Phone::pair(&server).await;
    let session = phone.create().await;
    let barrier = Arc::new(Barrier::new(2));
    let release = barrier.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    *lock(&store.delete_hook) = Some(Box::new(move || {
        entered_tx.send(()).unwrap();
        release.wait();
    }));
    phone
        .request("close_session", json!({"session":session.id}))
        .await;
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    server.revoke(&phone.device).await.unwrap();
    phone.close_code(4401).await;
    barrier.wait();
    observer.wait(|w| matches!(w, Wire::Json(ServerMsg::SessionClosed { session: id }) if id == &session.id)).await;
    assert_eq!(
        observer.call("list_sessions", json!({})).await["sessions"],
        json!([])
    );
    server.shutdown().await;
}

#[tokio::test]
async fn authentication_racing_revoke_never_leaves_a_valid_connection() {
    let server = start(
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryTerms::default()),
    )
    .await;
    for _ in 0..10 {
        let phone = Phone::pair(&server).await;
        let mut racing = Phone::connect(&server).await;
        let (result, _) = tokio::join!(
            server.revoke(&phone.device),
            racing
                .send(json!({"t":"hello","version":1,"token":phone.token,"device_name":"racing"}))
        );
        assert!(result.unwrap().is_some());
        match racing.read().await {
            Wire::Json(ServerMsg::Welcome { .. }) => {}
            Wire::Json(ServerMsg::Rejected {
                reason: RejectReason::Revoked,
            }) => {}
            Wire::Close(4401) => continue,
            other => panic!("unexpected auth/revoke result: {other:?}"),
        }
        racing.close_code(4401).await;
        assert!(!server.connected_devices().contains(&phone.device));
    }
    server.shutdown().await;
}

#[tokio::test]
async fn message_that_fits_request_but_not_event_envelope_is_bad_request() {
    let (server, store, _, mut phone) = setup().await;
    let session = phone.create().await;
    let text = "x".repeat(MAX_FRAME - 100);
    let value = json!({"t":"send_message","req":2,"session":session.id,"text":text});
    assert!(value.to_string().len() <= MAX_FRAME);
    phone.send(value).await;
    phone.error(Some(2), ErrorCode::BadRequest).await;
    assert!(store.history(&session.id).is_empty());
    server.shutdown().await;
}

#[tokio::test]
async fn large_terminal_chunks_are_split_and_exact_frame_limit_is_accepted() {
    let (server, _, terms, mut phone) = setup().await;
    let term = phone.term().await;
    phone.bytes(&term.id).await;
    let maximum_payload = MAX_FRAME - 2 - term.id.len();
    let input = vec![0xab; maximum_payload];
    phone
        .raw(Message::Binary(
            binary::encode(binary::TERM_INPUT, &term.id, &input).into(),
        ))
        .await;
    assert_eq!(phone.bytes(&term.id).await, input);
    let output = vec![0xcd; maximum_payload + 17];
    terms.emit(&term.id, TermChunk::Data(output.clone().into()));
    let first = phone.bytes(&term.id).await;
    let second = phone.bytes(&term.id).await;
    assert_eq!(first.len(), maximum_payload);
    assert_eq!(second.len(), 17);
    assert_eq!([first, second].concat(), output);
    server.shutdown().await;
}

#[tokio::test]
async fn approval_decisions_must_be_offered_and_idle_interrupt_is_a_noop() {
    let (server, _, _, mut phone) = setup().await;
    let session = phone.create().await;
    phone.call("interrupt", json!({"session":session.id})).await;
    phone.subscribe(&session.id, 0).await;
    phone
        .send_text(&session.id, "approve:restricted choices")
        .await;
    let event = phone
        .event(&session.id, |e| matches!(e, EventKind::Approval { .. }))
        .await;
    let mut approval = match event.kind {
        EventKind::Approval { approval } => approval,
        _ => unreachable!(),
    };
    approval.choices = vec![Decision::Deny];
    server
        .state
        .sessions
        .get(&session.id)
        .unwrap()
        .inject(vec![EventKind::Approval {
            approval: approval.clone(),
        }])
        .await;
    let req = phone
        .request(
            "answer",
            json!({"session":session.id,"approval":approval.id,"decision":"allow_session"}),
        )
        .await;
    phone.error(Some(req), ErrorCode::BadRequest).await;
    phone
        .call(
            "answer",
            json!({"session":session.id,"approval":approval.id,"decision":"deny"}),
        )
        .await;
    phone.idle(&session.id).await;
    server.shutdown().await;
}

#[tokio::test]
async fn restored_sessions_do_not_spawn_until_first_message() {
    let store = Arc::new(MemoryStore::default());
    let mut record = seeded("lazy", SessionState::NeedsYou, 0);
    record.native_id = None;
    store.seed(record, vec![]);
    let server = start(store.clone(), Arc::new(MemoryTerms::default())).await;
    let mut phone = Phone::pair(&server).await;
    phone.call("list_sessions", json!({})).await;
    phone.subscribe("lazy", 0).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(store.records()[0].native_id.is_none());
    assert_eq!(store.records()[0].info.state, SessionState::Idle);
    assert!(store.history("lazy").is_empty());
    phone.send_text("lazy", "first").await;
    phone.idle("lazy").await;
    assert!(store.records()[0].native_id.is_some());
    server.shutdown().await;
}

#[tokio::test]
async fn authentication_backend_failure_is_internal_not_an_invalid_token() {
    let (server, store, _, _) = setup().await;
    store.fail_auth.store(true, Ordering::SeqCst);
    let mut phone = Phone::connect(&server).await;
    phone
        .send(json!({"t":"hello","version":1,"token":"x","device_name":"x"}))
        .await;
    phone.error(None, ErrorCode::Internal).await;
    phone.close_code(1011).await;
    server.shutdown().await;
}

#[tokio::test]
async fn oversized_metadata_is_rejected_and_oversized_replies_keep_the_request_id() {
    let (server, store, _, mut phone) = setup().await;
    let req = phone
        .request(
            "create_session",
            json!({"agent":"echo","title":"x".repeat(MAX_FRAME - 100)}),
        )
        .await;
    phone.error(Some(req), ErrorCode::BadRequest).await;
    assert!(store.records().is_empty());
    server.shutdown().await;
    let mut record = seeded("oversized-list", SessionState::Idle, 0);
    record.info.title = "x".repeat(MAX_FRAME);
    store.seed(record, vec![]);
    let server = start(store, Arc::new(MemoryTerms::default())).await;
    let mut phone = Phone::pair(&server).await;
    let req = phone.request("list_sessions", json!({})).await;
    phone.error(Some(req), ErrorCode::BadRequest).await;
    phone.call("list_agents", json!({})).await;
    server.shutdown().await;
}

#[test]
fn default_cwd_is_home_and_paths_are_plain() {
    let home = std::env::home_dir().unwrap();
    let home = std::fs::canonicalize(home).unwrap();
    for cwd in [None, Some("~".to_string()), Some("  ".to_string())] {
        let got = crate::gateway::cwd_path(cwd).unwrap();
        assert_eq!(std::fs::canonicalize(&got).unwrap(), home);
        assert!(!got.to_string_lossy().starts_with(r"\?\"));
    }
    let dir = tempfile::tempdir().unwrap();
    let got = crate::gateway::cwd_path(Some(dir.path().display().to_string())).unwrap();
    assert!(!got.to_string_lossy().starts_with(r"\?\"));
    assert!(got.is_dir());
    assert!(crate::gateway::cwd_path(Some("~/tether-no-such-dir-xyz".into())).is_err());
}

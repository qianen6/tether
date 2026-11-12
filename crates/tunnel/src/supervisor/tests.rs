use super::*;
use crate::process::{Helper, Wait};
use serde_json::{Value, json};
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

struct MemorySpawner {
    peers: mpsc::UnboundedSender<Peer>,
    failures: Arc<AtomicUsize>,
}

struct Peer {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    exit: watch::Sender<Option<Exit>>,
    kills: Arc<AtomicUsize>,
}

struct MemoryProcess {
    status: watch::Receiver<Option<Exit>>,
    exit: watch::Sender<Option<Exit>>,
    kills: Arc<AtomicUsize>,
}

impl Spawner for MemorySpawner {
    fn spawn(&mut self, _: &TunnelConfig) -> Result<Helper> {
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            bail!("simulated spawn failure");
        }
        let (a, b) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(a);
        let (peer_read, peer_write) = tokio::io::split(b);
        let (exit, status) = watch::channel(None);
        let kills = Arc::new(AtomicUsize::new(0));
        self.peers
            .send(Peer {
                reader: BufReader::new(peer_read),
                writer: peer_write,
                exit: exit.clone(),
                kills: kills.clone(),
            })
            .map_err(|_| anyhow!("test stopped"))?;
        Ok(Helper {
            reader: Box::new(reader),
            writer: Box::new(writer),
            process: Box::new(MemoryProcess {
                status,
                exit,
                kills,
            }),
        })
    }
}

impl Process for MemoryProcess {
    fn wait(&mut self) -> Wait<'_> {
        Box::pin(async move {
            loop {
                if let Some(exit) = *self.status.borrow_and_update() {
                    return Ok(exit);
                }
                self.status.changed().await.map_err(io::Error::other)?;
            }
        })
    }

    fn kill(&mut self) -> io::Result<()> {
        if self.exit.borrow().is_none() {
            self.kills.fetch_add(1, Ordering::SeqCst);
            self.exit.send_replace(Some(Exit {
                success: false,
                code: None,
            }));
        }
        Ok(())
    }
}

impl Peer {
    async fn next(&mut self) -> Value {
        let mut line = String::new();
        let n = timeout(Duration::from_secs(2), self.reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(n > 0, "unexpected EOF");
        serde_json::from_str(&line).unwrap()
    }

    async fn send(&mut self, value: Value) {
        let mut frame = serde_json::to_vec(&value).unwrap();
        frame.push(b'\n');
        self.writer.write_all(&frame).await.unwrap();
    }

    async fn saved(&mut self, id: &str, state: &State) {
        self.send(json!({"event":"save","id":id,"state":state}))
            .await;
        assert_eq!(self.next().await, json!({"cmd":"saved","id":id,"ok":true}));
    }

    fn crash(&self) {
        self.exit.send_replace(Some(Exit {
            success: false,
            code: Some(17),
        }));
    }

    async fn graceful_eof(mut self) {
        let mut line = String::new();
        assert_eq!(
            timeout(Duration::from_secs(2), self.reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        self.exit.send_replace(Some(Exit {
            success: true,
            code: Some(0),
        }));
    }
}

fn timing() -> Timing {
    Timing {
        restart_min: Duration::from_millis(20),
        restart_max: Duration::from_millis(80),
        close: Duration::from_millis(40),
        ..Timing::default()
    }
}

fn config(path: std::path::PathBuf) -> TunnelConfig {
    TunnelConfig {
        helper: "unused-memory-helper".into(),
        forward: "127.0.0.1:12345".parse().unwrap(),
        state_path: path,
        name: "开发 电脑".into(),
        derp_map_url: Some("http://relay.invalid/map".into()),
        qr_prefix: "tether1:".into(),
    }
}

struct Running {
    tunnel: Tunnel,
    peer: Peer,
    peers: mpsc::UnboundedReceiver<Peer>,
    failures: Arc<AtomicUsize>,
    dir: tempfile::TempDir,
    initial: Value,
}

async fn boot(saved: Option<State>, timing: Timing) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path().join("state.json"));
    if let Some(state) = saved {
        StateFile(cfg.state_path.clone()).save(state).await.unwrap();
    }
    let (tx, mut peers) = mpsc::unbounded_channel();
    let failures = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(start(
        cfg,
        MemorySpawner {
            peers: tx,
            failures: failures.clone(),
        },
        timing,
    ));
    let mut peer = timeout(Duration::from_secs(2), peers.recv())
        .await
        .unwrap()
        .unwrap();
    let initial = peer.next().await;
    peer.send(json!({"event":"ready","address":""})).await;
    let tunnel = task.await.unwrap().unwrap();
    Running {
        tunnel,
        peer,
        peers,
        failures,
        dir,
        initial,
    }
}

async fn finish(tunnel: Tunnel, peer: Peer) {
    let (closed, ()) = tokio::join!(tunnel.close(), peer.graceful_eof());
    closed.unwrap();
}

fn state(phones: &[&str]) -> State {
    State {
        identity: "saved-private-identity".into(),
        allowed: phones.iter().map(|s| s.to_string()).collect(),
    }
}

async fn window(tunnel: &Tunnel, peer: &mut Peer) -> PairWindow {
    let (result, ()) = tokio::join!(
        tunnel.pair(Duration::from_secs(120), "secret".into()),
        async {
            let command = peer.next().await;
            peer.send(json!({"event":"pairing","id":command["id"],"qr":"tether1:fixture","expiresAt":now_ms()/1000+120})).await;
        }
    );
    result.unwrap()
}

#[tokio::test]
async fn startup_restores_identity_allowlist_and_exact_config() {
    let host = boot(Some(state(&["nodekey:first"])), timing()).await;
    assert_eq!(
        host.initial,
        json!({"v":1,"name":"开发 电脑","identity":"saved-private-identity",
        "allowed":["nodekey:first"],"forward":"127.0.0.1:12345","derpMapUrl":"http://relay.invalid/map","identifyPhone":false})
    );
    assert_eq!(host.tunnel.address(), None);
    finish(host.tunnel, host.peer).await;
}

#[tokio::test]
async fn pairing_join_and_revoke_wait_for_durable_save() {
    let mut host = boot(None, timing()).await;
    let expires = now_ms() / 1000 + 60;
    let (result, ()) = tokio::join!(
        host.tunnel
            .pair(Duration::from_millis(59_001), "密钥\n\"token\"".into()),
        async {
            let command = host.peer.next().await;
            assert_eq!(
                command,
                json!({"cmd":"pair","id":"t1","ttl":60,"payload":"密钥\n\"token\"","qrPrefix":"tether1:"})
            );
            host.peer
                .send(json!({"event":"pairing","id":"t1","qr":"tether1:qr","expiresAt":expires}))
                .await;
        }
    );
    let pair = result.unwrap();
    assert_eq!(pair.qr, "tether1:qr");
    assert_eq!(pair.expires_at, expires * 1000);
    host.peer.saved("s1", &state(&["nodekey:phone"])).await;
    let stored = StateFile(host.dir.path().join("state.json"))
        .load()
        .await
        .unwrap();
    assert!(stored == state(&["nodekey:phone"]));
    host.peer.send(json!({"event":"paired","id":"t1","phone":"nodekey:phone","device":"Pixel 9","address":"tc-address"})).await;
    assert_eq!(
        pair.joined.await.unwrap(),
        PhoneJoined {
            phone_key: "nodekey:phone".into(),
            device_name: "Pixel 9".into()
        }
    );
    assert_eq!(host.tunnel.address().as_deref(), Some("tc-address"));
    host.peer
        .send(json!({"event":"pairingClosed","id":"t1","reason":"paired"}))
        .await;
    let (revoked, ()) = tokio::join!(host.tunnel.revoke("nodekey:phone"), async {
        assert_eq!(
            host.peer.next().await,
            json!({"cmd":"revoke","id":"t2","phone":"nodekey:phone"})
        );
        host.peer.saved("s2", &state(&[])).await;
        host.peer
            .send(json!({"event":"allowed","id":"t2","allowed":[],"address":""}))
            .await;
    });
    revoked.unwrap();
    assert_eq!(host.tunnel.address(), None);
    assert!(
        StateFile(host.dir.path().join("state.json"))
            .load()
            .await
            .unwrap()
            .allowed
            .is_empty()
    );
    finish(host.tunnel, host.peer).await;
}

#[tokio::test]
async fn concurrent_commands_match_out_of_order_ids_and_surface_errors() {
    let mut host = boot(None, timing()).await;
    let (allowed, revoked, ()) = tokio::join!(
        host.tunnel.set_allowed(vec!["nodekey:a".into()]),
        host.tunnel.revoke("bad-key"),
        async {
            let a = host.peer.next().await;
            let b = host.peer.next().await;
            let (set, revoke) = if a["cmd"] == "setAllowed" {
                (a, b)
            } else {
                (b, a)
            };
            assert_eq!(set["allowed"], json!(["nodekey:a"]));
            assert_eq!(revoke["phone"], "bad-key");
            host.peer
                .send(json!({"event":"error","id":revoke["id"],"code":"invalid-allowed"}))
                .await;
            host.peer.saved("s1", &state(&["nodekey:a"])).await;
            host.peer.send(json!({"event":"allowed","id":set["id"],"allowed":["nodekey:a"],"address":"tc-a"})).await;
        }
    );
    allowed.unwrap();
    assert!(revoked.unwrap_err().to_string().contains("invalid-allowed"));
    assert_eq!(host.tunnel.address().as_deref(), Some("tc-a"));
    finish(host.tunnel, host.peer).await;
}

#[tokio::test]
async fn separate_windows_close_and_join_independently() {
    let mut host = boot(None, timing()).await;
    let first = window(&host.tunnel, &mut host.peer).await;
    let second = window(&host.tunnel, &mut host.peer).await;
    host.peer
        .send(json!({"event":"pairingClosed","id":"t1","reason":"expired"}))
        .await;
    assert!(first.joined.await.is_err());
    host.peer.send(json!({"event":"paired","id":"t2","phone":"nodekey:p","device":"Phone","address":"tc-p"})).await;
    assert_eq!(second.joined.await.unwrap().device_name, "Phone");
    finish(host.tunnel, host.peer).await;
}

#[tokio::test]
async fn pairing_expiry_watchdog_drops_joined_without_closed_event() {
    let mut host = boot(None, timing()).await;
    let (pair, ()) = tokio::join!(
        host.tunnel.pair(Duration::from_secs(1), String::new()),
        async {
            let command = host.peer.next().await;
            host.peer.send(json!({"event":"pairing","id":command["id"],"qr":"tether1:expired","expiresAt":0})).await;
        }
    );
    assert!(
        timeout(Duration::from_secs(1), pair.unwrap().joined)
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(host.peer.next().await, json!({"cmd":"cancel","id":"t1"}));
    finish(host.tunnel, host.peer).await;
}

#[tokio::test]
async fn failed_save_is_nacked_and_not_used_on_restart() {
    let mut host = boot(None, timing()).await;
    let path = host.dir.path().join("state.json");
    std::fs::create_dir(&path).unwrap();
    host.peer
        .send(json!({"event":"save","id":"s1","state":state(&["nodekey:new"])}))
        .await;
    assert_eq!(
        host.peer.next().await,
        json!({"cmd":"saved","id":"s1","ok":false})
    );
    assert!(path.is_dir());
    std::fs::remove_dir(&path).unwrap();
    host.peer.crash();
    let mut next = timeout(Duration::from_secs(2), host.peers.recv())
        .await
        .unwrap()
        .unwrap();
    let config = next.next().await;
    assert_eq!(config["identity"], "");
    assert_eq!(config["allowed"], json!([]));
    next.send(json!({"event":"ready","address":""})).await;
    finish(host.tunnel, next).await;
}

#[tokio::test]
async fn unexpected_process_exit_restarts_restores_and_does_not_replay_requests() {
    let mut host = boot(Some(state(&["nodekey:old"])), timing()).await;
    let pair = window(&host.tunnel, &mut host.peer).await;
    host.peer.saved("s1", &state(&["nodekey:new"])).await;
    let (failed, ()) = tokio::join!(host.tunnel.revoke("nodekey:new"), async {
        let revoke = host.peer.next().await;
        assert_eq!(revoke["id"], "t2");
        host.peer.crash(); // stdout deliberately stays open: wait() must detect exit too.
    });
    assert!(failed.unwrap_err().to_string().contains("not replayed"));
    assert!(pair.joined.await.is_err());
    assert_eq!(host.tunnel.address(), None);
    let mut next = timeout(Duration::from_secs(2), host.peers.recv())
        .await
        .unwrap()
        .unwrap();
    let restored = next.next().await;
    assert_eq!(restored["identity"], "saved-private-identity");
    assert_eq!(restored["allowed"], json!(["nodekey:new"]));
    next.send(json!({"event":"ready","address":"tc-restored"}))
        .await;
    timeout(Duration::from_secs(1), async {
        while host.tunnel.address().as_deref() != Some("tc-restored") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (revoked, ()) = tokio::join!(host.tunnel.revoke("nodekey:missing"), async {
        // Synchronize with ready before the API request (requests during startup fail fast).
        let command = next.next().await;
        assert_eq!(command["cmd"], "revoke");
        assert_eq!(command["id"], "t3");
        next.send(json!({"event":"allowed","id":command["id"],"allowed":["nodekey:new"],"address":"tc-restored"})).await;
    });
    revoked.unwrap();
    assert_eq!(host.tunnel.address().as_deref(), Some("tc-restored"));
    finish(host.tunnel, next).await;
}

#[test]
fn exponential_backoff_caps_at_thirty_seconds() {
    let mut backoff = Backoff::new(Timing::default());
    let seconds: Vec<_> = (0..9).map(|_| backoff.next().as_secs()).collect();
    assert_eq!(seconds, [1, 2, 4, 8, 16, 30, 30, 30, 30]);
}

#[tokio::test]
async fn failed_respawns_are_retried_and_close_interrupts_long_backoff() {
    let mut host = boot(None, timing()).await;
    host.failures.store(2, Ordering::SeqCst);
    host.peer.crash();
    let mut next = timeout(Duration::from_secs(2), host.peers.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(host.failures.load(Ordering::SeqCst), 0);
    assert_eq!(next.next().await["v"], 1);
    next.send(json!({"event":"ready","address":""})).await;
    finish(host.tunnel, next).await;

    let host = boot(
        None,
        Timing {
            restart_min: Duration::from_secs(30),
            ..timing()
        },
    )
    .await;
    host.peer.crash();
    // A command is rejected while disconnected, rather than replayed on the new helper.
    assert!(host.tunnel.revoke("nodekey:a").await.is_err());
    timeout(Duration::from_secs(1), host.tunnel.close())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn close_times_out_then_kills_and_drops_open_windows() {
    let mut host = boot(None, timing()).await;
    let pair = window(&host.tunnel, &mut host.peer).await;
    let kills = host.peer.kills.clone();
    timeout(Duration::from_secs(1), host.tunnel.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kills.load(Ordering::SeqCst), 1);
    assert!(pair.joined.await.is_err());
}

#[tokio::test]
async fn close_gracefully_reaps_without_killing_and_drop_also_closes_stdin() {
    let host = boot(None, timing()).await;
    let kills = host.peer.kills.clone();
    finish(host.tunnel, host.peer).await;
    assert_eq!(kills.load(Ordering::SeqCst), 0);

    let host = boot(None, timing()).await;
    drop(host.tunnel);
    host.peer.graceful_eof().await;
}

#[tokio::test]
async fn command_timeout_cancels_unacknowledged_pair_but_keeps_session_usable() {
    let mut host = boot(
        None,
        Timing {
            request: Duration::from_millis(40),
            ..timing()
        },
    )
    .await;
    let (result, ()) = tokio::join!(
        host.tunnel.pair(Duration::from_secs(900), "p".into()),
        async {
            assert_eq!(host.peer.next().await["ttl"], 900);
            assert_eq!(host.peer.next().await, json!({"cmd":"cancel","id":"t1"}));
        }
    );
    assert!(result.unwrap_err().to_string().contains("timed out"));
    let pair = window(&host.tunnel, &mut host.peer).await;
    finish(host.tunnel, host.peer).await;
    assert!(pair.joined.await.is_err());
}

#[tokio::test]
async fn cancelled_pair_call_cancels_window_when_reply_arrives() {
    let mut host = boot(None, timing()).await;
    {
        let call = host.tunnel.pair(Duration::from_secs(30), String::new());
        tokio::pin!(call);
        tokio::select! {
            _ = &mut call => panic!("unexpected response"),
            command = host.peer.next() => assert_eq!(command["cmd"], "pair"),
        }
    }
    host.peer.send(json!({"event":"pairing","id":"t1","qr":"tether1:cancelled","expiresAt":now_ms()/1000+30})).await;
    assert_eq!(host.peer.next().await, json!({"cmd":"cancel","id":"t1"}));
    finish(host.tunnel, host.peer).await;
}

#[tokio::test]
async fn zero_and_subsecond_ttl_preserve_default_and_round_up() {
    let mut host = boot(None, timing()).await;
    for (ttl, expected) in [
        (Duration::ZERO, 0),
        (Duration::from_nanos(1), 1),
        (Duration::MAX, 900),
    ] {
        let (pair, ()) = tokio::join!(host.tunnel.pair(ttl, String::new()), async {
            let command = host.peer.next().await;
            assert_eq!(command["ttl"], expected);
            host.peer
                .send(json!({"event":"error","id":command["id"],"code":"pair-failed"}))
                .await;
        });
        assert!(pair.unwrap_err().to_string().contains("pair-failed"));
    }
    finish(host.tunnel, host.peer).await;
}

#[tokio::test]
async fn startup_errors_and_timeouts_do_not_hang_or_retry() {
    for send_error in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut peers) = mpsc::unbounded_channel();
        let task = tokio::spawn(start(
            config(dir.path().join("state.json")),
            MemorySpawner {
                peers: tx,
                failures: Arc::new(AtomicUsize::new(0)),
            },
            Timing {
                startup: Duration::from_millis(40),
                ..timing()
            },
        ));
        let mut peer = peers.recv().await.unwrap();
        peer.next().await;
        if send_error {
            peer.send(json!({"event":"error","code":"invalid-identity"}))
                .await;
        }
        let error = timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(error.contains(if send_error {
            "invalid-identity"
        } else {
            "ready timed out"
        }));
        assert_eq!(peer.kills.load(Ordering::SeqCst), 1);
        assert!(peers.recv().await.is_none());
    }
}

#[tokio::test]
async fn native_missing_executable_and_invalid_forward_fail_offline() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path().join("state.json"));
    cfg.helper = dir.path().join("nonexistent-tailpocket-host.exe");
    assert!(Tunnel::start(cfg.clone()).await.is_err());
    cfg.forward = "0.0.0.0:12345".parse().unwrap();
    assert!(
        Tunnel::start(cfg)
            .await
            .unwrap_err()
            .to_string()
            .contains("loopback")
    );
}

#[tokio::test]
async fn corrupt_saved_state_is_not_silently_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, "not json").unwrap();
    assert!(
        Tunnel::start(config(path.clone()))
            .await
            .unwrap_err()
            .to_string()
            .contains("decode tunnel state")
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "not json");
}

#[test]
fn public_handle_is_send_sync() {
    fn assert_traits<T: Send + Sync>() {}
    assert_traits::<Tunnel>();
    assert_traits::<TunnelConfig>();
    assert_traits::<PairWindow>();
    assert_traits::<PhoneJoined>();
}

#[tokio::test]
async fn startup_save_is_persisted_and_acked_before_ready() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path().join("state.json"));
    let (tx, mut peers) = mpsc::unbounded_channel();
    let task = tokio::spawn(start(
        cfg,
        MemorySpawner {
            peers: tx,
            failures: Arc::new(AtomicUsize::new(0)),
        },
        timing(),
    ));
    let mut peer = peers.recv().await.unwrap();
    peer.next().await;
    peer.saved("s-before-ready", &state(&["nodekey:restored"]))
        .await;
    assert!(
        StateFile(dir.path().join("state.json"))
            .load()
            .await
            .unwrap()
            == state(&["nodekey:restored"])
    );
    peer.send(json!({"event":"ready","address":"tc-restored"}))
        .await;
    let tunnel = task.await.unwrap().unwrap();
    assert_eq!(tunnel.address().as_deref(), Some("tc-restored"));
    finish(tunnel, peer).await;
}

#[tokio::test]
async fn eof_and_malformed_stdout_force_reap_before_restart() {
    for eof in [true, false] {
        let mut host = boot(Some(state(&["nodekey:a"])), timing()).await;
        if eof {
            host.peer.writer.shutdown().await.unwrap();
        } else {
            host.peer.writer.write_all(b"not-json\n").await.unwrap();
        }
        let mut next = timeout(Duration::from_secs(2), host.peers.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(host.peer.kills.load(Ordering::SeqCst), 1);
        assert_eq!(next.next().await["allowed"], json!(["nodekey:a"]));
        next.send(json!({"event":"ready","address":"tc-a"})).await;
        finish(host.tunnel, next).await;
    }
}

#[tokio::test]
async fn cancelling_start_cleans_up_its_already_spawned_helper() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut peers) = mpsc::unbounded_channel();
    let task = tokio::spawn(start(
        config(dir.path().join("state.json")),
        MemorySpawner {
            peers: tx,
            failures: Arc::new(AtomicUsize::new(0)),
        },
        timing(),
    ));
    let mut peer = peers.recv().await.unwrap();
    peer.next().await;
    let kills = peer.kills.clone();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    peer.graceful_eof().await;
    assert_eq!(kills.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn blocked_stdin_does_not_delay_close_until_writer_timeout() {
    let host = boot(None, timing()).await;
    {
        let call = host
            .tunnel
            .pair(Duration::from_secs(30), "x".repeat(32_768));
        tokio::pin!(call);
        tokio::select! {
            _ = &mut call => panic!("a blocked helper must not reply"),
            _ = sleep(Duration::from_millis(20)) => {}
        }
    }
    timeout(Duration::from_secs(1), host.tunnel.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(host.peer.kills.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_expiry_returns_error_without_instant_overflow_or_panic() {
    for expiry in [u64::MAX, u64::MAX / 1000] {
        let mut host = boot(None, timing()).await;
        let (pair, ()) = tokio::join!(
            host.tunnel.pair(Duration::from_secs(1), String::new()),
            async {
                let command = host.peer.next().await;
                host.peer.send(json!({"event":"pairing","id":command["id"],"qr":"bad-expiry","expiresAt":expiry})).await;
            }
        );
        assert!(pair.is_err());
        timeout(Duration::from_secs(1), host.tunnel.close())
            .await
            .unwrap()
            .unwrap();
    }
}

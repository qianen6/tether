use std::sync::Mutex;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tether_tunnel::{PairWindow, PhoneJoined};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use super::*;
use crate::link::BoxFuture;

#[derive(Default)]
struct Calls {
    payloads: Vec<String>,
    windows: Vec<oneshot::Sender<PhoneJoined>>,
    revoked: Vec<String>,
    allowed: Vec<Vec<String>>,
    closed: bool,
}

#[derive(Default)]
struct FakeTunnel {
    calls: Mutex<Calls>,
}

impl FakeTunnel {
    fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
        self.calls.lock().unwrap()
    }
    /// The phone scanned the last window and joined.
    fn join(&self, key: &str) {
        let tx = self.calls().windows.pop().unwrap();
        tx.send(PhoneJoined {
            phone_key: key.into(),
            device_name: "phone".into(),
        })
        .unwrap();
    }
}

impl TunnelBackend for FakeTunnel {
    fn pair(&self, ttl: Duration, payload: String) -> BoxFuture<'_, anyhow::Result<PairWindow>> {
        let (tx, joined) = oneshot::channel();
        let mut calls = self.calls();
        calls.payloads.push(payload.clone());
        calls.windows.push(tx);
        Box::pin(async move {
            Ok(PairWindow {
                qr: format!("{QR_PREFIX}fake"),
                expires_at: tether_proto::now_ms() + ttl.as_millis() as u64,
                joined,
            })
        })
    }
    fn revoke<'a>(&'a self, phone_key: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        self.calls().revoked.push(phone_key.into());
        Box::pin(async { Ok(()) })
    }
    fn set_allowed(&self, phone_keys: Vec<String>) -> BoxFuture<'_, anyhow::Result<()>> {
        self.calls().allowed.push(phone_keys);
        Box::pin(async { Ok(()) })
    }
    fn address(&self) -> Option<String> {
        Some("100.64.0.1".into())
    }
    fn close(self: Arc<Self>) -> BoxFuture<'static, anyhow::Result<()>> {
        self.calls().closed = true;
        Box::pin(async { Ok(()) })
    }
}

async fn start(dir: &std::path::Path, fake: Option<Arc<FakeTunnel>>) -> Host {
    let cfg = HostConfig {
        data_dir: dir.into(),
        ..Default::default()
    };
    Host::start_with(cfg, "Test PC".into(), move |_, _, _| async move {
        Ok(fake.map(|f| f as Arc<dyn TunnelBackend>))
    })
    .await
    .unwrap()
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn read_json(ws: &mut Socket) -> Value {
    loop {
        let msg = timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// Pairs over the gateway like a phone would; returns (socket, device id).
async fn phone_pair(host: &Host, secret: &str) -> (Socket, String) {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/v1/ws", host.gateway))
        .await
        .unwrap();
    let pair = json!({"t":"pair","version":1,"secret":secret,"device_name":"Pixel"});
    ws.send(Message::Text(pair.to_string().into()))
        .await
        .unwrap();
    let paired = read_json(&mut ws).await;
    assert_eq!(paired["t"], "paired", "{paired}");
    let welcome = read_json(&mut ws).await;
    assert_eq!(welcome["t"], "welcome");
    assert_eq!(welcome["host_name"], "Test PC");
    (ws, paired["device_id"].as_str().unwrap().to_string())
}

async fn wait_for<F: Fn() -> bool>(f: F) {
    timeout(Duration::from_secs(10), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("condition not reached");
}

#[tokio::test]
async fn pair_links_phone_and_revoke_reaches_tunnel() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeTunnel::default());
    let host = start(dir.path(), Some(fake.clone())).await;
    assert_eq!(fake.calls().allowed, vec![Vec::<String>::new()]);

    let info = host.pair(Duration::from_secs(60)).await.unwrap();
    assert!(info.qr.starts_with(QR_PREFIX));
    assert!(info.expires_at > tether_proto::now_ms());
    let secret = fake.calls().payloads[0].clone();

    // The phone joins the tunnel, then pairs through it.
    fake.join("nodekey:aa");
    let (mut ws, device) = phone_pair(&host, &secret).await;
    wait_for(|| host.devices().unwrap()[0].phone_key.as_deref() == Some("nodekey:aa")).await;
    assert_eq!(host.status().connected_devices, 1);
    assert_eq!(host.status().tunnel_address.as_deref(), Some("100.64.0.1"));

    host.revoke(&device).await.unwrap();
    assert_eq!(fake.calls().revoked, vec!["nodekey:aa".to_string()]);
    assert!(host.devices().unwrap().is_empty());
    // The connection is closed at once.
    let closed = timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok());
    assert!(host.revoke(&device).await.is_err());

    host.shutdown().await.unwrap();
    assert!(fake.calls().closed);
}

#[tokio::test]
async fn device_before_join_still_links() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeTunnel::default());
    let host = start(dir.path(), Some(fake.clone())).await;
    host.pair(Duration::from_secs(60)).await.unwrap();
    let secret = fake.calls().payloads[0].clone();
    // The gateway can report the device before the tunnel reports the join.
    let (_ws, device) = phone_pair(&host, &secret).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(host.devices().unwrap()[0].phone_key, None);
    fake.join("nodekey:bb");
    wait_for(|| host.devices().unwrap()[0].phone_key.as_deref() == Some("nodekey:bb")).await;
    assert_eq!(host.devices().unwrap()[0].id, device);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_syncs_allowed_phones_and_keeps_devices() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeTunnel::default());
    let host = start(dir.path(), Some(fake.clone())).await;
    host.pair(Duration::from_secs(60)).await.unwrap();
    let secret = fake.calls().payloads[0].clone();
    fake.join("nodekey:cc");
    let (ws, _) = phone_pair(&host, &secret).await;
    wait_for(|| host.devices().unwrap()[0].phone_key.is_some()).await;
    drop(ws);
    host.shutdown().await.unwrap();

    let again = Arc::new(FakeTunnel::default());
    let host = start(dir.path(), Some(again.clone())).await;
    assert_eq!(again.calls().allowed, vec![vec!["nodekey:cc".to_string()]]);
    assert_eq!(host.devices().unwrap().len(), 1);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn unlinked_device_revoke_resyncs_tunnel() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeTunnel::default());
    let host = start(dir.path(), Some(fake.clone())).await;
    host.pair(Duration::from_secs(60)).await.unwrap();
    let secret = fake.calls().payloads[0].clone();
    let (_ws, device) = phone_pair(&host, &secret).await;
    host.revoke(&device).await.unwrap();
    {
        let calls = fake.calls();
        assert!(calls.revoked.is_empty());
        assert_eq!(calls.allowed.len(), 2);
        assert!(calls.allowed[1].is_empty());
    }
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn without_tunnel_and_basics() {
    let dir = tempfile::tempdir().unwrap();
    let host = start(dir.path(), None).await;
    assert!(host.pair(Duration::from_secs(60)).await.is_err());
    let status = host.status();
    assert_eq!(status.tunnel_address, None);
    assert!(status.gateway.ip().is_loopback());
    assert_eq!(status.connected_devices, 0);
    assert_eq!(status.sessions_working, 0);
    let agents = host.agents();
    assert!(agents.iter().any(|a| a.id == "codex"));
    assert!(agents.iter().any(|a| a.id == "shell" && !a.chat));
    assert!(dir.path().join(config::DATABASE_FILE).is_file());
    assert!(dir.path().join(config::AGENTS_FILE).is_file());
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn agents_toml_is_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(config::AGENTS_FILE),
        "[[agent]]\nid = \"mine\"\nname = \"Mine\"\ncommand = [\"tether-missing-xyz\"]\nmode = \"acp\"\n",
    )
    .unwrap();
    let host = start(dir.path(), None).await;
    let mine = host.agents().into_iter().find(|a| a.id == "mine").unwrap();
    assert_eq!(mine.name, "Mine");
    assert!(!mine.available);
    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn every_agent_can_be_pointed_at_its_program() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(config::AGENTS_FILE),
        "[[agent]]\nid = \"mine\"\nname = \"Mine\"\ncommand = [\"tether-missing-xyz\"]\nmode = \"acp\"\n",
    )
    .unwrap();
    let host = start(dir.path(), None).await;
    let listed: Vec<String> = host.locations().into_iter().map(|l| l.agent).collect();
    assert!(listed.contains(&"mine".to_string()), "{listed:?}");
    assert!(listed.contains(&"dsh".to_string()), "{listed:?}");
    assert!(!listed.contains(&"echo".to_string()), "{listed:?}");

    // A program with the right name that answers `--version`.
    let bin = tempfile::tempdir().unwrap();
    let program = if cfg!(windows) {
        let p = bin.path().join("tether-missing-xyz.cmd");
        std::fs::write(&p, "@echo off\r\necho mine 1.2.3\r\n").unwrap();
        p
    } else {
        let p = bin.path().join("tether-missing-xyz");
        std::fs::write(&p, "#!/bin/sh\necho mine 1.2.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    };
    let wrong = bin.path().join("other.cmd");
    std::fs::write(&wrong, "").unwrap();
    assert!(
        host.set_location("mine", &wrong.display().to_string())
            .await
            .is_err()
    );
    let checked = host
        .set_location("mine", &bin.path().display().to_string())
        .await
        .unwrap();
    assert_eq!(checked.program, program);
    assert_eq!(checked.version.as_deref(), Some("mine 1.2.3"));
    let mine = host.agents().into_iter().find(|a| a.id == "mine").unwrap();
    assert!(mine.available, "{mine:?}");
    assert_eq!(mine.terminal_command[0], program.display().to_string());
    let kept = host
        .locations()
        .into_iter()
        .find(|l| l.agent == "mine")
        .unwrap();
    assert_eq!(kept.program.as_deref(), Some(program.as_path()));
    host.shutdown().await.unwrap();

    // Kept across restarts, and forgotten on request.
    let host = start(dir.path(), None).await;
    assert!(
        host.agents()
            .into_iter()
            .find(|a| a.id == "mine")
            .unwrap()
            .available
    );
    host.clear_location("mine").unwrap();
    assert!(
        !host
            .agents()
            .into_iter()
            .find(|a| a.id == "mine")
            .unwrap()
            .available
    );
    host.shutdown().await.unwrap();
}

#[test]
fn data_dir_and_name() {
    let dir = default_data_dir();
    assert!(dir.ends_with("Tether") || dir.ends_with("tether"));
    let cfg = HostConfig::default();
    assert_eq!(host_name(&cfg, Some(" Desk ".into())), "Desk");
    let cfg = HostConfig {
        name: "Mine".into(),
        ..Default::default()
    };
    assert_eq!(host_name(&cfg, Some("Desk".into())), "Mine");
    assert!(!host_name(&HostConfig::default(), None).is_empty());
}

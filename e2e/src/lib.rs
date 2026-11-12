//! Helpers for the end-to-end tests: a phone made of `tailpocket-phone` (the
//! tunnel) and a raw WebSocket client speaking PROTOCOL.md.
//!
//! The tests themselves are in `tests/` and are `#[ignore]`: they reach
//! Tailcat's public relays. Run them by hand:
//!
//! ```sh
//! cargo test -p tether-e2e -- --ignored --nocapture
//! ```

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tether_proto::{ClientMsg, PROTOCOL_VERSION, ServerMsg};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

/// `vendor/tailpocket/bin/tailpocket-<kind>-<os>-<arch>`, or `TAILPOCKET_<KIND>`.
pub fn tailpocket(kind: &str) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os(format!("TAILPOCKET_{}", kind.to_uppercase())) {
        return Ok(path.into());
    }
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vendor/tailpocket/bin")
        .join(format!("tailpocket-{kind}-{os}-{arch}{ext}"));
    ensure!(
        path.is_file(),
        "missing {} (run scripts/fetch-vendor.sh)",
        path.display()
    );
    Ok(path)
}

fn quiet(path: &Path) -> Command {
    let mut command = Command::new(path);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    command
}

/// A phone in the tunnel: paired once, then forwarding a loopback port to
/// the computer's gateway.
pub struct PhoneTunnel {
    pub payload: String,
    pub forward: SocketAddr,
    link: PathBuf,
    child: Child,
}

impl PhoneTunnel {
    /// "Scans" `qr` with `tailpocket-phone pair`, then starts forwarding.
    pub async fn pair(qr: &str, dir: &Path) -> anyhow::Result<PhoneTunnel> {
        let phone = tailpocket("phone")?;
        let link = dir.join("phone-link.json");
        let mut pairing = quiet(&phone)
            .args(["pair", "-device", "Tether e2e phone", "-prefix"])
            .arg(tether_tunnel::QR_PREFIX)
            .arg("-key")
            .arg(dir.join("phone-key"))
            .arg("-out")
            .arg(&link)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("start tailpocket-phone pair")?;
        let mut stdin = pairing.stdin.take().context("no stdin")?;
        stdin.write_all(qr.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);
        let out = timeout(Duration::from_secs(150), pairing.wait_with_output())
            .await
            .context("tailpocket-phone pair timed out")??;
        ensure!(
            out.status.success(),
            "tailpocket-phone pair failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let saved: Value = serde_json::from_slice(&tokio::fs::read(&link).await?)?;
        let payload = saved["payload"]
            .as_str()
            .context("no payload in the phone link")?
            .to_string();
        let (child, forward) = Self::connect(&phone, &link).await?;
        Ok(PhoneTunnel {
            payload,
            forward,
            link,
            child,
        })
    }

    async fn connect(phone: &Path, link: &Path) -> anyhow::Result<(Child, SocketAddr)> {
        let mut child = quiet(phone)
            .arg("connect")
            .arg("-link")
            .arg(link)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("start tailpocket-phone connect")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let mut line = String::new();
        timeout(
            Duration::from_secs(20),
            BufReader::new(stdout).read_line(&mut line),
        )
        .await
        .context("tailpocket-phone did not announce its port")??;
        let forward: SocketAddr = line
            .trim()
            .strip_prefix("listening ")
            .with_context(|| format!("unexpected phone output: {line:?}"))?
            .parse()?;
        Ok((child, forward))
    }

    /// A fresh forwarder with the same link (a phone app restart).
    pub async fn reconnect(&mut self) -> anyhow::Result<()> {
        let _ = self.child.kill().await;
        let (child, forward) = Self::connect(&tailpocket("phone")?, &self.link).await?;
        self.child = child;
        self.forward = forward;
        Ok(())
    }

    pub fn url(&self) -> String {
        format!("ws://{}/v1/ws", self.forward)
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Frame {
    Json(ServerMsg),
    Binary(Vec<u8>),
}

/// A WebSocket client for the gateway, with an inbox for pushes.
pub struct Client {
    socket: Socket,
    pub inbox: VecDeque<Frame>,
    next_req: u64,
    pub token: String,
    pub device_id: String,
}

pub const WAIT: Duration = Duration::from_secs(60);

impl Client {
    pub async fn connect(url: &str) -> anyhow::Result<Client> {
        // The first dial through a fresh tunnel can take a while.
        let (socket, _) = timeout(WAIT, tokio_tungstenite::connect_async(url))
            .await
            .context("WebSocket connect timed out")??;
        Ok(Client {
            socket,
            inbox: VecDeque::new(),
            next_req: 0,
            token: String::new(),
            device_id: String::new(),
        })
    }

    pub async fn pair(url: &str, secret: &str) -> anyhow::Result<Client> {
        let mut c = Client::connect(url).await?;
        c.send(&ClientMsg::Pair {
            version: PROTOCOL_VERSION,
            secret: secret.into(),
            device_name: "Tether e2e".into(),
        })
        .await?;
        match c.read().await? {
            Frame::Json(ServerMsg::Paired { device_id, token }) => {
                c.token = token;
                c.device_id = device_id;
            }
            other => bail!("expected paired, got {other:?}"),
        }
        c.welcome().await?;
        Ok(c)
    }

    pub async fn hello(url: &str, token: &str) -> anyhow::Result<Client> {
        let mut c = Client::connect(url).await?;
        c.send(&ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            token: token.into(),
            device_name: "Tether e2e".into(),
        })
        .await?;
        c.token = token.into();
        c.welcome().await?;
        Ok(c)
    }

    async fn welcome(&mut self) -> anyhow::Result<()> {
        match self.read().await? {
            Frame::Json(ServerMsg::Welcome { device_id, .. }) => {
                self.device_id = device_id;
                Ok(())
            }
            other => bail!("expected welcome, got {other:?}"),
        }
    }

    pub async fn send(&mut self, msg: &ClientMsg) -> anyhow::Result<()> {
        let text = serde_json::to_string(msg)?;
        self.socket.send(Message::Text(text.into())).await?;
        Ok(())
    }

    pub async fn send_binary(&mut self, bytes: Vec<u8>) -> anyhow::Result<()> {
        self.socket.send(Message::Binary(bytes.into())).await?;
        Ok(())
    }

    /// The next frame off the wire (not the inbox).
    pub async fn read(&mut self) -> anyhow::Result<Frame> {
        loop {
            let msg = timeout(WAIT, self.socket.next())
                .await
                .context("no frame from the gateway")?
                .context("connection closed")??;
            match msg {
                Message::Text(t) => return Ok(Frame::Json(serde_json::from_str(&t)?)),
                Message::Binary(b) => return Ok(Frame::Binary(b.to_vec())),
                Message::Close(c) => bail!("closed: {c:?}"),
                _ => {}
            }
        }
    }

    pub fn req(&mut self) -> u64 {
        self.next_req += 1;
        self.next_req
    }

    /// Sends a request built from a fresh `req` and returns its `ok` data.
    pub async fn call(&mut self, build: impl FnOnce(u64) -> ClientMsg) -> anyhow::Result<Value> {
        let req = self.req();
        self.send(&build(req)).await?;
        loop {
            match self.read().await? {
                Frame::Json(ServerMsg::Ok { req: r, data }) if r == req => return Ok(data),
                Frame::Json(ServerMsg::Error {
                    req: Some(r),
                    code,
                    message,
                }) if r == req => bail!("{code:?}: {message}"),
                other => self.inbox.push_back(other),
            }
        }
    }

    /// Waits (inbox first) for a frame `pick` accepts, within `limit`.
    pub async fn wait<T>(
        &mut self,
        limit: Duration,
        mut pick: impl FnMut(&Frame) -> Option<T>,
    ) -> anyhow::Result<T> {
        if let Some(i) = self.inbox.iter().position(|f| pick(f).is_some()) {
            let frame = self.inbox.remove(i).context("inbox")?;
            return pick(&frame).context("pick");
        }
        timeout(limit, async {
            loop {
                let frame = self.read().await?;
                if let Some(t) = pick(&frame) {
                    return Ok(t);
                }
                self.inbox.push_back(frame);
            }
        })
        .await
        .context("timed out waiting for a frame")?
    }

    /// True once the server closes this connection.
    pub async fn closed_within(&mut self, limit: Duration) -> bool {
        timeout(limit, async {
            loop {
                match self.socket.next().await {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await
        .is_ok()
    }
}

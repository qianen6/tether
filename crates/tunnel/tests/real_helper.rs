//! Opt in only: this test reaches Tailcat's public relays. Never run it as part
//! of offline acceptance. Binaries can be overridden with TAILPOCKET_HOST/PHONE.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use tether_tunnel::{QR_PREFIX, Tunnel, TunnelConfig};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

const PROBE: &[u8] = b"Tether T01 echo\0\xff\n";

fn binary(kind: &str) -> Result<PathBuf> {
    let override_name = format!("TAILPOCKET_{}", kind.to_uppercase());
    if let Some(path) = std::env::var_os(override_name) {
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
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/tailpocket/bin")
        .join(format!("tailpocket-{kind}-{os}-{arch}{suffix}"));
    ensure!(path.is_file(), "missing offline helper: {}", path.display());
    Ok(path)
}

fn phone_command(path: &std::path::Path) -> Command {
    let mut command = Command::new(path);
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    command
}

struct Echo {
    task: JoinHandle<()>,
    address: SocketAddr,
    accepts: Arc<AtomicUsize>,
}

impl Echo {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let accepts = Arc::new(AtomicUsize::new(0));
        let count = accepts.clone();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((mut socket, _)) => {
                            count.fetch_add(1, Ordering::SeqCst);
                            clients.spawn(async move {
                                let (mut read, mut write) = socket.split();
                                let _ = tokio::io::copy(&mut read, &mut write).await;
                                let _ = write.shutdown().await;
                            });
                        }
                        Err(_) => return,
                    },
                    _ = clients.join_next(), if !clients.is_empty() => {}
                }
            }
        });
        Ok(Self {
            task,
            address,
            accepts,
        })
    }
}

impl Drop for Echo {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct PhoneForward {
    child: Child,
    address: SocketAddr,
}

impl PhoneForward {
    async fn start(phone: &std::path::Path, link: &std::path::Path) -> Result<Self> {
        // Inherit stderr so a failed Windows live run keeps the complete helper
        // diagnostics in --nocapture output; stdout only announces the listener.
        let mut child = phone_command(phone)
            .arg("connect")
            .arg("-link")
            .arg(link)
            .stderr(Stdio::inherit())
            .spawn()
            .context("start phone forwarder")?;
        let stdout = child.stdout.take().context("phone stdout missing")?;
        let mut line = String::new();
        timeout(
            Duration::from_secs(10),
            BufReader::new(stdout).read_line(&mut line),
        )
        .await
        .context("phone did not announce its loopback port")??;
        let address: SocketAddr = line
            .trim()
            .strip_prefix("listening ")
            .context("phone did not print 'listening ADDRESS'")?
            .parse()?;
        ensure!(
            address.ip().is_loopback(),
            "phone forwarder must be loopback-only"
        );
        Ok(Self { child, address })
    }

    async fn stop(mut self) -> Result<()> {
        self.child.kill().await.context("stop phone forwarder")
    }

    async fn assert_refused(&self) -> Result<()> {
        let mut socket = TcpStream::connect(self.address).await?;
        // A local TCP accept is NOT proof of a successful tunnel connection.
        // Require the failed 20-second remote dial to close/reset this socket.
        let _ = socket.write_all(PROBE).await;
        let mut reply = [0; 1];
        match timeout(Duration::from_secs(30), socket.read(&mut reply)).await {
            Ok(Ok(0)) | Ok(Err(_)) => Ok(()),
            Ok(Ok(_)) => bail!("revoked phone still received tunnel data"),
            Err(_) => bail!("revoked dial did not close within 30 seconds"),
        }
    }
}

async fn exercise(
    tunnel: &Tunnel,
    phone: PathBuf,
    dir: &std::path::Path,
    echo: &Echo,
) -> Result<()> {
    let started = Instant::now();
    let window = tunnel
        .pair(Duration::from_secs(180), "t01-payload".into())
        .await?;
    let link = dir.join("phone-link.json");
    let mut pairing = phone_command(&phone)
        .arg("pair")
        .arg("-device")
        .arg("Tether T01 phone")
        .arg("-prefix")
        .arg(QR_PREFIX)
        .arg("-key")
        .arg(dir.join("phone-key"))
        .arg("-out")
        .arg(&link)
        .arg("-")
        .stdin(Stdio::piped())
        .spawn()?;
    let mut stdin = pairing.stdin.take().context("phone stdin missing")?;
    stdin.write_all(window.qr.as_bytes()).await?;
    stdin.shutdown().await?;
    drop(stdin);
    let output = timeout(Duration::from_secs(150), pairing.wait_with_output())
        .await
        .context("phone pairing timed out")??;
    ensure!(
        output.status.success(),
        "phone pairing failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let joined = timeout(Duration::from_secs(10), window.joined)
        .await
        .context("host did not report paired")??;
    eprintln!("配对耗时：{:?}", started.elapsed());
    ensure!(
        joined.device_name == "Tether T01 phone",
        "wrong joined device name"
    );
    let saved_link: Value = serde_json::from_slice(&tokio::fs::read(&link).await?)?;
    ensure!(
        saved_link["payload"] == "t01-payload",
        "pair payload was changed"
    );
    ensure!(
        saved_link["address"].as_str() == tunnel.address().as_deref(),
        "host/link address mismatch"
    );
    let saved_state: Value =
        serde_json::from_slice(&tokio::fs::read(dir.join("host-state.json")).await?)?;
    ensure!(
        saved_state["allowed"]
            .as_array()
            .is_some_and(|a| a.iter().any(|key| key.as_str() == Some(&joined.phone_key))),
        "paired phone was not persisted"
    );

    let forward = PhoneForward::start(&phone, &link).await?;
    let mut socket = TcpStream::connect(forward.address).await?;
    timeout(Duration::from_secs(60), async {
        socket.write_all(PROBE).await?;
        let mut echoed = vec![0; PROBE.len()];
        socket.read_exact(&mut echoed).await?;
        ensure!(echoed == PROBE, "tunnel echo differs");
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("initial tunnel echo timed out")??;
    eprintln!("二进制回显：通过；路径：未观测（上游 IPC 不提供直连/中继遥测，请人工记录）");

    let revoked = Instant::now();
    tunnel.revoke(&joined.phone_key).await?;
    let mut byte = [0; 1];
    match timeout(Duration::from_secs(5), socket.read(&mut byte)).await {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(_)) => bail!("existing revoked connection still carries data"),
        Err(_) => bail!("existing revoked connection did not disconnect within 5 seconds"),
    }
    ensure!(
        revoked.elapsed() <= Duration::from_secs(5),
        "revoke was not immediate: {:?}",
        revoked.elapsed()
    );
    eprintln!("吊销至原连接断开：{:?}", revoked.elapsed());
    ensure!(
        tunnel.address().is_none(),
        "last phone revoked but main node still has an address"
    );
    let accepts = echo.accepts.load(Ordering::SeqCst);
    forward
        .assert_refused()
        .await
        .context("existing phone client reconnected after revoke")?;
    forward.stop().await?;
    let fresh = PhoneForward::start(&phone, &link).await?;
    fresh
        .assert_refused()
        .await
        .context("fresh phone client reconnected after revoke")?;
    fresh.stop().await?;
    ensure!(
        echo.accepts.load(Ordering::SeqCst) == accepts,
        "revoked phone reached the gateway"
    );
    let saved_state: Value =
        serde_json::from_slice(&tokio::fs::read(dir.join("host-state.json")).await?)?;
    ensure!(
        saved_state["allowed"] == serde_json::json!([]),
        "revocation was not persisted"
    );
    eprintln!("吊销：原连接已断开；原客户端及新客户端均未能重连；网关未接受新连接");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires real TailPocket binaries and outbound Tailcat relay access; run manually"]
async fn pair_echo_and_revoke() -> Result<()> {
    let host = binary("host")?;
    let phone = binary("phone")?;
    let dir = tempfile::tempdir()?;
    let echo = Echo::start().await?;
    let tunnel = Tunnel::start(TunnelConfig {
        helper: host,
        forward: echo.address,
        state_path: dir.path().join("host-state.json"),
        name: "Tether T01 host".into(),
        derp_map_url: None,
        qr_prefix: QR_PREFIX.into(),
    })
    .await?;
    let result = exercise(&tunnel, phone, dir.path(), &echo).await;
    let closed = tunnel.close().await;
    if let Err(error) = &result {
        eprintln!("真实隧道测试完整错误：{error:#}");
    }
    result?;
    closed
}

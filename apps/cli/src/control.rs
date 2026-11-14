//! The local control endpoint of a running `tether serve`.
//!
//! `serve` listens on a random 127.0.0.1 port and writes the port and a random
//! token to `control.json` in the data directory (readable by the user only).
//! Every connection carries one JSON line each way:
//!
//! ```text
//! → {"token":"…","cmd":"pair","minutes":10}
//! ← {"ok":true,"data":{"qr":"tether1:…","expires_at":1700000000000}}
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tether_host::Host;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

pub const FILE: &str = "control.json";
/// Requests are tiny; anything longer is not ours.
const MAX_LINE: u64 = 64 << 10;

#[derive(Debug, Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
    pid: u32,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Pair {
        minutes: u64,
    },
    Devices,
    Revoke {
        device_id: String,
    },
    /// The agents, whether each can run, and the program chosen for it.
    Agents,
    /// Checks and keeps `path` as `agent`'s program (for DSH also the
    /// address of a running `dsh web`); `None` forgets it.
    Locate {
        agent: String,
        path: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    token: String,
    #[serde(flatten)]
    request: Request,
}

/// What `serve` does with a request; [`Host`] in production.
pub trait Handler: Send + Sync + 'static {
    fn handle(
        &self,
        request: Request,
    ) -> impl std::future::Future<Output = anyhow::Result<Value>> + Send;
}

impl Handler for Host {
    async fn handle(&self, request: Request) -> anyhow::Result<Value> {
        match request {
            Request::Pair { minutes } => {
                let info = self
                    .pair(Duration::from_secs(minutes.clamp(1, 15) * 60))
                    .await?;
                Ok(json!({ "qr": info.qr, "expires_at": info.expires_at }))
            }
            Request::Devices => {
                let devices: Vec<Value> = self
                    .devices()?
                    .into_iter()
                    .map(|d| {
                        json!({
                            "id": d.id,
                            "name": d.name,
                            "linked": d.phone_key.is_some(),
                            "paired_at": d.paired_at,
                            "last_seen": d.last_seen,
                        })
                    })
                    .collect();
                Ok(Value::Array(devices))
            }
            Request::Revoke { device_id } => {
                self.revoke(&device_id).await?;
                Ok(Value::Null)
            }
            Request::Agents => {
                let locations = self.locations();
                let agents: Vec<Value> = self
                    .agents()
                    .into_iter()
                    .map(|a| {
                        let location = locations.iter().find(|l| l.agent == a.id);
                        json!({
                            "id": a.id,
                            "name": a.name,
                            "available": a.available,
                            "detail": a.detail,
                            "locatable": location.is_some(),
                            "location": location
                                .and_then(|l| l.program.as_ref())
                                .map(|p| p.display().to_string()),
                        })
                    })
                    .collect();
                Ok(Value::Array(agents))
            }
            Request::Locate { agent, path } => match path {
                Some(path) => {
                    let checked = self.set_location(&agent, &path).await?;
                    Ok(json!({
                        "program": checked.program.display().to_string(),
                        "version": checked.version,
                    }))
                }
                None => {
                    self.clear_location(&agent)?;
                    Ok(Value::Null)
                }
            },
        }
    }
}

pub struct Control {
    path: PathBuf,
    task: JoinHandle<()>,
}

impl Control {
    pub async fn start<H: Handler>(data_dir: &Path, handler: Arc<H>) -> anyhow::Result<Control> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let token = hex::encode(rand::random::<[u8; 32]>());
        let endpoint = Endpoint {
            port: listener.local_addr()?.port(),
            token: token.clone(),
            pid: std::process::id(),
        };
        let path = data_dir.join(FILE);
        write_private(&path, &serde_json::to_vec(&endpoint)?)?;
        let token = Arc::new(token);
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    continue;
                };
                let (handler, token) = (handler.clone(), token.clone());
                tokio::spawn(async move {
                    if let Err(error) = serve_one(socket, &token, &*handler).await {
                        tracing::debug!(%error, "control request failed");
                    }
                });
            }
        });
        Ok(Control { path, task })
    }

    pub fn stop(self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let mut file = options
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    file.write_all(bytes)?;
    Ok(())
}

async fn serve_one<H: Handler>(socket: TcpStream, token: &str, handler: &H) -> anyhow::Result<()> {
    let (read, mut write) = socket.into_split();
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        BufReader::new(read).take(MAX_LINE).read_line(&mut line),
    )
    .await??;
    let reply = match serde_json::from_str::<Envelope>(&line) {
        Ok(e) if bool::from(e.token.as_bytes().ct_eq(token.as_bytes())) => {
            match handler.handle(e.request).await {
                Ok(data) => json!({ "ok": true, "data": data }),
                Err(error) => json!({ "ok": false, "error": format!("{error:#}") }),
            }
        }
        Ok(_) => json!({ "ok": false, "error": "wrong control token" }),
        Err(error) => json!({ "ok": false, "error": format!("bad request: {error}") }),
    };
    let mut out = serde_json::to_vec(&reply)?;
    out.push(b'\n');
    write.write_all(&out).await?;
    write.shutdown().await?;
    Ok(())
}

/// Sends one request to the `serve` that owns `data_dir`.
pub async fn call(data_dir: &Path, request: Request) -> anyhow::Result<Value> {
    let path = data_dir.join(FILE);
    let text = match std::fs::read(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("tether serve is not running (no {})", path.display())
        }
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let endpoint: Endpoint =
        serde_json::from_slice(&text).with_context(|| format!("invalid {}", path.display()))?;
    let mut socket = TcpStream::connect(("127.0.0.1", endpoint.port))
        .await
        .context("tether serve is not running (cannot reach its control port)")?;
    let mut line = serde_json::to_vec(&Envelope {
        token: endpoint.token,
        request,
    })?;
    line.push(b'\n');
    socket.write_all(&line).await?;
    let mut reply = String::new();
    // Pairing waits for the tunnel, which may take a while.
    tokio::time::timeout(
        Duration::from_secs(60),
        BufReader::new(socket).take(MAX_LINE).read_line(&mut reply),
    )
    .await
    .context("tether serve did not answer")??;
    let reply: Value = serde_json::from_str(&reply).context("bad reply from tether serve")?;
    if reply["ok"] == true {
        Ok(reply["data"].clone())
    } else {
        bail!("{}", reply["error"].as_str().unwrap_or("unknown error"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        seen: Mutex<Vec<Request>>,
    }

    impl Handler for Fake {
        async fn handle(&self, request: Request) -> anyhow::Result<Value> {
            self.seen.lock().unwrap().push(request);
            let seen = self.seen.lock().unwrap();
            let reply = match seen.last().unwrap() {
                Request::Pair { minutes } => json!({ "qr": "tether1:x", "expires_at": minutes }),
                Request::Devices => json!([]),
                Request::Revoke { device_id } if device_id == "nope" => bail!("no device nope"),
                Request::Revoke { .. } => Value::Null,
                Request::Agents => json!([]),
                Request::Locate { .. } => Value::Null,
            };
            Ok(reply)
        }
    }

    #[tokio::test]
    async fn round_trip_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            call(dir.path(), Request::Devices)
                .await
                .unwrap_err()
                .to_string()
                .contains("not running")
        );
        let fake = Arc::new(Fake::default());
        let control = Control::start(dir.path(), fake.clone()).await.unwrap();
        let pair = call(dir.path(), Request::Pair { minutes: 5 })
            .await
            .unwrap();
        assert_eq!(pair["qr"], "tether1:x");
        assert_eq!(pair["expires_at"], 5);
        assert_eq!(call(dir.path(), Request::Devices).await.unwrap(), json!([]));
        let err = call(
            dir.path(),
            Request::Revoke {
                device_id: "nope".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no device nope"));
        assert_eq!(fake.seen.lock().unwrap().len(), 3);

        // A wrong token is refused before reaching the handler.
        let mut endpoint: Endpoint =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE)).unwrap()).unwrap();
        endpoint.token = "0".repeat(64);
        std::fs::write(
            dir.path().join(FILE),
            serde_json::to_vec(&endpoint).unwrap(),
        )
        .unwrap();
        let err = call(dir.path(), Request::Devices).await.unwrap_err();
        assert!(err.to_string().contains("wrong control token"));
        assert_eq!(fake.seen.lock().unwrap().len(), 3);

        control.stop();
        assert!(!dir.path().join(FILE).exists());
    }
}

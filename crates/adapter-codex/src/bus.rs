//! The Codex app's IPC bus: `\\.\pipe\codex-ipc` on Windows, a Unix socket
//! elsewhere. A router inside the app passes length-prefixed JSON frames
//! (u32 little-endian byte count, then UTF-8) between its clients: each
//! `initialize`s for an id, then sends requests (routed to a client that says
//! it can handle them) and broadcasts.

use std::path::PathBuf;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
};

/// The router refuses bigger frames (`0x10000000`); a thread snapshot is far smaller.
pub(crate) const MAX_FRAME: usize = 64 << 20;

/// What a request is answered with by every client before `initialize`.
pub(crate) const UNINITIALIZED: &str = "initializing-client";

/// Message versions the app checks (`thread-follower-*`) or stamps (broadcasts).
pub(crate) mod version {
    pub(crate) const INITIALIZE: u32 = 0;
    pub(crate) const STATE_CHANGED: u32 = 11;
    pub(crate) const FOLLOWING_CHANGED: u32 = 1;
    pub(crate) const START_TURN: u32 = 2;
    pub(crate) const INTERRUPT_TURN: u32 = 4;
    pub(crate) const READ_MODEL_SETTINGS: u32 = 1;
    pub(crate) const APPROVAL_DECISION: u32 = 1;
    pub(crate) const UNARCHIVED: u32 = 1;
}

pub(crate) type Reader = Box<dyn AsyncRead + Unpin + Send>;
pub(crate) type Writer = Box<dyn AsyncWrite + Unpin + Send>;

/// The app is not running (or not this user's).
#[derive(Debug)]
pub(crate) struct Closed;

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(CLOSED_TEXT)
    }
}

impl std::error::Error for Closed {}

/// The app went away while a conversation was followed.
pub(crate) const CLOSED_TEXT: &str =
    "电脑上的 Codex 桌面版关闭了。请在电脑上重新打开 Codex，再发送。";

/// The app is not running when a conversation starts.
pub(crate) const NOT_OPEN_TEXT: &str =
    "电脑上的 Codex 桌面版没有打开。请先在电脑上打开 Codex，再发送。";

/// The app is running but does not answer on its bus.
pub(crate) const NO_ANSWER_TEXT: &str =
    "电脑上的 Codex 桌面版没有回应。请确认它已正常打开，再发送。";

/// The app answers, but not in the way this Tether knows (it was updated).
#[derive(Debug)]
pub(crate) struct Updated;

impl std::fmt::Display for Updated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(UPDATED_TEXT)
    }
}

impl std::error::Error for Updated {}

/// The app speaks a newer bus version. The phone offers a fork on "分叉成新对话".
pub(crate) const UPDATED_TEXT: &str = "电脑上的 Codex 桌面版已更新，Tether 暂时没法接管它正开着的这个对话。可以在电脑上继续，或者分叉成新对话在手机上继续。";

pub(crate) fn encode(message: &Value) -> anyhow::Result<Vec<u8>> {
    let body = serde_json::to_vec(message)?;
    anyhow::ensure!(body.len() <= MAX_FRAME, "IPC frame too large");
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&u32::try_from(body.len())?.to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

pub(crate) async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Value,
) -> anyhow::Result<()> {
    writer.write_all(&encode(message)?).await?;
    writer.flush().await?;
    Ok(())
}

/// The next frame; `None` when the app closed the connection between frames.
pub(crate) async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<Option<Value>> {
    let mut head = [0u8; 4];
    match reader.read_exact(&mut head).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(head) as usize;
    if len > MAX_FRAME {
        bail!("IPC frame of {len} bytes is too large");
    }
    let mut body = vec![0; len];
    reader
        .read_exact(&mut body)
        .await
        .context("IPC frame cut short")?;
    Ok(Some(
        serde_json::from_slice(&body).context("IPC frame is not JSON")?,
    ))
}

/// Frames as they come, read by a task of their own so a `select!` never
/// loses half of one.
pub(crate) fn frames<R: AsyncRead + Unpin + Send + 'static>(
    mut reader: R,
) -> mpsc::Receiver<anyhow::Result<Value>> {
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                frame = read(&mut reader) => frame,
                _ = tx.closed() => break,
            };
            let end = !matches!(frame, Ok(Some(_)));
            let sent = match frame {
                Ok(Some(message)) => tx.send(Ok(message)).await,
                Ok(None) => break,
                Err(e) => tx.send(Err(e)).await,
            };
            if sent.is_err() || end {
                break;
            }
        }
    });
    rx
}

pub(crate) fn request(
    request_id: &str,
    client: &str,
    method: &str,
    version: u32,
    params: Value,
    timeout_ms: u64,
) -> Value {
    json!({"type":"request","requestId":request_id,"sourceClientId":client,"version":version,
           "method":method,"params":params,"timeoutMs":timeout_ms})
}

pub(crate) fn broadcast(client: &str, method: &str, version: u32, params: Value) -> Value {
    json!({"type":"broadcast","method":method,"sourceClientId":client,"version":version,"params":params})
}

/// The router asks every client whether it can take a request: we take none.
pub(crate) fn decline(discovery: &Value) -> Value {
    json!({"type":"client-discovery-response","requestId":discovery["requestId"],"response":{"canHandle":false}})
}

/// Connects to the running app's router.
pub(crate) async fn connect() -> anyhow::Result<(Reader, Writer)> {
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        const PIPE_BUSY: i32 = 231;
        let mut tries = 0;
        let pipe = loop {
            match ClientOptions::new().open(r"\\.\pipe\codex-ipc") {
                Ok(pipe) => break pipe,
                Err(e) if e.raw_os_error() == Some(PIPE_BUSY) && tries < 20 => {
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Closed.into()),
                Err(e) => return Err(anyhow::Error::from(e).context(Closed)),
            }
        };
        let (reader, writer) = tokio::io::split(pipe);
        Ok((Box::new(reader), Box::new(writer)))
    }
    #[cfg(unix)]
    {
        for path in socket_paths() {
            if let Ok(stream) = tokio::net::UnixStream::connect(&path).await {
                let (reader, writer) = stream.into_split();
                return Ok((Box::new(reader), Box::new(writer)));
            }
        }
        Err(Closed.into())
    }
}

/// Where the app may listen on macOS and Linux (unverified there): under the
/// Codex home, or the older per-user socket in the temp folder.
#[cfg(unix)]
fn socket_paths() -> Vec<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let mut paths = Vec::new();
    if let Some(home) = crate::desktop::codex_home() {
        paths.push(home.join("ipc").join("ipc.sock"));
    }
    let tmp = std::env::temp_dir().join("codex-ipc");
    if let Some(uid) = std::env::var_os("HOME")
        .and_then(|h| std::fs::metadata(h).ok())
        .map(|m| m.uid())
    {
        paths.push(tmp.join(format!("ipc-{uid}.sock")));
    }
    paths.push(tmp.join("ipc.sock"));
    paths
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn socket_paths() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip_with_a_length_prefix() {
        let (mut a, b) = tokio::io::duplex(1024);
        write(&mut a, &json!({"type":"broadcast","text":"中文"}))
            .await
            .unwrap();
        write(&mut a, &json!({"n":2})).await.unwrap();
        drop(a);
        let mut frames = frames(b);
        assert_eq!(frames.recv().await.unwrap().unwrap()["text"], "中文");
        assert_eq!(frames.recv().await.unwrap().unwrap()["n"], 2);
        assert!(frames.recv().await.is_none());
        let bytes = encode(&json!({})).unwrap();
        assert_eq!(&bytes[..4], &2u32.to_le_bytes());
    }

    #[tokio::test]
    async fn oversized_and_cut_frames_are_errors() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(MAX_FRAME as u32 + 1).to_le_bytes())
            .await
            .unwrap();
        assert!(read(&mut b).await.is_err());
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&10u32.to_le_bytes()).await.unwrap();
        a.write_all(b"{}").await.unwrap();
        drop(a);
        assert!(read(&mut b).await.is_err());
    }

    #[test]
    fn messages_carry_the_routing_fields() {
        let r = request(
            "r1",
            "c1",
            "thread-follower-start-turn",
            2,
            json!({"a":1}),
            500,
        );
        assert_eq!(r["type"], "request");
        assert_eq!(r["version"], 2);
        assert_eq!(r["timeoutMs"], 500);
        let b = broadcast("c1", "thread-stream-following-changed", 1, json!({}));
        assert_eq!(b["sourceClientId"], "c1");
        assert_eq!(
            decline(&json!({"requestId":"d"}))["response"]["canHandle"],
            false
        );
    }
}

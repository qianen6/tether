//! Line framing and the helper's wire vocabulary; no process or filesystem I/O.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::{TunnelConfig, state::State};

pub(crate) const MAX_LINE: usize = 1 << 20; // upstream bufio.Scanner limit
pub(crate) type Reader = Box<dyn AsyncRead + Unpin + Send>;
pub(crate) type Writer = Box<dyn AsyncWrite + Unpin + Send>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config<'a> {
    v: u8,
    name: &'a str,
    identity: &'a str,
    allowed: &'a [String],
    forward: String,
    derp_map_url: &'a str,
    identify_phone: bool,
}

impl<'a> Config<'a> {
    pub fn new(cfg: &'a TunnelConfig, state: &'a State) -> Self {
        Self {
            v: 1,
            name: &cfg.name,
            identity: &state.identity,
            allowed: &state.allowed,
            forward: cfg.forward.to_string(),
            derp_map_url: cfg.derp_map_url.as_deref().unwrap_or(""),
            identify_phone: false,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "cmd")]
pub(crate) enum Command<'a> {
    #[serde(rename = "pair")]
    Pair {
        id: &'a str,
        ttl: u64,
        payload: &'a str,
        #[serde(rename = "qrPrefix")]
        qr_prefix: &'a str,
    },
    #[serde(rename = "revoke")]
    Revoke { id: &'a str, phone: &'a str },
    #[serde(rename = "setAllowed")]
    SetAllowed { id: &'a str, allowed: &'a [String] },
    #[serde(rename = "saved")]
    Saved { id: &'a str, ok: bool },
    #[serde(rename = "cancel")]
    Cancel { id: &'a str },
}

#[derive(Deserialize)]
#[serde(tag = "event")]
pub(crate) enum Event {
    #[serde(rename = "ready")]
    Ready { address: String },
    #[serde(rename = "save")]
    Save { id: String, state: State },
    #[serde(rename = "pairing")]
    Pairing {
        id: String,
        qr: String,
        #[serde(rename = "expiresAt")]
        expires_at: u64,
    },
    #[serde(rename = "paired")]
    Paired {
        id: String,
        phone: String,
        device: String,
        address: String,
    },
    #[serde(rename = "pairingClosed")]
    PairingClosed { id: String, reason: String },
    #[serde(rename = "allowed")]
    Allowed {
        id: String,
        allowed: Vec<String>,
        address: String,
    },
    #[serde(rename = "error")]
    Error { id: Option<String>, code: String },
    #[serde(other)]
    Unknown,
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value).context("encode helper command")?;
    bytes.push(b'\n');
    ensure!(bytes.len() < MAX_LINE, "helper command exceeds 1 MiB");
    Ok(bytes)
}

struct Frames<R> {
    reader: BufReader<R>,
    // Retained across cancelled next() futures (select!), including partial UTF-8.
    partial: Vec<u8>,
}

impl<R: AsyncRead + Unpin> Frames<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            partial: Vec::new(),
        }
    }

    async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            let bytes = self.reader.fill_buf().await.context("read helper stdout")?;
            if bytes.is_empty() {
                ensure!(self.partial.is_empty(), "helper stdout ended mid-frame");
                return Ok(None);
            }
            let end = bytes.iter().position(|&b| b == b'\n');
            let count = end.map_or(bytes.len(), |i| i + 1);
            ensure!(
                self.partial.len() + count < MAX_LINE,
                "helper event exceeds 1 MiB"
            );
            self.partial.extend_from_slice(&bytes[..count]);
            self.reader.consume(count);
            if end.is_some() {
                let event = serde_json::from_slice(&self.partial).context("decode helper event")?;
                self.partial.clear();
                return Ok(Some(event));
            }
        }
    }
}

pub(crate) struct Connection {
    frames: Frames<Reader>,
    outgoing: mpsc::Sender<Vec<u8>>,
    writer_done: oneshot::Receiver<Result<()>>,
    writer: Option<JoinHandle<()>>,
}

impl Connection {
    pub fn new(reader: Reader, mut writer: Writer) -> Self {
        let (outgoing, mut queue) = mpsc::channel::<Vec<u8>>(128);
        let (done, writer_done) = oneshot::channel();
        let writer = tokio::spawn(async move {
            let result = async {
                while let Some(frame) = queue.recv().await {
                    tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        writer.write_all(&frame).await?;
                        writer.flush().await
                    })
                    .await
                    .context("helper stdin write timed out")?
                    .context("write helper stdin")?;
                }
                writer.shutdown().await.context("close helper stdin")
            }
            .await;
            let _ = done.send(result);
        });
        Self {
            frames: Frames::new(reader),
            outgoing,
            writer_done,
            writer: Some(writer),
        }
    }

    pub fn send(&self, value: &impl Serialize) -> Result<()> {
        self.outgoing
            .try_send(encode(value)?)
            .context("helper stdin queue is full or closed")
    }

    pub async fn next(&mut self) -> Result<Option<Event>> {
        tokio::select! {
            event = self.frames.next() => event,
            done = &mut self.writer_done => {
                done.context("helper stdin writer stopped")??;
                bail!("helper stdin closed unexpectedly")
            }
        }
    }

    pub async fn stop(mut self) {
        // Await cancellation so ChildStdin has actually been dropped before the
        // supervisor starts its five-second child-exit timer.
        if let Some(writer) = self.writer.take() {
            writer.abort();
            let _ = writer.await;
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(writer) = &self.writer {
            writer.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn exact_command_names_units_and_escaping() {
        let frame = encode(&Command::Pair {
            id: "t1",
            ttl: 60,
            payload: "密钥\n\"x\"",
            qr_prefix: "tether1:",
        })
        .unwrap();
        assert_eq!(frame.iter().filter(|&&b| b == b'\n').count(), 1);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&frame).unwrap(),
            json!({"cmd":"pair","id":"t1","ttl":60,"payload":"密钥\n\"x\"","qrPrefix":"tether1:"})
        );
        assert!(encode(&json!({"payload": "x".repeat(MAX_LINE)})).is_err());
    }

    #[tokio::test]
    async fn partial_utf8_survives_cancellation_and_crlf() {
        let (a, mut b) = tokio::io::duplex(64);
        let mut frames = Frames::new(a);
        let bytes = "{\"event\":\"ready\",\"address\":\"手机\"}\r\n".as_bytes();
        let split = bytes.iter().position(|&b| b >= 128).unwrap() + 1;
        b.write_all(&bytes[..split]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), frames.next())
                .await
                .is_err()
        );
        b.write_all(&bytes[split..]).await.unwrap();
        assert!(
            matches!(frames.next().await.unwrap(), Some(Event::Ready { address }) if address == "手机")
        );
        b.shutdown().await.unwrap();
        assert!(frames.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_malformed_truncated_and_oversized_frames() {
        for input in [
            b"not json\n".to_vec(),
            b"{\"event\":\"ready\"}\n".to_vec(),
            b"{\"event\":".to_vec(),
            vec![b'x'; MAX_LINE],
        ] {
            let (a, mut b) = tokio::io::duplex(MAX_LINE + 1);
            b.write_all(&input).await.unwrap();
            b.shutdown().await.unwrap();
            assert!(Frames::new(a).next().await.is_err());
        }
    }

    #[test]
    fn unknown_events_are_forward_compatible_but_known_fields_are_checked() {
        assert!(matches!(
            serde_json::from_value::<Event>(json!({"event":"futureEvent"})).unwrap(),
            Event::Unknown
        ));
        assert!(
            serde_json::from_value::<Event>(
                json!({"event":"pairing","id":"a","qr":"x","expiresAt":-1})
            )
            .is_err()
        );
    }
}

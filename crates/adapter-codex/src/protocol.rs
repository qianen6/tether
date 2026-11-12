//! Line-delimited app-server JSON-RPC (intentionally no `jsonrpc` member).

use std::{collections::HashMap, path::Path, time::Duration};

use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    fs::{self, File},
    io::{AsyncWrite, AsyncWriteExt},
    time::Instant,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Id {
    Number(i64),
    String(String),
}

impl Id {
    pub(crate) fn approval_id(&self) -> String {
        match self {
            Self::Number(n) => format!("codex:number:{n}"),
            Self::String(s) => format!("codex:string:{s}"),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Message {
    Request {
        id: Id,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: Id,
        result: Result<Value, Value>,
    },
}

pub(crate) fn decode(line: &str) -> anyhow::Result<Message> {
    let v: Value = serde_json::from_str(line).context("invalid app-server JSON")?;
    let obj = v
        .as_object()
        .context("app-server message must be an object")?;
    if let Some(method) = obj.get("method") {
        let method = method
            .as_str()
            .context("method must be a string")?
            .to_owned();
        ensure!(
            !obj.contains_key("result") && !obj.contains_key("error"),
            "ambiguous request"
        );
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        return match obj.get("id") {
            Some(id) => Ok(Message::Request {
                id: serde_json::from_value(id.clone())?,
                method,
                params,
            }),
            None => Ok(Message::Notification { method, params }),
        };
    }
    let id = serde_json::from_value(obj.get("id").context("missing response id")?.clone())?;
    let result = match (obj.get("result"), obj.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) if error.is_object() => Err(error.clone()),
        _ => bail!("response must have exactly one result or error"),
    };
    Ok(Message::Response { id, result })
}

/// Raw directional JSONL files can be replayed with testkit::LineEnd.
/// Each spawn gets its own prefix, including when several sessions share a directory.
pub(crate) struct Recording {
    input: File,
    output: File,
}

impl Recording {
    pub(crate) async fn open(dir: &Path) -> anyhow::Result<(Self, File)> {
        fs::create_dir_all(dir)
            .await
            .context("create TETHER_RECORD_DIR")?;
        let prefix = format!("codex-{}", uuid::Uuid::new_v4());
        let input = File::create_new(dir.join(format!("{prefix}.stdin.jsonl"))).await?;
        let output = File::create_new(dir.join(format!("{prefix}.stdout.jsonl"))).await?;
        let stderr = File::create_new(dir.join(format!("{prefix}.stderr.log"))).await?;
        Ok((Self { input, output }, stderr))
    }

    pub(crate) async fn incoming(&mut self, line: &str) -> anyhow::Result<()> {
        write_line(&mut self.output, line).await
    }
}

async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, line: &str) -> anyhow::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

pub(crate) struct Pending<T> {
    pub(crate) method: &'static str,
    pub(crate) context: T,
    deadline: Instant,
}

/// The adapter drives reads concurrently with commands; response correlation and
/// deadlines live here so an approval never blocks the reader waiting for an RPC.
pub(crate) struct Rpc<W, T> {
    writer: W,
    next_id: i64,
    pending: HashMap<Id, Pending<T>>,
    recording: Option<Recording>,
    timeout: Duration,
}

impl<W: AsyncWrite + Unpin, T> Rpc<W, T> {
    pub(crate) fn new(writer: W, recording: Option<Recording>, timeout: Duration) -> Self {
        Self {
            writer,
            next_id: 0,
            pending: HashMap::new(),
            recording,
            timeout,
        }
    }

    pub(crate) async fn send(&mut self, value: Value) -> anyhow::Result<()> {
        let line = serde_json::to_string(&value)?;
        if let Some(recording) = &mut self.recording {
            write_line(&mut recording.input, &line)
                .await
                .context("record app-server stdin")?;
        }
        write_line(&mut self.writer, &line)
            .await
            .context("write app-server stdin")
    }

    pub(crate) async fn request(
        &mut self,
        method: &'static str,
        params: Value,
        context: T,
    ) -> anyhow::Result<Id> {
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("request id exhausted")?;
        let id = Id::Number(self.next_id);
        self.send(json!({"id": id, "method": method, "params": params}))
            .await?;
        self.pending.insert(
            id.clone(),
            Pending {
                method,
                context,
                deadline: Instant::now() + self.timeout,
            },
        );
        Ok(id)
    }

    pub(crate) fn resolve(&mut self, id: &Id) -> Option<Pending<T>> {
        self.pending.remove(id)
    }

    pub(crate) fn expired(&mut self, now: Instant) -> Vec<Pending<T>> {
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| self.pending.remove(&id))
            .collect()
    }

    pub(crate) async fn record_incoming(&mut self, line: &str) -> anyhow::Result<()> {
        if let Some(recording) = &mut self.recording {
            recording
                .incoming(line)
                .await
                .context("record app-server stdout")?;
        }
        Ok(())
    }

    pub(crate) async fn reject(&mut self, id: &Id, code: i32, message: &str) -> anyhow::Result<()> {
        self.send(json!({"id": id, "error": {"code": code, "message": message}}))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tether_agent_api::testkit::line_duplex;

    #[test]
    fn decode_all_wire_shapes_and_reject_ambiguous_input() {
        assert!(matches!(
            decode(r#"{"id":"r","method":"m","params":{}}"#).unwrap(),
            Message::Request {
                id: Id::String(_),
                ..
            }
        ));
        assert!(matches!(
            decode(r#"{"method":"initialized"}"#).unwrap(),
            Message::Notification {
                params: Value::Null,
                ..
            }
        ));
        assert!(matches!(
            decode(r#"{"id":-1,"result":null}"#).unwrap(),
            Message::Response {
                result: Ok(Value::Null),
                ..
            }
        ));
        assert!(matches!(
            decode(r#"{"id":1,"error":{"code":-1}}"#).unwrap(),
            Message::Response { result: Err(_), .. }
        ));
        for bad in [
            "bad",
            "[]",
            "{}",
            r#"{"id":null,"result":{}}"#,
            r#"{"id":1,"result":{},"error":{}}"#,
            r#"{"id":1,"method":4}"#,
            r#"{"id":1,"method":"m","result":{}}"#,
        ] {
            assert!(decode(bad).is_err(), "{bad}");
        }
        assert_ne!(
            Id::Number(1).approval_id(),
            Id::String("1".into()).approval_id()
        );
    }

    #[tokio::test]
    async fn rpc_correlates_out_of_order_responses_and_times_out() {
        let (a, mut b) = line_duplex();
        let (_, w) = a.into_parts();
        let mut rpc = Rpc::new(w, None, Duration::from_secs(1));
        let first = rpc.request("first", json!({}), 10).await.unwrap();
        let second = rpc.request("second", json!({}), 20).await.unwrap();
        assert_eq!(
            b.next_json().await.unwrap().unwrap(),
            json!({"id":1,"method":"first","params":{}})
        );
        assert_eq!(b.next_json().await.unwrap().unwrap()["id"], 2);
        assert_eq!(rpc.resolve(&second).unwrap().context, 20);
        assert!(rpc.resolve(&second).is_none());
        assert!(rpc.expired(Instant::now()).is_empty());
        assert_eq!(
            rpc.expired(Instant::now() + Duration::from_secs(2))[0].context,
            10
        );
        assert!(rpc.resolve(&first).is_none());
    }

    #[tokio::test]
    async fn recording_keeps_raw_directions_separate() {
        let dir = tempfile::tempdir().unwrap();
        let (record, _) = Recording::open(dir.path()).await.unwrap();
        let (a, mut b) = line_duplex();
        let (_, w) = a.into_parts();
        let mut rpc: Rpc<_, ()> = Rpc::new(w, Some(record), Duration::from_secs(1));
        rpc.send(json!({"method":"initialized"})).await.unwrap();
        rpc.record_incoming(r#"{"id":"s","result":{}}"#)
            .await
            .unwrap();
        assert_eq!(
            b.next_json().await.unwrap().unwrap(),
            json!({"method":"initialized"})
        );
        let paths: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let read = |suffix: &str| {
            std::fs::read_to_string(
                paths
                    .iter()
                    .find(|p| p.to_string_lossy().ends_with(suffix))
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(read(".stdin.jsonl"), "{\"method\":\"initialized\"}\n");
        assert_eq!(read(".stdout.jsonl"), "{\"id\":\"s\",\"result\":{}}\n");
    }
}

//! Bounded, single-writer JSON-RPC transport. Reading runs in its own task so
//! command selection never cancels a partially consumed line.

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use tether_proto::MAX_FRAME;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn read<R: AsyncBufRead + Unpin>(
    mut reader: R,
    messages: mpsc::Sender<Result<Value>>,
) {
    loop {
        match next(&mut reader).await {
            Ok(Some(value)) => {
                if messages.send(Ok(value)).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = messages.send(Err(error)).await;
                return;
            }
        }
    }
}

async fn next<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Option<Value>> {
    let mut line = Vec::new();
    let read = reader
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', &mut line)
        .await
        .context("reading ACP stdout")?;
    if read == 0 {
        return Ok(None);
    }
    ensure!(line.len() <= MAX_FRAME, "ACP frame exceeds 8 MiB");
    Ok(Some(
        serde_json::from_slice(&line).context("ACP stdout is not a UTF-8 JSON message")?,
    ))
}

pub(crate) async fn write<W: AsyncWrite + Unpin>(writer: &mut W, value: Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(&value)?;
    ensure!(bytes.len() < MAX_FRAME, "outgoing ACP frame exceeds 8 MiB");
    bytes.push(b'\n');
    tokio::time::timeout(WRITE_TIMEOUT, async {
        writer.write_all(&bytes).await?;
        writer.flush().await
    })
    .await
    .context("ACP stdin write timed out")?
    .context("writing ACP stdin")
}

pub(crate) fn valid_id(id: &Value) -> bool {
    id.is_string() || id.as_i64().is_some() || id.as_u64().is_some()
}

pub(crate) async fn error<W: AsyncWrite + Unpin>(
    writer: &mut W,
    id: Value,
    code: i64,
    message: &str,
) -> Result<()> {
    write(
        writer,
        json!({"jsonrpc":"2.0", "id":id, "error":{"code":code, "message":message}}),
    )
    .await
}

pub(crate) async fn permission<W: AsyncWrite + Unpin>(
    writer: &mut W,
    id: Value,
    option: Option<&str>,
) -> Result<()> {
    let outcome = match option {
        Some(option) => json!({"outcome":"selected", "optionId":option}),
        None => json!({"outcome":"cancelled"}),
    };
    write(
        writer,
        json!({"jsonrpc":"2.0", "id":id, "result":{"outcome":outcome}}),
    )
    .await
}

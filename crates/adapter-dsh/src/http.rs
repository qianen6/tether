//! Just enough HTTP/1.1 to POST JSON to a `dsh web` on loopback: one request
//! per connection (`Connection: close`), plain or chunked response bodies.

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Largest response body accepted (a history page with tool output).
const MAX_RESPONSE: usize = 64 << 20;

/// `http://host:port` → `host:port`. Only plain HTTP: `dsh web` serves no TLS.
pub(crate) fn authority(origin: &str) -> Result<&str> {
    let rest = origin
        .strip_prefix("http://")
        .with_context(|| format!("DSH 地址必须以 http:// 开头：{origin}"))?;
    let authority = rest.trim_end_matches('/');
    ensure!(
        !authority.is_empty() && !authority.contains('/'),
        "DSH 地址不能带路径：{origin}"
    );
    Ok(authority)
}

pub(crate) async fn post_json(
    origin: &str,
    path: &str,
    body: &Value,
    timeout: Duration,
) -> Result<Value> {
    let authority = authority(origin)?;
    let work = async {
        let stream = TcpStream::connect(authority)
            .await
            .with_context(|| format!("连接 DSH {origin} 失败"))?;
        exchange(stream, authority, path, body).await
    };
    tokio::time::timeout(timeout, work)
        .await
        .with_context(|| format!("DSH {path} 超时（{}s）", timeout.as_secs()))?
}

/// Write one request on `stream` and read the whole response.
pub(crate) async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    authority: &str,
    path: &str,
    body: &Value,
) -> Result<Value> {
    let body = serde_json::to_vec(body)?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    (&mut stream)
        .take(MAX_RESPONSE as u64 + 1)
        .read_to_end(&mut raw)
        .await?;
    ensure!(raw.len() <= MAX_RESPONSE, "DSH 的回应太大");
    let (status, body) = parse_response(&raw)?;
    let text = String::from_utf8_lossy(&body);
    if status != 200 {
        bail!("DSH {path} 返回 HTTP {status}：{}", text.trim());
    }
    serde_json::from_str(&text).with_context(|| format!("DSH {path} 的回应不是 JSON"))
}

/// Status code and decoded body.
pub(crate) fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>)> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("DSH 的 HTTP 回应不完整")?;
    let head = std::str::from_utf8(&raw[..split]).context("DSH 的 HTTP 头不是 UTF-8")?;
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .with_context(|| format!("DSH 的 HTTP 状态行无法识别：{status_line}"))?;
    let mut chunked = false;
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.to_ascii_lowercase().contains("chunked");
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.parse::<usize>().ok();
        }
    }
    let body = if chunked {
        dechunk(body)?
    } else if let Some(n) = length {
        ensure!(body.len() >= n, "DSH 的 HTTP 回应被截断");
        body[..n].to_vec()
    } else {
        body.to_vec()
    };
    Ok((status, body))
}

fn dechunk(mut rest: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .context("DSH 的分块回应不完整")?;
        let size_text = std::str::from_utf8(&rest[..end])?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .with_context(|| format!("DSH 的分块长度无法识别：{size_text}"))?;
        rest = &rest[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        ensure!(rest.len() >= size + 2, "DSH 的分块回应被截断");
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_and_chunked_bodies() {
        let plain =
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}";
        assert_eq!(parse_response(plain).unwrap(), (200, b"{}".to_vec()));
        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n5\r\n\":1}\n\r\n0\r\n\r\n";
        assert_eq!(
            parse_response(chunked).unwrap(),
            (200, b"{\"a\":1}\n".to_vec())
        );
        let missing = b"HTTP/1.1 404 Not Found\r\n\r\nnot found";
        assert_eq!(parse_response(missing).unwrap().0, 404);
        assert!(parse_response(b"HTTP/1.1 200").is_err());
    }

    #[test]
    fn origins() {
        assert_eq!(
            authority("http://127.0.0.1:3080").unwrap(),
            "127.0.0.1:3080"
        );
        assert_eq!(
            authority("http://127.0.0.1:3080/").unwrap(),
            "127.0.0.1:3080"
        );
        assert!(authority("https://x").is_err());
        assert!(authority("http://x/api").is_err());
    }

    #[tokio::test]
    async fn exchange_over_memory() {
        let (client, mut server) = tokio::io::duplex(4096);
        let serve = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let mut got = Vec::new();
            loop {
                let n = server.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                if got.ends_with(b"{\"x\":1}") {
                    break;
                }
            }
            let text = String::from_utf8(got).unwrap();
            assert!(text.starts_with("POST /api/host.describe HTTP/1.1\r\n"));
            assert!(text.contains("Host: 127.0.0.1:9\r\n"));
            server
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}")
                .await
                .unwrap();
        });
        let got = exchange(client, "127.0.0.1:9", "/api/host.describe", &json!({"x":1}))
            .await
            .unwrap();
        assert_eq!(got, json!({"ok": true}));
        serve.await.unwrap();
    }
}

//! Just enough HTTP/1.1 to POST JSON to a `dsh web` on loopback: one request
//! per connection (`Connection: close`), plain or chunked response bodies.

use std::time::Duration;

use anyhow::{Context, Result, ensure};
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
    post_json_with(origin, path, body, timeout, None).await
}

/// [`post_json`] carrying a `Cookie` header (DSH 0.2 authenticates with one).
pub(crate) async fn post_json_with(
    origin: &str,
    path: &str,
    body: &Value,
    timeout: Duration,
    cookie: Option<&str>,
) -> Result<Value> {
    let authority = authority(origin)?;
    let work = async {
        let stream = TcpStream::connect(authority)
            .await
            .with_context(|| format!("连接 DSH {origin} 失败"))?;
        exchange_with(stream, authority, path, body, cookie).await
    };
    tokio::time::timeout(timeout, work)
        .await
        .with_context(|| format!("DSH {path} 超时（{}s）", timeout.as_secs()))?
}

/// An HTTP status other than 200 that DSH answered with.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HttpStatus {
    pub status: u16,
    pub path: String,
    pub body: String,
}

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DSH {} 返回 HTTP {}：{}",
            self.path,
            self.status,
            self.body.trim()
        )
    }
}

impl std::error::Error for HttpStatus {}

/// The HTTP status inside `error`, if DSH answered with one.
pub(crate) fn status_of(error: &anyhow::Error) -> Option<u16> {
    error
        .chain()
        .find_map(|e| e.downcast_ref::<HttpStatus>().map(|s| s.status))
}

/// Write one request on `stream` and read the whole response.
#[cfg(test)]
pub(crate) async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    authority: &str,
    path: &str,
    body: &Value,
) -> Result<Value> {
    exchange_with(stream, authority, path, body, None).await
}

async fn exchange_with<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    authority: &str,
    path: &str,
    body: &Value,
    cookie: Option<&str>,
) -> Result<Value> {
    let body = serde_json::to_vec(body)?;
    let cookie = cookie
        .map(|c| format!("Cookie: {c}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         {cookie}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    let raw = read_all(&mut stream).await?;
    let (status, body) = parse_response(&raw)?;
    let text = String::from_utf8_lossy(&body);
    if status != 200 {
        return Err(HttpStatus {
            status,
            path: path.to_owned(),
            body: text.into_owned(),
        }
        .into());
    }
    serde_json::from_str(&text).with_context(|| format!("DSH {path} 的回应不是 JSON"))
}

async fn read_all<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    stream
        .take(MAX_RESPONSE as u64 + 1)
        .read_to_end(&mut raw)
        .await?;
    ensure!(raw.len() <= MAX_RESPONSE, "DSH 的回应太大");
    Ok(raw)
}

/// `GET path`: the status and the `Set-Cookie` pairs (`name=value`).
pub(crate) async fn get_cookies(
    origin: &str,
    path: &str,
    timeout: Duration,
) -> Result<(u16, Vec<String>)> {
    let authority = authority(origin)?;
    let work = async {
        let stream = TcpStream::connect(authority)
            .await
            .with_context(|| format!("连接 DSH {origin} 失败"))?;
        get_cookies_on(stream, authority, path).await
    };
    tokio::time::timeout(timeout, work)
        .await
        .with_context(|| format!("连接 DSH {origin} 超时（{}s）", timeout.as_secs()))?
}

pub(crate) async fn get_cookies_on<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    authority: &str,
    path: &str,
) -> Result<(u16, Vec<String>)> {
    let head = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await?;
    let raw = read_all(&mut stream).await?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("DSH 的 HTTP 回应不完整")?;
    let head = std::str::from_utf8(&raw[..split]).context("DSH 的 HTTP 头不是 UTF-8")?;
    let (status, _) = parse_response(&raw)?;
    let cookies = head
        .split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("set-cookie"))
        .filter_map(|(_, value)| value.split(';').next())
        .map(|pair| pair.trim().to_owned())
        .filter(|pair| pair.contains('='))
        .collect();
    Ok((status, cookies))
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
    async fn cookies_and_statuses() {
        let (client, mut server) = tokio::io::duplex(4096);
        let serve = tokio::spawn(async move {
            let mut buf = vec![0u8; 1024];
            let n = server.read(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
            assert!(text.starts_with("GET /connect?token=t&interactions=answer HTTP/1.1\r\n"));
            server
                .write_all(
                    b"HTTP/1.1 302 Found\r\nlocation: ./api/remote.mux\r\n\
                      set-cookie: dsh-auth-x=v2.abc; Max-Age=1; Path=/; HttpOnly\r\n\
                      Transfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let (status, cookies) = get_cookies_on(
            client,
            "127.0.0.1:9",
            "/connect?token=t&interactions=answer",
        )
        .await
        .unwrap();
        assert_eq!(
            (status, cookies),
            (302, vec!["dsh-auth-x=v2.abc".to_owned()])
        );
        serve.await.unwrap();

        let (client, mut server) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = server.read(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
            assert!(text.contains("Cookie: a=b\r\n"), "{text}");
            server
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\n\r\nunauthorized")
                .await
                .unwrap();
        });
        let error = exchange_with(client, "h", "/api/session/list", &json!({}), Some("a=b"))
            .await
            .unwrap_err();
        assert_eq!(status_of(&error), Some(401));
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

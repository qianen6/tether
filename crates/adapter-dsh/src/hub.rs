//! One `dsh web` shared by every DSH session: found or started on first use,
//! with its two downlink WebSockets (`/api/events.mux`, `/api/events.host`)
//! fanned out to the sessions.
//!
//! Where it comes from, first that answers `host.describe`:
//! 1. `TETHER_DSH_URL` (a `dsh web` someone else runs);
//! 2. the one Tether started last time (its address is kept in the temp
//!    folder, so a `dsh web` left behind by a crash is reused);
//! 3. `http://127.0.0.1:3080`, where `dsh web` listens by default — sessions
//!    then show live in DSH's own browser UI as well;
//! 4. otherwise Tether starts `dsh web --no-open --host 127.0.0.1 --port 0`
//!    itself, with the person's own `DSH_HOME`.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    sync::{Mutex, broadcast},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    api::{self, Api, HttpApi},
    http,
};

pub(crate) const URL_ENV: &str = "TETHER_DSH_URL";
pub(crate) const DEFAULT_ORIGIN: &str = "http://127.0.0.1:3080";
const STATE_FILE: &str = "tether-dsh-web.json";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// The first boot of a profile installs nothing but may be slow.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub(crate) enum Frame {
    /// A `ServerRequest` on the mux stream; `rpc_id` answers approvals and
    /// questions.
    Mux {
        rpc_id: String,
        payload: Value,
    },
    Host(Value),
    /// The connection to `dsh web` is gone.
    Down(String),
}

pub(crate) struct Hub {
    pub origin: String,
    pub api: Arc<HttpApi>,
    frames: broadcast::Sender<Arc<Frame>>,
    alive: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
    /// Kept so the process stays ours; dropped with the hub.
    _child: Option<Mutex<Child>>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Hub {
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Arc<Frame>> {
        self.frames.subscribe()
    }

    pub(crate) fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub(crate) fn api(&self) -> Arc<dyn Api> {
        self.api.clone()
    }
}

/// The cached hub, replaced when its connection drops.
#[derive(Default)]
pub(crate) struct Hubs {
    current: Mutex<Option<Arc<Hub>>>,
}

impl Hubs {
    pub(crate) async fn get(&self, command: &[String]) -> Result<Arc<Hub>> {
        let mut current = self.current.lock().await;
        if let Some(hub) = current.as_ref().filter(|h| h.alive()) {
            return Ok(hub.clone());
        }
        let hub = Arc::new(connect(command).await?);
        *current = Some(hub.clone());
        Ok(hub)
    }
}

fn state_file() -> PathBuf {
    std::env::temp_dir().join(STATE_FILE)
}

fn saved_origin() -> Option<String> {
    let text = std::fs::read_to_string(state_file()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("origin")?.as_str().map(str::to_owned)
}

pub(crate) async fn describe(origin: &str) -> Result<Value> {
    let body = api::request("host.describe", json!({}));
    let response = http::post_json(origin, "/api/host.describe", &body, PROBE_TIMEOUT).await?;
    api::unwrap_response("host.describe", &response)
}

/// Where a running `dsh web` may be, most specific first.
fn candidates() -> Vec<String> {
    let mut list = Vec::new();
    if let Some(url) = std::env::var(URL_ENV).ok().filter(|u| !u.trim().is_empty()) {
        list.push(url.trim().trim_end_matches('/').to_owned());
    }
    if let Some(saved) = saved_origin() {
        list.push(saved);
    }
    list.push(DEFAULT_ORIGIN.to_owned());
    list.dedup();
    list
}

async fn connect(command: &[String]) -> Result<Hub> {
    for origin in candidates() {
        match describe(&origin).await {
            Ok(_) => {
                tracing::info!(%origin, "using a running dsh web");
                return attach(origin, None).await;
            }
            Err(error) => tracing::debug!(%origin, "no dsh web: {error:#}"),
        }
    }
    if std::env::var(URL_ENV).is_ok_and(|u| !u.trim().is_empty()) {
        bail!("{URL_ENV} 指向的 DSH 没有响应");
    }
    let (origin, child) = launch(command).await?;
    let _ = std::fs::write(state_file(), json!({ "origin": origin }).to_string());
    attach(origin, Some(child)).await
}

fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// The `dsh web` launch line it prints: `dsh web: http://127.0.0.1:PORT`.
pub(crate) fn launch_origin(line: &str) -> Option<String> {
    let at = line.find("dsh web: http://")?;
    let rest = &line[at + "dsh web: ".len()..];
    let url = rest.split_whitespace().next()?;
    // 0.2 adds `/?token=…`; this adapter speaks the token-less 0.1 gateway.
    let url = url.split(['?', '#']).next().unwrap_or(url);
    Some(url.trim_end_matches('/').to_owned())
}

/// npm's Windows shim (`dsh.cmd`) runs `"%dp0%\node.exe" "%dp0%\…\bin.js" %*`.
/// Starting it through `cmd.exe` leaves its node behind when Tether stops
/// it, so read the script out of the shim and run node directly.
pub(crate) fn shim_target(shim: &Path, text: &str) -> Option<(PathBuf, PathBuf)> {
    let dir = shim.parent()?;
    let at = text.find("%dp0%\\node_modules\\")?;
    let rest = &text[at + "%dp0%\\".len()..];
    let end = rest.find(".js")? + ".js".len();
    let script = dir.join(&rest[..end]);
    let node = [dir.join("node.exe"), dir.join("node")]
        .into_iter()
        .find(|p| p.is_file())
        .or_else(|| which::which("node").ok())?;
    Some((node, script))
}

fn launcher(resolved: &Path) -> (PathBuf, Vec<String>) {
    let is_shim = resolved
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if is_shim
        && let Ok(text) = std::fs::read_to_string(resolved)
        && let Some((node, script)) = shim_target(resolved, &text)
        && script.is_file()
    {
        return (node, vec![script.display().to_string()]);
    }
    (resolved.to_path_buf(), Vec::new())
}

async fn launch(command: &[String]) -> Result<(String, Child)> {
    let program = command.first().context("没有配置 dsh 命令")?;
    let resolved = which::which(program).with_context(|| format!("未找到命令 {program}"))?;
    let (exe, first) = launcher(&resolved);
    let mut cmd = Command::new(&exe);
    cmd.args(first)
        .args(&command[1..])
        .args(["web", "--no-open", "--host", "127.0.0.1", "--port", "0"])
        .current_dir(home_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    tracing::info!(program = %exe.display(), "starting dsh web");
    let mut child = cmd
        .spawn()
        .with_context(|| format!("启动 {} 失败", resolved.display()))?;
    crate::job::adopt(&child);
    let stdout = child.stdout.take().context("dsh web stdout")?;
    let stderr = child.stderr.take().context("dsh web stderr")?;
    let tail = Arc::new(std::sync::Mutex::new(String::new()));
    drain(stderr, Some(tail.clone()));
    let mut lines = BufReader::new(stdout).lines();
    let found = tokio::time::timeout(LAUNCH_TIMEOUT, async {
        while let Some(line) = lines.next_line().await? {
            tracing::debug!("dsh web: {line}");
            if let Some(origin) = launch_origin(&line) {
                return Ok::<_, anyhow::Error>(Some(origin));
            }
        }
        Ok(None)
    })
    .await;
    let tail_text = || tail.lock().map(|t| t.clone()).unwrap_or_default();
    let origin = match found {
        Ok(Ok(Some(origin))) => origin,
        Ok(Ok(None)) => {
            let tail = tail_text();
            match explain_failure(&tail) {
                Some(why) => bail!("{why}"),
                None => bail!("dsh web 没有打印地址就退出了\n{}", tail.trim_end()),
            }
        }
        Ok(Err(error)) => return Err(error.context("读取 dsh web 输出失败")),
        Err(_) => bail!(
            "dsh web {} 秒内没有打印地址\n{}",
            LAUNCH_TIMEOUT.as_secs(),
            tail_text()
        ),
    };
    tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!("dsh web: {line}");
        }
    });
    tracing::info!(%origin, "dsh web started");
    Ok((origin, child))
}

/// What a failed `dsh web` start means, in words the phone can show.
pub(crate) fn explain_failure(stderr: &str) -> Option<String> {
    // A plugin that holds a fixed port (dsh-turbo-agent's proxy on 18888)
    // lets one DSH run at a time:
    // `dsh-turbo-agent: 127.0.0.1:18888 is already in use`.
    let line = stderr.lines().find(|l| l.contains(" is already in use"))?;
    let before = line[..line.find(" is already in use")?].trim_end();
    let address = before.rsplit(' ').next().unwrap_or(before);
    let plugin = before
        .rsplit_once(": ")
        .map(|(head, _)| head.rsplit(' ').next().unwrap_or(head))
        .filter(|p| p.starts_with("dsh-"));
    Some(format!(
        "DSH 启动不了：{}要用的地址 {address} 已被占用。通常是这台电脑上已经有一个 DSH 在运行（你自己开着的 DSH，或上次没退出干净的 dsh web），而 DSH 同一时间只能运行一个。\n\n\
         请在电脑上关掉那个 DSH，再在手机上重试。想一边在电脑上用、一边在手机上用：在电脑上用 `dsh web` 启动 DSH（默认端口 3080），Tether 会直接连上它。",
        plugin.map(|p| format!("插件 {p} ")).unwrap_or_default(),
    ))
}

/// Keep reading a pipe so the child never blocks on it; remember the tail.
fn drain(
    pipe: impl AsyncRead + Unpin + Send + 'static,
    tail: Option<Arc<std::sync::Mutex<String>>>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!("dsh web stderr: {line}");
            if let Some(tail) = &tail
                && let Ok(mut t) = tail.lock()
            {
                t.push_str(&line);
                t.push('\n');
                if t.len() > 4000 {
                    let cut = t.len() - 4000;
                    let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(0);
                    t.replace_range(..cut, "");
                }
            }
        }
    });
}

async fn attach(origin: String, child: Option<Child>) -> Result<Hub> {
    let authority = http::authority(&origin)?.to_owned();
    let (frames, _) = broadcast::channel(4096);
    let alive = Arc::new(AtomicBool::new(true));
    let mut tasks = Vec::new();
    for stream in ["events.mux", "events.host"] {
        let url = format!("ws://{authority}/api/{stream}");
        let (socket, _) =
            tokio::time::timeout(PROBE_TIMEOUT * 5, tokio_tungstenite::connect_async(&url))
                .await
                .with_context(|| format!("连接 {url} 超时"))?
                .with_context(|| format!("连接 {url} 失败"))?;
        let frames = frames.clone();
        let alive = alive.clone();
        let mux = stream == "events.mux";
        tasks.push(tokio::spawn(async move {
            let (_write, mut read) = socket.split();
            let reason = loop {
                match read.next().await {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(frame) = parse_frame(mux, &text) {
                            let _ = frames.send(Arc::new(frame));
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break "DSH 关闭了连接".to_owned(),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => break format!("与 DSH 的连接断开：{error}"),
                }
            };
            alive.store(false, Ordering::SeqCst);
            let _ = frames.send(Arc::new(Frame::Down(reason)));
        }));
    }
    Ok(Hub {
        api: HttpApi::new(origin.clone()),
        origin,
        frames,
        alive,
        tasks,
        _child: child.map(Mutex::new),
    })
}

/// A `ServerRequest` text frame from one of the two downlinks.
pub(crate) fn parse_frame(mux: bool, text: &str) -> Option<Frame> {
    let v: Value = serde_json::from_str(text).ok()?;
    let payload = v.get("payload")?.clone();
    if mux {
        let rpc_id = v.get("rpcId").and_then(Value::as_str)?.to_owned();
        Some(Frame::Mux { rpc_id, payload })
    } else {
        Some(Frame::Host(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_conflicts_are_explained() {
        let tail = concat!(
            "file:///C:/dsh/lib/index.js:1187\n",
            "    throw new Error(`${binName}: ${stage}: ${detail}${stack}`, { cause });\n\n",
            "Error: dsh: plugin tree failed to load: failed to apply loader entry dsh-turbo-agent ",
            "(@dsh-external/dsh-turbo-agent): dsh-turbo-agent: 127.0.0.1:18888 is already in use\n",
            "Error: dsh-turbo-agent: 127.0.0.1:18888 is already in use\n",
        );
        let why = explain_failure(tail).unwrap();
        assert!(
            why.contains("插件 dsh-turbo-agent 要用的地址 127.0.0.1:18888 已被占用"),
            "{why}"
        );
        assert!(why.contains("dsh web"));
        assert_eq!(explain_failure("Error: something else"), None);
    }

    #[test]
    fn launch_lines() {
        assert_eq!(
            launch_origin("dsh web: http://127.0.0.1:58533").as_deref(),
            Some("http://127.0.0.1:58533")
        );
        assert_eq!(
            launch_origin("dsh web: http://127.0.0.1:55765/?token=abc").as_deref(),
            Some("http://127.0.0.1:55765")
        );
        assert_eq!(launch_origin("starting…"), None);
    }

    #[test]
    fn npm_shims_run_their_script() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("node.exe");
        std::fs::write(&node, "").unwrap();
        let shim = dir.path().join("dsh.cmd");
        // The npm shim of DSH 0.1.1-rc.2, abridged.
        let text = concat!(
            "@ECHO off\r\n",
            r#"IF EXIST "%dp0%\node.exe" ("#,
            "\r\n",
            r#"  SET "_prog=%dp0%\node.exe""#,
            "\r\n)\r\n",
            r#"endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\node_modules\@deepseek-ai\dsh\lib\bin.js" %*"#,
            "\r\n",
        );
        let (got_node, script) = shim_target(&shim, text).unwrap();
        assert_eq!(got_node, node);
        assert_eq!(
            script,
            dir.path().join(r"node_modules\@deepseek-ai\dsh\lib\bin.js")
        );
        assert!(shim_target(&shim, "@echo off\r\nnode x.js").is_none());
        // Not a shim, or its script is missing: run it as it is.
        assert_eq!(launcher(&shim), (shim.clone(), Vec::new()));
    }

    #[test]
    fn frames() {
        let mux = r#"{"type":"server-request","rpcId":"r1","method":"events.mux","payload":{"type":"session/subscribed","sessionId":"s","lastSeq":2}}"#;
        match parse_frame(true, mux) {
            Some(Frame::Mux { rpc_id, payload }) => {
                assert_eq!(rpc_id, "r1");
                assert_eq!(payload["sessionId"], "s");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse_frame(
                false,
                r#"{"rpcId":"x","payload":{"type":"host/session-status"}}"#
            ),
            Some(Frame::Host(_))
        ));
        assert!(parse_frame(true, "not json").is_none());
    }
}

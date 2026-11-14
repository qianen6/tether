//! One `dsh web` shared by every DSH session: found or started on first use,
//! with its downlinks fanned out to the sessions as [`Frame`]s.
//!
//! Two gateways are spoken: DSH 0.1 (no auth; `/api/events.mux` and
//! `/api/events.host`) and DSH 0.2+ (the launch token is traded for a
//! cookie; one `/api/remote.mux`, see [`crate::v2`]). A URL carrying
//! `?token=` is 0.2; a token-less one that answers is 0.1.
//!
//! Where it comes from, first that answers:
//! 1. the address filled in on the computer ([`set_saved_url`]), then
//!    `TETHER_DSH_URL`: a `dsh web` the person runs, so the phone continues
//!    the same conversations (0.2 needs the whole URL it printed);
//! 2. `http://127.0.0.1:3080`, where 0.1's `dsh web` listens by default;
//! 3. otherwise Tether starts `dsh web --no-open --host 127.0.0.1 --port 0`
//!    itself, with the person's own `DSH_HOME`, and keeps the URL it prints
//!    (token included) in memory only.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, RwLock,
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
    http, v2,
};

pub(crate) const URL_ENV: &str = "TETHER_DSH_URL";
pub(crate) const DEFAULT_ORIGIN: &str = "http://127.0.0.1:3080";
const OLD_STATE_FILE: &str = "tether-dsh-web.json";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// The first boot of a profile installs nothing but may be slow.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(120);

static SAVED_URL: RwLock<Option<String>> = RwLock::new(None);

/// The address of a running `dsh web` the person filled in (the whole line
/// it printed, `http://127.0.0.1:PORT/?token=…` for 0.2), or none to find
/// or start one. Applies from the next connection on.
pub fn set_saved_url(url: Option<String>) {
    if let Ok(mut saved) = SAVED_URL.write() {
        *saved = url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
    }
}

fn saved_url() -> Option<String> {
    SAVED_URL.read().ok().and_then(|s| s.clone())
}

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
    api: Arc<dyn Api>,
    frames: broadcast::Sender<Arc<Frame>>,
    alive: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
    /// The `dsh web` Tether started, if it did; ended with the hub.
    _launched: Option<Launched>,
}

/// A `dsh web` Tether started: dropping it ends the process and everything
/// it started (see [`crate::job`]).
pub(crate) struct Launched {
    _tree: crate::job::Tree,
    _child: Mutex<Child>,
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

/// How long a failed connect answers later callers with the same error.
/// The phone asks for history, models and a session at once; without this,
/// each of them would start (and fail to use) another `dsh web` in turn.
const FAILURE_REUSE: Duration = Duration::from_secs(10);

/// The cached hub, replaced when its connection drops.
#[derive(Default)]
pub(crate) struct Hubs {
    current: Mutex<State>,
}

#[derive(Default)]
struct State {
    hub: Option<Arc<Hub>>,
    failed: Option<(tokio::time::Instant, String)>,
}

impl Hubs {
    pub(crate) async fn get(&self, command: &[String]) -> Result<Arc<Hub>> {
        let mut state = self.current.lock().await;
        if let Some(hub) = state.hub.as_ref().filter(|h| h.alive()) {
            return Ok(hub.clone());
        }
        if let Some((at, error)) = &state.failed
            && at.elapsed() < FAILURE_REUSE
        {
            bail!("{error}");
        }
        match connect(command).await {
            Ok(hub) => {
                let hub = Arc::new(hub);
                state.hub = Some(hub.clone());
                state.failed = None;
                Ok(hub)
            }
            Err(error) => {
                state.hub = None;
                state.failed = Some((tokio::time::Instant::now(), format!("{error:#}")));
                Err(error)
            }
        }
    }
}

/// Earlier versions kept the address of the `dsh web` they started in the
/// temp folder. That one now ends with Tether, so the address is always
/// stale: remove it rather than leave a wrong port lying around.
fn forget_old_state() {
    let _ = std::fs::remove_file(std::env::temp_dir().join(OLD_STATE_FILE));
}

/// DSH 0.1's `host.describe` at `origin`.
pub(crate) async fn describe(origin: &str) -> Result<Value> {
    let body = api::request("host.describe", json!({}));
    let response = http::post_json(origin, "/api/host.describe", &body, PROBE_TIMEOUT).await?;
    api::unwrap_response("host.describe", &response)
}

/// The address the person gave: the saved one, else `TETHER_DSH_URL`.
pub(crate) fn given_url() -> Option<String> {
    saved_url().or_else(env_url)
}

/// `TETHER_DSH_URL`: set by hand, so it must work.
fn env_url() -> Option<String> {
    std::env::var(URL_ENV)
        .ok()
        .map(|u| u.trim().to_owned())
        .filter(|u| !u.is_empty())
}

/// The URL in what the person pasted: the bare address, or the whole
/// `dsh web: http://…` line.
pub(crate) fn url_in(input: &str) -> Option<String> {
    let text = input.trim().trim_matches(['"', '\'']).trim();
    if let Some(url) = launch_url(text) {
        return Some(url);
    }
    let url = text.split_whitespace().next()?;
    v2::split_url(url)?;
    Some(match url.split_once('?') {
        Some((head, query)) => format!("{}/?{query}", head.trim_end_matches('/')),
        None => url.trim_end_matches('/').to_owned(),
    })
}

/// Whether `input` is a `dsh web` address rather than a launcher path.
pub fn is_url(input: &str) -> bool {
    let text = input.trim().trim_matches(['"', '\'']).trim();
    text.starts_with("http://") || text.starts_with("https://") || text.contains("dsh web: http")
}

/// Checks that a `dsh web` answers at `input` (a URL or the whole line `dsh
/// web` printed) and accepts its token, without opening a session. Returns
/// the URL as it will be used.
pub async fn check_url(input: &str) -> Result<String> {
    let url = url_in(input).context(
        "DSH 地址要以 http:// 开头，例如 dsh web 启动时打印的 http://127.0.0.1:端口/?token=…",
    )?;
    let (origin, token) = v2::split_url(&url).context("DSH 地址要以 http:// 开头")?;
    match token {
        Some(token) => {
            v2::cookie(&origin, &token)
                .await
                .with_context(|| format!("{origin} 上的 dsh web 没有接受这个地址"))?;
        }
        None => {
            if let Err(error) = describe(&origin).await {
                if http::status_of(&error) == Some(401) {
                    return Err(error.context(needs_token(&origin)));
                }
                return Err(error.context(format!("{origin} 上没有可用的 dsh web")));
            }
        }
    }
    Ok(url)
}

/// Where a running `dsh web` may be, most specific first.
fn candidates() -> Vec<String> {
    let mut list = Vec::new();
    list.extend(given_url());
    list.push(DEFAULT_ORIGIN.to_owned());
    list.dedup();
    list
}

/// Says a 0.2 `dsh web` answered without a token.
fn needs_token(origin: &str) -> String {
    format!(
        "{origin} 上的 DSH 是 0.2 或更新的版本，连接要带 token：请填写 dsh web 启动时打印的完整地址（http://127.0.0.1:端口/?token=…）"
    )
}

/// Connect to the `dsh web` at `url` (either version).
async fn attach_url(url: &str, launched: Option<Launched>) -> Result<Hub> {
    let (origin, token) =
        v2::split_url(url).with_context(|| format!("DSH 地址要以 http:// 开头：{url}"))?;
    match token {
        Some(token) => attach_v2(origin, &token, launched).await,
        None => {
            if let Err(error) = describe(&origin).await {
                if http::status_of(&error) == Some(401) {
                    return Err(error.context(needs_token(&origin)));
                }
                return Err(error.context(format!("{origin} 上没有可用的 dsh web")));
            }
            attach(origin, launched).await
        }
    }
}

async fn connect(command: &[String]) -> Result<Hub> {
    forget_old_state();
    let saved = saved_url();
    let strict = if saved.is_none() { env_url() } else { None };
    let mut hint = None;
    for url in candidates() {
        match attach_url(&url, None).await {
            Ok(hub) => {
                tracing::info!(origin = %hub.origin, "using a running dsh web");
                return Ok(hub);
            }
            Err(error) => {
                tracing::debug!("no dsh web at {}: {error:#}", redact(&url));
                if strict.as_deref() == Some(url.as_str()) {
                    return Err(error.context(format!("{URL_ENV} 里的 DSH 地址连不上")));
                }
                if saved.as_deref() == Some(url.as_str()) {
                    // A 0.2 token lasts as long as its dsh web: once that
                    // restarts, start one rather than stay stuck.
                    tracing::warn!(
                        "填写的 DSH 地址 {} 连不上，改为自己启动 dsh web：{error:#}",
                        redact(&url)
                    );
                    hint = Some(format!(
                        "填写的 DSH 地址连不上（dsh web 重启后 token 会变，请重新填写）：{error}"
                    ));
                } else if http::status_of(&error) == Some(401) && hint.is_none() {
                    hint = Some(format!("{error}"));
                }
            }
        }
    }
    let (url, launched) = match launch(command).await {
        Ok(found) => found,
        Err(error) => {
            return Err(match hint {
                Some(hint) => error.context(hint),
                None => error,
            });
        }
    };
    attach_url(&url, Some(launched)).await
}

/// `url` without its token, for logs.
pub(crate) fn redact(url: &str) -> String {
    match url.split_once("token=") {
        Some((head, _)) => format!("{head}token=…"),
        None => url.to_owned(),
    }
}

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// The URL in the launch line of `dsh web`, kept whole:
/// `dsh web: http://127.0.0.1:PORT` (0.1) or
/// `dsh web: http://127.0.0.1:PORT/?token=…` (0.2).
pub(crate) fn launch_url(line: &str) -> Option<String> {
    let at = line.find("dsh web: http://")?;
    let rest = &line[at + "dsh web: ".len()..];
    let url = rest.split_whitespace().next()?;
    Some(match url.split_once('?') {
        Some((head, query)) => format!("{}/?{query}", head.trim_end_matches('/')),
        None => url.trim_end_matches('/').to_owned(),
    })
}

/// npm's Windows shim (`dsh.cmd`) runs `"%dp0%\node.exe" "%dp0%\…\bin.js" %*`.
/// Starting it through `cmd.exe` leaves its node behind when Tether stops
/// it, so read the script out of the shim and run node directly.
pub(crate) fn shim_target(shim: &Path, text: &str) -> Option<(PathBuf, PathBuf)> {
    let dir = shim.parent()?;
    // `%dp0%\node_modules\…` (a global install) or `%dp0%\..\…` (a
    // project's `node_modules\.bin`).
    let end = text.find(".js\"")? + ".js".len();
    let at = text[..end].rfind("\"%dp0%\\")? + "\"%dp0%\\".len();
    let script = dir.join(&text[at..end]);
    let node = [dir.join("node.exe"), dir.join("node")]
        .into_iter()
        .find(|p| p.is_file())
        .or_else(|| which::which("node").ok())?;
    Some((node, script))
}

pub(crate) fn launcher(resolved: &Path) -> (PathBuf, Vec<String>) {
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

async fn launch(command: &[String]) -> Result<(String, Launched)> {
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
    crate::job::prepare(&mut cmd);
    tracing::info!(program = %exe.display(), "starting dsh web");
    let mut child = cmd
        .spawn()
        .with_context(|| format!("启动 {} 失败", resolved.display()))?;
    // From here on every early return drops `tree`, ending the whole tree.
    let tree = crate::job::Tree::adopt(&child);
    let stdout = child.stdout.take().context("dsh web stdout")?;
    let stderr = child.stderr.take().context("dsh web stderr")?;
    let tail = Arc::new(std::sync::Mutex::new(String::new()));
    drain(stderr, Some(tail.clone()));
    let mut lines = BufReader::new(stdout).lines();
    let found = tokio::time::timeout(LAUNCH_TIMEOUT, async {
        while let Some(line) = lines.next_line().await? {
            tracing::debug!("dsh web: {line}");
            if let Some(url) = launch_url(&line) {
                return Ok::<_, anyhow::Error>(Some(url));
            }
        }
        Ok(None)
    })
    .await;
    let tail_text = || tail.lock().map(|t| t.clone()).unwrap_or_default();
    let url = match found {
        Ok(Ok(Some(url))) => url,
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
    tracing::info!(url = %redact(&url), "dsh web started");
    Ok((
        url,
        Launched {
            _tree: tree,
            _child: Mutex::new(child),
        },
    ))
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

async fn attach(origin: String, launched: Option<Launched>) -> Result<Hub> {
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
        api: HttpApi::new(origin.clone()) as Arc<dyn Api>,
        origin,
        frames,
        alive,
        tasks,
        _launched: launched,
    })
}

/// Connect to a DSH 0.2+ `dsh web` with its launch token.
async fn attach_v2(origin: String, token: &str, launched: Option<Launched>) -> Result<Hub> {
    let opened = v2::open(&origin, token).await?;
    Ok(Hub {
        api: Arc::new(opened.api),
        origin,
        frames: opened.frames,
        alive: opened.alive,
        tasks: opened.tasks,
        _launched: launched,
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
            launch_url("dsh web: http://127.0.0.1:58533").as_deref(),
            Some("http://127.0.0.1:58533")
        );
        // 0.2: the token is kept; without it every request is refused.
        assert_eq!(
            launch_url("dsh web: http://127.0.0.1:55765/?token=abc").as_deref(),
            Some("http://127.0.0.1:55765/?token=abc")
        );
        assert_eq!(
            launch_url("dsh web: http://127.0.0.1:55765?token=abc (LAN: http://10.0.0.2:55765)")
                .as_deref(),
            Some("http://127.0.0.1:55765/?token=abc")
        );
        assert_eq!(launch_url("starting…"), None);
        // What the person pastes into the desktop app.
        for pasted in [
            "dsh web: http://127.0.0.1:50328/?token=abc",
            " \"http://127.0.0.1:50328?token=abc\" ",
            "http://127.0.0.1:50328/?token=abc",
        ] {
            assert!(is_url(pasted), "{pasted}");
            assert_eq!(
                url_in(pasted).as_deref(),
                Some("http://127.0.0.1:50328/?token=abc"),
                "{pasted}"
            );
        }
        assert_eq!(
            url_in("http://127.0.0.1:58533/").as_deref(),
            Some("http://127.0.0.1:58533")
        );
        assert!(!is_url(r"C:\Tools\dsh.cmd"));
        assert_eq!(url_in(r"C:\Tools\dsh.cmd"), None);
        assert_eq!(
            redact("http://127.0.0.1:1/?token=secret"),
            "http://127.0.0.1:1/?token=…"
        );
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
        // DSH 0.2 installed into a project: the shim is in node_modules\.bin.
        let local = concat!(
            r#"IF EXIST "%dp0%\node.exe" ("#,
            "\r\n",
            r#"endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\..\@deepseek-ai\dsh\lib\bin.js" %*"#,
        );
        let (_, script) = shim_target(&shim, local).unwrap();
        assert_eq!(script, dir.path().join(r"..\@deepseek-ai\dsh\lib\bin.js"));
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

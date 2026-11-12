//! Through the real tunnel: pair, chat with approval, terminal, revoke, Codex.
//! Manual (network): cargo test -p tether-e2e -- --ignored --nocapture

use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use tether_e2e::{Client, Frame, PhoneTunnel, WAIT};
use tether_host::{Host, HostConfig};
use tether_proto::{ClientMsg, Decision, EventKind, ServerMsg, SessionState, binary};

async fn start_host(dir: &std::path::Path) -> anyhow::Result<Host> {
    // The scripted echo agent; read once by Host::start.
    // SAFETY: set before any other thread of this test reads the environment.
    unsafe { std::env::set_var("TETHER_ENABLE_ECHO", "1") };
    Host::start(HostConfig {
        data_dir: dir.join("host"),
        name: "Tether e2e".into(),
        ..Default::default()
    })
    .await
}

/// Host → pair window → phone joins the tunnel → WebSocket pair.
async fn paired(host: &Host, dir: &std::path::Path) -> anyhow::Result<(PhoneTunnel, Client)> {
    let started = Instant::now();
    let info = host.pair(Duration::from_secs(300)).await?;
    let tunnel = PhoneTunnel::pair(&info.qr, dir).await?;
    eprintln!("隧道配对：{:?}", started.elapsed());
    let client = Client::pair(&tunnel.url(), &tunnel.payload).await?;
    eprintln!(
        "网关配对：{:?}，设备 {}",
        started.elapsed(),
        client.device_id
    );
    // The host ties the tunnel key to the device shortly after.
    for _ in 0..100 {
        if host.devices()?.iter().any(|d| d.phone_key.is_some()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ensure!(
        host.devices()?.iter().any(|d| d.phone_key.is_some()),
        "phone key never linked to the device"
    );
    Ok((tunnel, client))
}

fn event(frame: &Frame, session: &str) -> Option<EventKind> {
    match frame {
        Frame::Json(ServerMsg::Event { session: s, event }) if s == session => {
            Some(event.kind.clone())
        }
        _ => None,
    }
}

async fn create(c: &mut Client, agent: &str, cwd: Option<String>) -> anyhow::Result<String> {
    let agent = agent.to_string();
    let data = c
        .call(|req| ClientMsg::CreateSession {
            req,
            agent,
            cwd,
            title: None,
            resume: None,
            model: None,
            effort: None,
            chat: None,
            project: None,
            roots: None,
            speed: None,
            permission: None,
            reveal: None,
            fork: None,
        })
        .await?;
    let session = data["session"]["id"]
        .as_str()
        .context("no session id")?
        .to_string();
    let s = session.clone();
    c.call(|req| ClientMsg::Subscribe {
        req,
        session: s,
        after: 0,
    })
    .await?;
    Ok(session)
}

async fn echo_chat(c: &mut Client) -> anyhow::Result<()> {
    let session = create(c, "echo", None).await?;
    let s = session.clone();
    c.call(|req| ClientMsg::SendMessage {
        req,
        session: s,
        text: "approve:ls".into(),
        images: None,
    })
    .await?;
    let approval = c
        .wait(WAIT, |f| match event(f, &session) {
            Some(EventKind::Approval { approval }) => Some(approval.id),
            _ => None,
        })
        .await?;
    let s = session.clone();
    c.call(|req| ClientMsg::Answer {
        req,
        session: s,
        approval,
        decision: Decision::AllowOnce,
    })
    .await?;
    c.wait(WAIT, |f| {
        matches!(event(f, &session), Some(EventKind::TurnDone { .. })).then_some(())
    })
    .await?;
    let s = session.clone();
    c.call(|req| ClientMsg::CloseSession { req, session: s })
        .await?;
    eprintln!("Echo 对话 + 审批：通过");
    Ok(())
}

async fn terminal(c: &mut Client) -> anyhow::Result<()> {
    let data = c
        .call(|req| ClientMsg::TermOpen {
            req,
            command: None,
            agent: Some("shell".into()),
            cwd: None,
            cols: 100,
            rows: 30,
        })
        .await?;
    let term = data["term"]["id"]
        .as_str()
        .context("no term id")?
        .to_string();
    // The shell computes 42, so the echoed input line alone does not match.
    let input: &[u8] = if cfg!(windows) {
        b"echo tether-e2e-$(40+2)\r"
    } else {
        b"echo tether-e2e-$((40+2))\r"
    };
    c.send_binary(binary::encode(binary::TERM_INPUT, &term, input))
        .await?;
    let mut seen = Vec::new();
    c.wait(WAIT, |f| {
        if let Frame::Binary(b) = f
            && let Ok((binary::TERM_OUTPUT, t, data)) = binary::decode(b)
            && t == term
        {
            seen.extend_from_slice(data);
        }
        String::from_utf8_lossy(&seen)
            .contains("tether-e2e-42")
            .then_some(())
    })
    .await?;
    let t = term.clone();
    c.call(|req| ClientMsg::TermKill { req, term: t }).await?;
    eprintln!("终端经隧道执行 echo：通过");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual: real TailPocket tunnel over Tailcat relays"]
async fn pair_chat_terminal_revoke() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let host = start_host(dir.path()).await?;
    let result = async {
        let (mut tunnel, mut c) = paired(&host, dir.path()).await?;
        let agents = c.call(|req| ClientMsg::ListAgents { req }).await?;
        eprintln!("agents: {agents}");
        echo_chat(&mut c).await?;
        terminal(&mut c).await?;

        // A restarted phone app reconnects with its token.
        tunnel.reconnect().await?;
        let token = c.token.clone();
        let mut again = Client::hello(&tunnel.url(), &token).await?;
        again.call(|req| ClientMsg::ListSessions { req }).await?;

        let revoked = Instant::now();
        host.revoke(&again.device_id).await?;
        ensure!(
            again.closed_within(Duration::from_secs(5)).await,
            "revoked connection stayed open"
        );
        eprintln!("吊销后断开：{:?}", revoked.elapsed());
        ensure!(
            c.closed_within(Duration::from_secs(5)).await,
            "older connection stayed open"
        );
        // The tunnel no longer lets this phone in.
        tunnel.reconnect().await?;
        let dial = tokio::time::timeout(
            Duration::from_secs(40),
            Client::hello(&tunnel.url(), &token),
        )
        .await;
        ensure!(
            !matches!(dial, Ok(Ok(_))),
            "revoked phone got through the tunnel again"
        );
        eprintln!("吊销后隧道拒绝：通过");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    host.shutdown().await?;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual: real tunnel plus real Codex and the configured model"]
async fn codex_command_approval_through_tunnel() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work)?;
    let host = start_host(dir.path()).await?;
    let result = async {
        let (_tunnel, mut c) = paired(&host, dir.path()).await?;
        let session = create(&mut c, "codex", Some(work.display().to_string())).await?;
        let s = session.clone();
        c.call(|req| ClientMsg::SendMessage {
            req,
            session: s,
            text: "请用 shell 在当前目录创建 a.txt，内容为 hi。执行命令时明确请求用户审批（sandbox_permissions=require_escalated，理由：验证手机命令审批）。不要用文件编辑工具代替命令。完成后只回复 OK。".into(), images: None,
        })
        .await?;
        let long = Duration::from_secs(300);
        let approval = c
            .wait(long, |f| match event(f, &session) {
                Some(EventKind::Approval { approval }) => Some(approval.id),
                _ => None,
            })
            .await?;
        eprintln!("收到 Codex 命令审批 {approval}");
        let s = session.clone();
        c.call(|req| ClientMsg::Answer {
            req,
            session: s,
            approval,
            decision: Decision::AllowOnce,
        })
        .await?;
        c.wait(long, |f| match event(f, &session) {
            Some(EventKind::TurnDone { .. }) => Some(()),
            Some(EventKind::State {
                state: SessionState::Error,
                status_line,
            }) => panic!("codex failed: {status_line:?}"),
            _ => None,
        })
        .await?;
        let bytes = std::fs::read(work.join("a.txt")).context("a.txt not created")?;
        ensure!(String::from_utf8_lossy(&bytes).contains('h'), "a.txt has no hi");
        eprintln!("Codex 经隧道完成带审批的任务：通过");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    host.shutdown().await?;
    result
}

use super::*;

use std::{process::Command, sync::OnceLock};

struct MockProgram {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

fn mock_program() -> &'static PathBuf {
    static PROGRAM: OnceLock<MockProgram> = OnceLock::new();
    &PROGRAM
        .get_or_init(|| {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(if cfg!(windows) {
                "mock-acp.exe"
            } else {
                "mock-acp"
            });
            let source =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mock_acp/mock_program.rs");
            let mut compiler =
                Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
            compiler
                .arg("--edition=2024")
                .arg(&source)
                .arg("-o")
                .arg(&path);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                compiler.creation_flags(0x0800_0000);
            }
            let output = compiler
                .output()
                .expect("offline Rust compiler for the std-only test agent");
            assert!(
                output.status.success(),
                "mock compiler failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            MockProgram {
                _directory: directory,
                path,
            }
        })
        .path
}

fn process_adapter(args: &[&str]) -> std::sync::Arc<dyn AgentAdapter> {
    adapter(AcpAgent {
        id: "mock-process".into(),
        name: "Rust mock ACP process".into(),
        command: std::iter::once(mock_program().to_string_lossy().into_owned())
            .chain(args.iter().map(|arg| (*arg).to_owned()))
            .collect(),
    })
}

#[tokio::test]
async fn spawn_returns_immediately_and_reports_missing_executable_as_exited() {
    let adapter = adapter(AcpAgent {
        id: "missing".into(),
        name: "Missing".into(),
        command: vec!["tether-no-such-program-8b0f".into()],
    });
    let mut d = Driver::spawn(adapter.as_ref(), SpawnOptions::default()).unwrap();
    let (code, error) = exit(&mut d).await;
    assert_eq!(code, None);
    assert!(error.unwrap().contains("ACP command not found"));
    assert!(d.rx.recv().await.is_none());
}

#[tokio::test]
async fn empty_command_is_rejected_unless_program_override_is_present() {
    let adapter = AcpAdapter {
        agent: AcpAgent {
            id: "empty".into(),
            name: "Empty".into(),
            command: vec![],
        },
    };
    let (out, _) = mpsc::unbounded_channel();
    assert!(adapter.spawn(SpawnOptions::default(), out.clone()).is_err());
    let cmd = adapter
        .spawn(
            SpawnOptions {
                program: Some(PathBuf::from("missing-override-agent")),
                ..Default::default()
            },
            out,
        )
        .unwrap();
    drop(cmd);
}

#[tokio::test]
async fn real_mock_stdio_honors_program_override_preset_args_extra_args_env_and_cwd() {
    let directory = tempfile::tempdir().unwrap();
    let cwd = directory.path().join("中文 folder with spaces");
    std::fs::create_dir(&cwd).unwrap();
    let a = adapter(AcpAgent {
        id: "override".into(),
        name: "Override".into(),
        command: vec!["not-the-real-executable".into(), "--preset".into()],
    });
    let mut d = Driver::spawn(
        a.as_ref(),
        SpawnOptions {
            cwd: cwd.clone(),
            program: Some(mock_program().clone()),
            args: vec!["--extra".into()],
            env: vec![("TETHER_MOCK_ENV".into(), "env-中文".into())],
            ..Default::default()
        },
    )
    .unwrap();
    send(&d, "probe");
    idle(&mut d, 1).await;
    let text = d
        .events()
        .iter()
        .find_map(|e| match e {
            EventKind::AssistantText { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(text.contains("中文 folder with spaces"));
    assert!(text.contains("env=env-中文"));
    assert!(text.contains("args=--preset|--extra"));
    d.send(AgentCommand::Shutdown).unwrap();
    assert!(exit(&mut d).await.1.is_none());
    assert!(d.rx.recv().await.is_none());
}

#[tokio::test]
async fn process_exit_drains_final_stdout_before_exited() {
    let a = process_adapter(&["--exit-after-prompt"]);
    let mut d = Driver::spawn(a.as_ref(), SpawnOptions::default()).unwrap();
    send(&d, "exit normally");
    assert_eq!(exit(&mut d).await, (Some(0), None));
    assert!(names(&d).ends_with(&["text", "turn_done", "idle"]));
    assert!(d.rx.recv().await.is_none());
}

#[tokio::test]
async fn crashed_process_preserves_exit_status_and_drains_stderr_without_deadlock() {
    let a = process_adapter(&["--stderr-flood", "--crash"]);
    let mut d = Driver::spawn(a.as_ref(), SpawnOptions::default()).unwrap();
    send(&d, "crash");
    let (code, error) = exit(&mut d).await;
    assert_eq!(code, Some(7));
    let error = error.unwrap();
    assert!(error.contains("STDERR-END"));
    assert!(error.contains("MOCK-CRASH"));
    assert!(error.len() < MAX_TOOL_OUTPUT + 300);
    assert!(d.rx.recv().await.is_none());
}

#[tokio::test]
async fn process_interrupt_uses_cancel_and_keeps_process_available_for_next_turn() {
    let a = process_adapter(&["--wait-for-cancel"]);
    let mut d = Driver::spawn(a.as_ref(), SpawnOptions::default()).unwrap();
    for turn in 1..=2 {
        send(&d, "wait");
        d.wait_until(
            |out| {
                out.iter()
                    .filter(|o| matches!(o.event(), Some(EventKind::AssistantDelta { .. })))
                    .count()
                    >= turn
            },
            T,
        )
        .await
        .unwrap();
        d.send(AgentCommand::Interrupt).unwrap();
        idle(&mut d, turn).await;
    }
    d.send(AgentCommand::Shutdown).unwrap();
    assert!(exit(&mut d).await.1.is_none());
    assert!(d.rx.recv().await.is_none());
}

#[tokio::test]
async fn shutdown_and_drop_reap_an_unresponsive_mock_process() {
    for dropping in [true, false] {
        let a = process_adapter(&["--silent-initialize"]);
        let mut d = Driver::spawn(a.as_ref(), SpawnOptions::default()).unwrap();
        // NativeId cannot arrive, but the command loop must still react.
        send(&d, "queued");
        d.wait_until(
            |out| {
                out.iter().any(|o| {
                    matches!(
                        o.event(),
                        Some(EventKind::State {
                            state: SessionState::Working,
                            ..
                        })
                    )
                })
            },
            T,
        )
        .await
        .unwrap();
        if dropping {
            let (tx, _) = mpsc::unbounded_channel();
            drop(std::mem::replace(&mut d.cmd, tx));
        } else {
            d.send(AgentCommand::Shutdown).unwrap();
        }
        let (code, error) = exit(&mut d).await;
        assert!(code.is_some() || cfg!(unix));
        assert!(error.is_none());
        assert!(d.rx.recv().await.is_none());
    }
}

#[cfg(windows)]
#[tokio::test]
async fn windows_resolves_cmd_using_configured_path_with_spaces() {
    let directory = tempfile::tempdir().unwrap();
    let tools = directory.path().join("tools with spaces");
    std::fs::create_dir(&tools).unwrap();
    let script = tools.join("tether-acp-mock.cmd");
    std::fs::write(
        &script,
        format!("@echo off\r\n\"{}\" %*\r\n", mock_program().display()),
    )
    .unwrap();
    let a = adapter(AcpAgent {
        id: "cmd".into(),
        name: "CMD mock".into(),
        command: vec!["tether-acp-mock".into(), "--preset".into()],
    });
    let mut d = Driver::spawn(
        a.as_ref(),
        SpawnOptions {
            env: vec![("PATH".into(), tools.to_string_lossy().into_owned())],
            ..Default::default()
        },
    )
    .unwrap();
    send(&d, "cmd wrapper");
    idle(&mut d, 1).await;
    assert!(
        d.events().iter().any(
            |e| matches!(e,EventKind::AssistantText{text,..} if text.contains("args=--preset"))
        )
    );
    d.send(AgentCommand::Shutdown).unwrap();
    assert!(exit(&mut d).await.1.is_none());
    assert!(d.rx.recv().await.is_none());
}

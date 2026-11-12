use std::{path::PathBuf, time::Duration};

use tether_terminals::{DEFAULT_SCROLLBACK, TermChunk, TermManager};
use tokio::{sync::broadcast, time::timeout};

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap()
}

fn shell(script: &str) -> Vec<String> {
    #[cfg(windows)]
    let command = vec!["cmd.exe", "/d", "/q", "/c", script];
    #[cfg(unix)]
    let command = vec!["sh", "-c", script];
    command.into_iter().map(str::to_owned).collect()
}

async fn next(rx: &mut broadcast::Receiver<TermChunk>) -> TermChunk {
    timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("PTY output timed out")
        .expect("PTY broadcast closed or lagged")
}

async fn through_exit(rx: &mut broadcast::Receiver<TermChunk>) -> (Vec<u8>, Option<i32>) {
    let mut output = Vec::new();
    loop {
        match next(rx).await {
            TermChunk::Data(data) => output.extend_from_slice(&data),
            TermChunk::Attention => {}
            TermChunk::Exit(code) => return (output, code),
        }
    }
}

#[tokio::test]
async fn echo_hello_exits_zero() {
    let manager = TermManager::new(DEFAULT_SCROLLBACK);
    let term = manager.open(shell("echo hello"), cwd(), 80, 24).unwrap();
    let (mut output, mut rx) = manager.attach(&term.id).unwrap();
    let (live, code) = through_exit(&mut rx).await;
    output.extend(live);
    assert!(String::from_utf8_lossy(&output).contains("hello"));
    assert_eq!(code, Some(0));
    assert!(
        !manager
            .list()
            .iter()
            .find(|t| t.id == term.id)
            .unwrap()
            .running
    );
}

#[tokio::test]
async fn interactive_attach_resize_and_kill() {
    let manager = TermManager::new(DEFAULT_SCROLLBACK);
    #[cfg(windows)]
    let command = vec!["cmd.exe", "/d", "/q"];
    #[cfg(unix)]
    let command = vec!["sh", "-i"];
    let term = manager
        .open(
            command.into_iter().map(str::to_owned).collect(),
            cwd(),
            80,
            24,
        )
        .unwrap();
    let (mut expected, mut first) = manager.attach(&term.id).unwrap();
    manager.input(&term.id, b"echo abc\r").unwrap();
    while !String::from_utf8_lossy(&expected).contains("abc") {
        if let TermChunk::Data(data) = next(&mut first).await {
            expected.extend(data);
        }
    }
    let (mut reattached, mut second) = manager.attach(&term.id).unwrap();
    assert!(String::from_utf8_lossy(&reattached).contains("abc"));
    manager.resize(&term.id, 100, 30).unwrap();
    let resized = manager
        .list()
        .into_iter()
        .find(|t| t.id == term.id)
        .unwrap();
    assert_eq!((resized.cols, resized.rows), (100, 30));
    manager.input(&term.id, b"echo after_attach\r").unwrap();
    while !String::from_utf8_lossy(&expected).contains("after_attach") {
        if let TermChunk::Data(data) = next(&mut first).await {
            expected.extend(data);
        }
    }
    manager.kill(&term.id).unwrap();
    let (tail, _) = through_exit(&mut first).await;
    expected.extend(tail);
    let (tail, _) = through_exit(&mut second).await;
    reattached.extend(tail);
    assert_eq!(
        reattached, expected,
        "snapshot + broadcast must equal the continuous stream"
    );
    assert!(manager.list().is_empty());
    assert!(manager.attach(&term.id).is_err());
}

#[tokio::test]
async fn nonzero_exit_is_retained_and_late_attach_gets_status() {
    let manager = TermManager::new(DEFAULT_SCROLLBACK);
    #[cfg(windows)]
    let command = shell("echo final_output & exit /b 7");
    #[cfg(unix)]
    let command = shell("echo final_output; exit 7");
    let term = manager.open(command, cwd(), 80, 24).unwrap();
    let (_, mut rx) = manager.attach(&term.id).unwrap();
    assert_eq!(through_exit(&mut rx).await.1, Some(7));
    assert_eq!(rx.recv().await, Err(broadcast::error::RecvError::Closed));
    let (snapshot, mut late) = manager.attach(&term.id).unwrap();
    assert!(String::from_utf8_lossy(&snapshot).contains("final_output"));
    assert_eq!(next(&mut late).await, TermChunk::Exit(Some(7)));
    assert_eq!(manager.list().len(), 1);
    assert!(manager.input(&term.id, b"ignored").is_err());
    assert!(manager.resize(&term.id, 100, 30).is_err());
    manager.kill(&term.id).unwrap();
    assert!(manager.list().is_empty());
}

#[tokio::test]
async fn environment_cwd_uuid_and_metadata() {
    let manager = TermManager::new(DEFAULT_SCROLLBACK);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("marker.txt"), "cwd_marker_73591\n").unwrap();
    #[cfg(windows)]
    let command = shell("type marker.txt & echo %TERM%:%COLORTERM%");
    #[cfg(unix)]
    let command = shell("cat marker.txt; printf '%s:%s' \"$TERM\" \"$COLORTERM\"");
    let before = tether_proto::now_ms();
    let term = manager
        .open(command.clone(), dir.path().to_owned(), 93, 27)
        .unwrap();
    assert!(uuid::Uuid::parse_str(&term.id).is_ok());
    assert_eq!(term.command, command);
    assert_eq!((term.cols, term.rows), (93, 27));
    assert_eq!(term.cwd, dir.path().to_str().unwrap());
    assert!(term.created_at >= before);
    let (mut output, mut rx) = manager.attach(&term.id).unwrap();
    let (tail, code) = through_exit(&mut rx).await;
    output.extend(tail);
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("cwd_marker_73591"), "{text:?}");
    assert!(text.contains("xterm-256color:truecolor"), "{text:?}");
    assert_eq!(code, Some(0));
}

#[tokio::test]
async fn bell_triggers_attention_on_real_pty() {
    let manager = TermManager::new(DEFAULT_SCROLLBACK);
    #[cfg(windows)]
    let command = vec![
        "powershell.exe",
        "-NoProfile",
        "-Command",
        "[Console]::WriteLine('bell_ready'); $null=Read-Host; [Console]::Write([char]7); $null=Read-Host",
    ];
    #[cfg(unix)]
    let command = vec![
        "sh",
        "-c",
        "echo bell_ready; read x; printf '\\007'; read x",
    ];
    let term = manager
        .open(
            command.into_iter().map(str::to_owned).collect(),
            cwd(),
            80,
            24,
        )
        .unwrap();
    let (_, mut rx) = manager.attach(&term.id).unwrap();
    let mut startup = Vec::new();
    let mut pending_bells = 0_usize;
    while !String::from_utf8_lossy(&startup).contains("bell_ready") || pending_bells > 0 {
        match next(&mut rx).await {
            TermChunk::Data(data) => {
                pending_bells += usize::from(data.contains(&7));
                startup.extend(data);
            }
            TermChunk::Attention => pending_bells = pending_bells.saturating_sub(1),
            TermChunk::Exit(code) => panic!("exited during startup: {code:?}"),
        }
    }
    manager.input(&term.id, b"\r").unwrap();
    let mut bell_seen = false;
    loop {
        match next(&mut rx).await {
            TermChunk::Data(data) => bell_seen |= data.contains(&7),
            TermChunk::Attention => {
                assert!(bell_seen, "must be a bell, not the 8s silence timer");
                break;
            }
            TermChunk::Exit(code) => panic!("exited before bell: {code:?}"),
        }
    }
    manager.kill(&term.id).unwrap();
    through_exit(&mut rx).await;
}

#[tokio::test]
async fn large_output_is_drained_before_exit_and_scrollback_is_bounded() {
    let manager = TermManager::new(128);
    #[cfg(windows)]
    let command = shell("for /L %i in (1,1,2000) do @echo line_%i");
    #[cfg(unix)]
    let command = shell("i=1; while [ $i -le 2000 ]; do echo line_$i; i=$((i+1)); done");
    let term = manager.open(command, cwd(), 100, 30).unwrap();
    let (mut output, mut rx) = manager.attach(&term.id).unwrap();
    let (tail, code) = through_exit(&mut rx).await;
    output.extend(tail);
    assert_eq!(code, Some(0));
    let (snapshot, _) = manager.attach(&term.id).unwrap();
    assert!(snapshot.len() <= 128);
    // ConPTY may emit cursor/erase sequences, but must retain the final line.
    assert!(String::from_utf8_lossy(&snapshot).contains("line_2000"));
    assert!(String::from_utf8_lossy(&output).contains("line_2000"));
}

#[tokio::test]
async fn dropping_manager_terminates_its_running_shell() {
    let manager = TermManager::new(DEFAULT_SCROLLBACK);
    #[cfg(windows)]
    let command = vec!["cmd.exe", "/d", "/q"];
    #[cfg(unix)]
    let command = vec!["sh", "-i"];
    let term = manager
        .open(
            command.into_iter().map(str::to_owned).collect(),
            cwd(),
            80,
            24,
        )
        .unwrap();
    let (_, mut rx) = manager.attach(&term.id).unwrap();
    drop(manager);
    through_exit(&mut rx).await;
}

#[test]
fn spawn_failure_and_invalid_cwd_do_not_register_a_terminal() {
    let manager = TermManager::new(DEFAULT_SCROLLBACK);
    let dir = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let program = dir.path().join("invalid.exe");
    #[cfg(unix)]
    let program = dir.path().join("invalid");
    std::fs::write(&program, "this is not an executable binary").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Not executable: an executable text file runs as a shell script on
        // Linux (execvp's fallback), and would start after all.
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert!(
        manager
            .open(vec![program.to_str().unwrap().into()], cwd(), 80, 24)
            .is_err()
    );
    assert!(
        manager
            .open(shell("echo hello"), dir.path().join("missing"), 80, 24)
            .is_err()
    );
    assert!(manager.list().is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn resolves_exe_cmd_bat_and_spaced_unicode_paths() {
    let manager = TermManager::new(DEFAULT_SCROLLBACK);
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("终端 space");
    std::fs::create_dir(&cwd).unwrap();
    for ext in ["cmd", "bat"] {
        let path = cwd.join(format!("runner.{ext}"));
        std::fs::write(&path, "@echo off\r\necho batch_marker\r\necho \"%~1\"\r\n").unwrap();
        let term = manager
            .open(
                vec![
                    format!(".\\runner.{ext}"),
                    "hello space!%PATH%&literal".into(),
                ],
                cwd.clone(),
                120,
                30,
            )
            .unwrap();
        let (mut output, mut rx) = manager.attach(&term.id).unwrap();
        let (tail, code) = through_exit(&mut rx).await;
        output.extend(tail);
        let text = String::from_utf8_lossy(&output);
        assert_eq!(code, Some(0), "{text:?}");
        assert!(text.contains("batch_marker"), "{text:?}");
        assert!(text.contains("hello space!%PATH%&literal"), "{text:?}");
    }
    // Extension omitted: PATHEXT resolution must select runner.cmd.
    let term = manager
        .open(
            vec![".\\runner".into(), "extension_lookup".into()],
            cwd,
            80,
            24,
        )
        .unwrap();
    let (mut output, mut rx) = manager.attach(&term.id).unwrap();
    let (tail, code) = through_exit(&mut rx).await;
    output.extend(tail);
    assert_eq!(code, Some(0));
    assert!(String::from_utf8_lossy(&output).contains("extension_lookup"));
}

#[cfg(windows)]
#[tokio::test]
async fn batch_shims_forward_quotes_metacharacters_and_backslashes() {
    use std::{os::windows::process::CommandExt, process::Command};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("argv.rs");
    let exe = dir.path().join("argv.exe");
    std::fs::write(
        &source,
        r#"fn main() { for arg in std::env::args().skip(1) { println!("ARG:{arg:?}"); } }"#,
    )
    .unwrap();
    let build = Command::new("rustc")
        .args(["--crate-name", "tether_argv_fixture"])
        .arg(&source)
        .arg("-o")
        .arg(&exe)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW for this non-PTY helper.
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let script = dir.path().join("shim.cmd");
    std::fs::write(&script, "@echo off\r\n\"%~dp0argv.exe\" %*\r\n").unwrap();
    let args = [
        "",
        "hello space",
        "a\"b",
        "{\"a\":\"x&y\"}",
        "tail\\",
        "slash\\\"quote",
        "!%PATH%&|<>()^",
        "x\" & echo INJECT & rem \"",
    ];
    let mut command = vec![script.to_str().unwrap().to_owned()];
    command.extend(args.iter().map(|s| (*s).to_owned()));
    let manager = TermManager::new(DEFAULT_SCROLLBACK);
    let term = manager
        .open(command, dir.path().to_owned(), 250, 40)
        .unwrap();
    let (mut output, mut rx) = manager.attach(&term.id).unwrap();
    let (tail, code) = through_exit(&mut rx).await;
    output.extend(tail);
    let text = String::from_utf8_lossy(&output);
    assert_eq!(code, Some(0), "{text:?}");
    for arg in args {
        assert!(text.contains(&format!("ARG:{arg:?}")), "{text:?}");
    }
    assert_eq!(text.matches("ARG:").count(), args.len(), "{text:?}");
    assert!(
        manager
            .open(
                vec![script.to_str().unwrap().into(), "a\nb".into()],
                dir.path().to_owned(),
                80,
                24
            )
            .is_err()
    );
}

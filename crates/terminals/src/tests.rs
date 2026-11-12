use super::*;
use std::{
    io::{self, Cursor},
    sync::{
        Barrier,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

fn info() -> TermInfo {
    TermInfo {
        id: "memory".into(),
        command: vec!["memory-shell".into()],
        cwd: ".".into(),
        cols: 80,
        rows: 24,
        running: true,
        created_at: 0,
    }
}

#[test]
fn manager_is_debug_send_sync() {
    fn assert_traits<T: fmt::Debug + Send + Sync>() {}
    assert_traits::<TermManager>();
    assert_traits::<TermChunk>();
    assert_eq!(Timing::default().attention_after, Duration::from_secs(8));
    assert_eq!(Timing::default().retain_exited, Duration::from_secs(600));
    assert_eq!(DEFAULT_SCROLLBACK, 512 * 1024);
}

#[test]
fn ring_keeps_exact_capacity_and_whole_lines() {
    let mut ring = Scrollback::new(8);
    ring.push(b"aaa\nbbb\n");
    assert_eq!(ring.snapshot(), b"aaa\nbbb\n");
    ring.push(b"ccc\n");
    assert_eq!(ring.snapshot(), b"bbb\nccc\n");
    ring.push(b"dd");
    assert_eq!(ring.snapshot(), b"ccc\ndd");
}

#[test]
fn ring_skips_utf8_and_ansi_fragments_at_cut() {
    let mut ring = Scrollback::new(14);
    ring.push("\x1b[31m旧行🙂\x1b[0m\n新行\n".as_bytes());
    assert_eq!(ring.snapshot(), "新行\n".as_bytes());
    assert!(std::str::from_utf8(&ring.snapshot()).is_ok());
}

#[test]
fn ring_discards_an_overlong_line_across_reads() {
    let mut ring = Scrollback::new(4);
    ring.push(b"12345");
    assert!(ring.snapshot().is_empty());
    ring.push(b"\x1b[3");
    ring.push(b"1m");
    assert!(ring.snapshot().is_empty());
    ring.push(b"\r\nok\n");
    assert_eq!(ring.snapshot(), b"ok\n");
}

#[test]
fn ring_handles_zero_capacity_and_many_wraps() {
    let mut zero = Scrollback::new(0);
    zero.push(b"anything\n");
    assert!(zero.snapshot().is_empty());
    let mut ring = Scrollback::new(15);
    for n in 0..1000 {
        ring.push(format!("{n:04}\n").as_bytes());
        assert!(ring.snapshot().len() <= 15);
    }
    assert_eq!(ring.snapshot(), b"0997\n0998\n0999\n");
}

#[test]
fn ring_retains_a_new_complete_line_after_a_huge_chunk() {
    let mut ring = Scrollback::new(6);
    ring.push(&[b'x'; 20_000]);
    ring.push(b"rest\nnew\n");
    assert_eq!(ring.snapshot(), b"new\n");
}

#[test]
fn memory_reader_preserves_raw_bytes_and_bell() {
    let output = Mutex::new(Output::new(info(), 100));
    let (_, mut rx) = lock(&output).attach();
    let bytes = b"\x1b[31mhello\x1b[0m\r\n\x07\xff";
    read_output(Cursor::new(bytes), &output);
    assert_eq!(
        rx.try_recv().unwrap(),
        TermChunk::Data(bytes::Bytes::copy_from_slice(bytes))
    );
    assert_eq!(rx.try_recv().unwrap(), TermChunk::Attention);
    assert_eq!(lock(&output).attach().0, bytes);
    assert!(rx.try_recv().is_err());
}

struct InterruptedReader(u8);

impl Read for InterruptedReader {
    fn read(&mut self, data: &mut [u8]) -> io::Result<usize> {
        self.0 += 1;
        match self.0 {
            1 => Err(io::ErrorKind::Interrupted.into()),
            2 => {
                data[..3].copy_from_slice(b"ok\n");
                Ok(3)
            }
            _ => Ok(0),
        }
    }
}

#[test]
fn memory_reader_retries_interrupted_and_accepts_eof() {
    let output = Mutex::new(Output::new(info(), 100));
    read_output(InterruptedReader(0), &output);
    assert_eq!(lock(&output).scrollback.snapshot(), b"ok\n");
}

#[test]
fn conpty_startup_query_is_removed_but_application_queries_are_preserved() {
    for bytes in [
        b"\x1b[6nhello\x1b[6n".as_slice(),
        b"hello\x1b[6n",
        b"\x1b[",
        b"",
    ] {
        let output = Mutex::new(Output::new(info(), 100));
        // One-byte reads force every possible split of the startup query.
        struct ByteReader<'a>(&'a [u8]);
        impl Read for ByteReader<'_> {
            fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() {
                    return Ok(0);
                }
                dst[0] = self.0[0];
                self.0 = &self.0[1..];
                Ok(1)
            }
        }
        read_conpty_output(ByteReader(bytes), &output);
        assert_eq!(
            lock(&output).scrollback.snapshot(),
            bytes.strip_prefix(b"\x1b[6n").unwrap_or(bytes)
        );
    }
}

#[test]
fn silence_is_reset_by_output_and_reported_once_per_period() {
    let mut output = Output::new(info(), 100);
    let (_, mut rx) = output.attach();
    let start = Instant::now();
    let delay = Duration::from_millis(5);
    output.check_attention(start + delay, delay);
    assert!(
        rx.try_recv().is_err(),
        "no output must not produce attention"
    );
    output.publish(b"a", start);
    assert!(matches!(rx.try_recv().unwrap(), TermChunk::Data(_)));
    output.check_attention(start + Duration::from_millis(4), delay);
    assert!(rx.try_recv().is_err());
    output.publish(b"b", start + Duration::from_millis(4));
    rx.try_recv().unwrap();
    output.check_attention(start + delay, delay);
    assert!(rx.try_recv().is_err(), "timer must have reset");
    output.check_attention(start + Duration::from_millis(9), delay);
    assert_eq!(rx.try_recv().unwrap(), TermChunk::Attention);
    output.check_attention(start + Duration::from_secs(99), delay);
    assert!(rx.try_recv().is_err(), "same silence must only fire once");
    output.publish(b"c", start + Duration::from_secs(100));
    rx.try_recv().unwrap();
    output.check_attention(start + Duration::from_secs(100) + delay, delay);
    assert_eq!(rx.try_recv().unwrap(), TermChunk::Attention);
    output.exit(Some(0), start + Duration::from_secs(101));
    assert_eq!(rx.try_recv().unwrap(), TermChunk::Exit(Some(0)));
    output.check_attention(start + Duration::from_secs(200), delay);
    assert_eq!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed));
}

#[test]
fn late_attach_replays_exit_without_duplicating_existing_exit() {
    let mut output = Output::new(info(), 100);
    let (_, mut existing) = output.attach();
    output.publish(b"last\n", Instant::now());
    output.exit(Some(7), Instant::now());
    output.exit(Some(8), Instant::now());
    let (snapshot, mut late) = output.attach();
    assert_eq!(snapshot, b"last\n");
    assert!(matches!(existing.try_recv().unwrap(), TermChunk::Data(_)));
    assert_eq!(existing.try_recv().unwrap(), TermChunk::Exit(Some(7)));
    assert_eq!(
        existing.try_recv(),
        Err(broadcast::error::TryRecvError::Closed)
    );
    assert_eq!(late.try_recv().unwrap(), TermChunk::Exit(Some(7)));
    assert_eq!(late.try_recv(), Err(broadcast::error::TryRecvError::Closed));
}

#[test]
fn attach_racing_publication_neither_loses_nor_repeats_bytes() {
    for _ in 0..32 {
        let output = Arc::new(Mutex::new(Output::new(info(), 100_000)));
        let gate = Arc::new(Barrier::new(2));
        let writer_output = Arc::clone(&output);
        let writer_gate = Arc::clone(&gate);
        let writer = thread::spawn(move || {
            writer_gate.wait();
            for n in 0..500 {
                lock(&writer_output).publish(format!("{n:04}\n").as_bytes(), Instant::now());
                thread::yield_now();
            }
            lock(&writer_output).exit(Some(0), Instant::now());
        });
        gate.wait();
        let (mut combined, mut rx) = lock(&output).attach();
        writer.join().unwrap();
        while let Ok(chunk) = rx.try_recv() {
            if let TermChunk::Data(data) = chunk {
                combined.extend(data);
            }
        }
        let expected: String = (0..500).map(|n| format!("{n:04}\n")).collect();
        assert_eq!(combined, expected.as_bytes());
        assert_eq!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed));
    }
}

#[derive(Debug, Clone)]
struct MemoryChild {
    running: Arc<AtomicBool>,
    code: u32,
}

impl portable_pty::ChildKiller for MemoryChild {
    fn kill(&mut self) -> io::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

impl Child for MemoryChild {
    fn try_wait(&mut self) -> io::Result<Option<portable_pty::ExitStatus>> {
        Ok((!self.running.load(Ordering::SeqCst))
            .then(|| portable_pty::ExitStatus::with_exit_code(self.code)))
    }
    fn wait(&mut self) -> io::Result<portable_pty::ExitStatus> {
        while self.running.load(Ordering::SeqCst) {
            thread::yield_now();
        }
        Ok(portable_pty::ExitStatus::with_exit_code(self.code))
    }
    fn process_id(&self) -> Option<u32> {
        None
    }
    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

struct MemoryReader(mpsc::Receiver<Vec<u8>>);

impl Read for MemoryReader {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        match self.0.recv() {
            Ok(data) => {
                assert!(data.len() <= dst.len());
                dst[..data.len()].copy_from_slice(&data);
                Ok(data.len())
            }
            Err(_) => Ok(0),
        }
    }
}

#[test]
fn in_memory_process_kill_drains_reader_before_single_exit() {
    let output = Arc::new(Mutex::new(Output::new(info(), 100)));
    let (_, mut rx) = lock(&output).attach();
    let child = MemoryChild {
        running: Arc::new(AtomicBool::new(true)),
        code: 9,
    };
    let running = Arc::clone(&child.running);
    let terminal = Arc::new(Terminal {
        output: Arc::clone(&output),
        control: Mutex::new(None),
        child: Mutex::new(Some(Box::new(child))),
    });
    let (tx, reader) = mpsc::channel();
    let reader_output = Arc::clone(&output);
    let reader = thread::spawn(move || read_output(MemoryReader(reader), &reader_output));
    let worker_terminal = Arc::clone(&terminal);
    let worker = thread::spawn(move || supervise(worker_terminal, reader, Timing::default()));
    terminal.terminate().unwrap();
    assert!(!running.load(Ordering::SeqCst));
    assert!(
        lock(&output).info.running,
        "exit must await the pending reader"
    );
    tx.send(b"final bytes\n".to_vec()).unwrap();
    drop(tx);
    worker.join().unwrap();
    assert!(!lock(&output).info.running);
    assert!(matches!(rx.try_recv().unwrap(), TermChunk::Data(_)));
    assert_eq!(rx.try_recv().unwrap(), TermChunk::Exit(Some(9)));
    assert_eq!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed));
    terminal.terminate().unwrap();
}

#[test]
fn retention_reaps_only_exited_terminals_at_deadline() {
    let manager = TermManager::new(100);
    let now = Instant::now();
    for id in ["live", "exited"] {
        let mut state = Output::new(info(), 100);
        state.info.id = id.into();
        if id == "exited" {
            state.exit(Some(0), now);
        }
        lock(&manager.inner.terminals).insert(
            id.into(),
            Arc::new(Terminal {
                output: Arc::new(Mutex::new(state)),
                control: Mutex::new(None),
                child: Mutex::new(None),
            }),
        );
    }
    manager.inner.reap(now + Duration::from_secs(599));
    assert_eq!(manager.list().len(), 2);
    manager.inner.reap(now + Duration::from_secs(600));
    assert_eq!(manager.list().len(), 1);
    assert_eq!(manager.list()[0].id, "live");
}

#[test]
fn rejects_invalid_commands_sizes_and_unknown_ids() {
    let manager = TermManager::new(0);
    let cwd = std::env::current_dir().unwrap();
    for command in [
        vec![],
        vec![String::new()],
        vec!["tether-certainly-not-a-program-92837".into()],
        vec!["a\0b".into()],
    ] {
        assert!(manager.open(command, cwd.clone(), 80, 24).is_err());
    }
    assert!(pty_size(0, 24).is_err());
    assert!(pty_size(80, 0).is_err());
    #[cfg(windows)]
    assert!(pty_size(u16::MAX, 24).is_err());
    assert!(manager.attach("missing").is_err());
    assert!(manager.input("missing", b"x").is_err());
    assert!(manager.resize("missing", 80, 24).is_err());
    assert!(manager.kill("missing").is_err());
    assert!(manager.list().is_empty());
}

#[tokio::test]
async fn injected_short_silence_and_automatic_retention() {
    let manager = TermManager::with_timing(
        100_000,
        Timing {
            attention_after: Duration::from_millis(80),
            retain_exited: Duration::from_millis(100),
            poll: Duration::from_millis(2),
        },
    );
    #[cfg(windows)]
    let command = vec!["cmd.exe", "/d", "/q", "/c", "echo ready & set /p reply="];
    #[cfg(unix)]
    let command = vec!["sh", "-c", "printf 'ready\n'; read x"];
    let term = manager
        .open(
            command.into_iter().map(str::to_owned).collect(),
            std::env::current_dir().unwrap(),
            80,
            24,
        )
        .unwrap();
    let (_, mut rx) = manager.attach(&term.id).unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut seen = Vec::new();
        let mut pending_bells = 0_usize;
        loop {
            match rx.recv().await.unwrap() {
                TermChunk::Data(data) => {
                    pending_bells += usize::from(data.contains(&7));
                    seen.extend(data);
                }
                TermChunk::Attention if pending_bells > 0 => pending_bells -= 1,
                TermChunk::Attention if String::from_utf8_lossy(&seen).contains("ready") => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    // Let the child exit naturally so it remains registered for the reaper.
    #[cfg(windows)]
    manager.input(&term.id, b"x\r").unwrap();
    #[cfg(unix)]
    manager.input(&term.id, b"x\n").unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if matches!(rx.recv().await.unwrap(), TermChunk::Exit(_)) {
                break;
            }
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        // Inspect the map directly: cleanup must work with no list/attach call.
        while !lock(&manager.inner.terminals).is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

//! PTY terminals for running any CLI from the phone (ConPTY on Windows).
//!
//! The public API is synchronous and does not require a Tokio runtime. PTY
//! reads and process supervision run on dedicated OS threads. Consumers must
//! handle broadcast lag (for example, by attaching again for a fresh snapshot).

mod scrollback;

use std::{
    collections::HashMap,
    fmt,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, ensure};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tether_proto::TermInfo;
use tokio::sync::broadcast;

use scrollback::Scrollback;

/// Default scrollback kept per terminal.
pub const DEFAULT_SCROLLBACK: usize = 512 << 10;
const BROADCAST_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum TermChunk {
    Data(bytes::Bytes),
    /// A bell, or silence after output for longer than the attention delay
    /// (8 s): the program may be waiting for the user.
    Attention,
    Exit(Option<i32>),
}

#[derive(Debug, Clone, Copy)]
struct Timing {
    attention_after: Duration,
    retain_exited: Duration,
    poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            attention_after: Duration::from_secs(8),
            retain_exited: Duration::from_secs(10 * 60),
            poll: Duration::from_millis(20),
        }
    }
}

// No user callback runs under these locks. Recovering a poisoned lock also
// allows Drop to clean up a child if another thread has unexpectedly panicked.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

#[derive(Debug)]
struct Output {
    info: TermInfo,
    scrollback: Scrollback,
    tx: Option<broadcast::Sender<TermChunk>>,
    last_output: Option<Instant>,
    silence_reported: bool,
    exited_at: Option<Instant>,
    exit_code: Option<i32>,
}

impl Output {
    fn new(info: TermInfo, capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            info,
            scrollback: Scrollback::new(capacity),
            tx: Some(tx),
            last_output: None,
            silence_reported: false,
            exited_at: None,
            exit_code: None,
        }
    }

    fn send(&self, chunk: TermChunk) {
        if let Some(tx) = &self.tx {
            // No listeners is normal: scrollback still records the output.
            let _ = tx.send(chunk);
        }
    }

    fn publish(&mut self, data: &[u8], now: Instant) {
        if data.is_empty() || !self.info.running {
            return;
        }
        self.scrollback.push(data);
        self.last_output = Some(now);
        self.silence_reported = false;
        // Mutation and publication share the SAME mutex as attach().
        self.send(TermChunk::Data(bytes::Bytes::copy_from_slice(data)));
        if data.contains(&b'\x07') {
            self.send(TermChunk::Attention);
        }
    }

    fn attach(&self) -> (Vec<u8>, broadcast::Receiver<TermChunk>) {
        let snapshot = self.scrollback.snapshot();
        let rx = if let Some(tx) = &self.tx {
            tx.subscribe()
        } else {
            // Late attachment to an exited terminal also receives its status,
            // without rebroadcasting a duplicate Exit to existing subscribers.
            let (tx, rx) = broadcast::channel(1);
            let _ = tx.send(TermChunk::Exit(self.exit_code));
            rx
        };
        (snapshot, rx)
    }

    fn check_attention(&mut self, now: Instant, after: Duration) {
        if self.info.running
            && !self.silence_reported
            && self
                .last_output
                .is_some_and(|last| now.saturating_duration_since(last) >= after)
        {
            self.silence_reported = true;
            self.send(TermChunk::Attention);
        }
    }

    fn exit(&mut self, code: Option<i32>, now: Instant) {
        if !self.info.running {
            return;
        }
        self.info.running = false;
        self.exited_at = Some(now);
        self.exit_code = code;
        self.send(TermChunk::Exit(code));
        self.tx.take();
    }
}

struct Control {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
}

struct Terminal {
    output: Arc<Mutex<Output>>,
    control: Mutex<Option<Control>>,
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
}

impl fmt::Debug for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Terminal")
            .field("info", &lock(&self.output).info)
            .finish_non_exhaustive()
    }
}

impl Terminal {
    fn terminate(&self) -> Result<()> {
        if let Some(child) = lock(&self.child).as_mut()
            && child
                .try_wait()
                .context("query terminal process")?
                .is_none()
        {
            child.kill().context("kill terminal process")?;
        }
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Also covers failure to start the supervisor thread in open(). The
        // reader is already draining, so closing ConPTY cannot fill its pipe.
        if let Some(mut child) = lock(&self.child).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let control = lock(&self.control).take();
        drop(control);
    }
}

#[derive(Debug)]
struct Manager {
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    reaper_started: Mutex<bool>,
    scrollback_bytes: usize,
    timing: Timing,
}

impl Manager {
    fn reap(&self, now: Instant) {
        lock(&self.terminals).retain(|_, terminal| {
            !lock(&terminal.output).exited_at.is_some_and(|exited| {
                now.saturating_duration_since(exited) >= self.timing.retain_exited
            })
        });
    }

    fn start_reaper(self: &Arc<Self>) -> Result<()> {
        let mut started = lock(&self.reaper_started);
        if !*started {
            let manager = Arc::downgrade(self);
            let interval = self
                .timing
                .retain_exited
                .clamp(Duration::from_millis(1), Duration::from_secs(1));
            thread::Builder::new()
                .name("tether-pty-reaper".into())
                .spawn(move || {
                    loop {
                        // Never keep the manager alive during the sleep.
                        thread::sleep(interval);
                        let Some(manager) = manager.upgrade() else {
                            break;
                        };
                        manager.reap(Instant::now());
                    }
                })
                .context("start terminal retention worker")?;
            *started = true;
        }
        Ok(())
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        for terminal in lock(&self.terminals).values() {
            if let Err(error) = terminal.terminate() {
                tracing::warn!(%error, "could not stop terminal during manager shutdown");
            }
        }
    }
}

#[derive(Debug)]
pub struct TermManager {
    inner: Arc<Manager>,
}

impl TermManager {
    pub fn new(scrollback_bytes: usize) -> TermManager {
        Self::with_timing(scrollback_bytes, Timing::default())
    }

    // Private injection point keeps the frozen public contract unchanged.
    fn with_timing(scrollback_bytes: usize, timing: Timing) -> Self {
        Self {
            inner: Arc::new(Manager {
                terminals: Mutex::new(HashMap::new()),
                reaper_started: Mutex::new(false),
                scrollback_bytes,
                timing,
            }),
        }
    }

    pub fn open(
        &self,
        command: Vec<String>,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    ) -> Result<TermInfo> {
        let size = pty_size(cols, rows)?;
        let cwd = if cwd.is_absolute() {
            cwd
        } else {
            std::env::current_dir()
                .context("get current directory")?
                .join(cwd)
        };
        ensure!(
            cwd.is_dir(),
            "terminal cwd is not a directory: {}",
            cwd.display()
        );
        let cwd_text = cwd.to_str().context("terminal cwd must be UTF-8")?;
        ensure!(!cwd_text.contains('\0'), "terminal cwd contains NUL");
        let builder = build_command(&command, &cwd)?;
        self.inner.start_reaper()?;

        let pair = native_pty_system().openpty(size).context("open PTY")?;
        let reader = pair.master.try_clone_reader().context("clone PTY reader")?;
        let writer = pair.master.take_writer().context("take PTY writer")?;
        #[cfg(windows)]
        let writer = {
            // portable-pty 0.9 enables INHERIT_CURSOR. A fresh remote terminal
            // has no previous cursor to inherit; answer its initial DSR before
            // a phone attaches, otherwise even `cmd /c echo` waits forever.
            let mut writer = writer;
            writer
                .write_all(b"\x1b[1;1R")
                .context("initialize ConPTY cursor")?;
            writer.flush().context("flush ConPTY cursor reply")?;
            writer
        };
        let info = TermInfo {
            id: uuid::Uuid::new_v4().to_string(),
            command,
            cwd: cwd_text.to_owned(),
            cols,
            rows,
            running: true,
            created_at: tether_proto::now_ms(),
        };
        let output = Arc::new(Mutex::new(Output::new(
            info.clone(),
            self.inner.scrollback_bytes,
        )));

        // Start draining BEFORE spawning: this also makes failure cleanup of
        // ClosePseudoConsole safe. No blocking Read runs on a Tokio worker.
        let reader_output = Arc::clone(&output);
        let reader_thread = thread::Builder::new()
            .name(format!("tether-pty-read-{}", info.id))
            .spawn(move || {
                #[cfg(windows)]
                read_conpty_output(reader, &reader_output);
                #[cfg(not(windows))]
                read_output(reader, &reader_output);
            })
            .context("start PTY reader thread")?;
        let child = pair
            .slave
            .spawn_command(builder)
            .context("spawn terminal command");
        drop(pair.slave);
        let child = child?;
        let terminal = Arc::new(Terminal {
            output,
            control: Mutex::new(Some(Control {
                master: pair.master,
                writer,
            })),
            child: Mutex::new(Some(child)),
        });
        let supervisor_terminal = Arc::clone(&terminal);
        let timing = self.inner.timing;
        thread::Builder::new()
            .name(format!("tether-pty-wait-{}", info.id))
            .spawn(move || supervise(supervisor_terminal, reader_thread, timing))
            .context("start terminal supervisor thread")?;
        lock(&self.inner.terminals).insert(info.id.clone(), terminal);
        Ok(info)
    }

    fn terminal(&self, id: &str) -> Result<Arc<Terminal>> {
        self.inner.reap(Instant::now());
        lock(&self.inner.terminals)
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("terminal not found: {id}"))
    }

    /// Snapshot and subscription are taken under the publication mutex.
    pub fn attach(&self, term: &str) -> Result<(Vec<u8>, broadcast::Receiver<TermChunk>)> {
        let terminal = self.terminal(term)?;
        Ok(lock(&terminal.output).attach())
    }

    pub fn input(&self, term: &str, data: &[u8]) -> Result<()> {
        let terminal = self.terminal(term)?;
        let mut control = lock(&terminal.control);
        let control = control.as_mut().context("terminal has exited")?;
        control
            .writer
            .write_all(data)
            .context("write terminal input")?;
        control.writer.flush().context("flush terminal input")
    }

    pub fn resize(&self, term: &str, cols: u16, rows: u16) -> Result<()> {
        let size = pty_size(cols, rows)?;
        let terminal = self.terminal(term)?;
        let control = lock(&terminal.control);
        let control = control.as_ref().context("terminal has exited")?;
        control.master.resize(size).context("resize PTY")?;
        let mut output = lock(&terminal.output);
        output.info.cols = cols;
        output.info.rows = rows;
        Ok(())
    }

    pub fn kill(&self, term: &str) -> Result<()> {
        let terminal = self.terminal(term)?;
        terminal.terminate()?;
        lock(&self.inner.terminals).remove(term);
        Ok(())
    }

    pub fn list(&self) -> Vec<TermInfo> {
        self.inner.reap(Instant::now());
        let mut terms: Vec<_> = lock(&self.inner.terminals)
            .values()
            .map(|terminal| lock(&terminal.output).info.clone())
            .collect();
        terms.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        terms
    }
}

fn pty_size(cols: u16, rows: u16) -> Result<PtySize> {
    ensure!(cols > 0 && rows > 0, "terminal dimensions must be nonzero");
    #[cfg(windows)]
    ensure!(
        cols <= i16::MAX as u16 && rows <= i16::MAX as u16,
        "ConPTY dimensions must fit a positive COORD"
    );
    Ok(PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn build_command(command: &[String], cwd: &Path) -> Result<CommandBuilder> {
    let program = command
        .first()
        .filter(|s| !s.is_empty())
        .context("command is empty")?;
    ensure!(
        command.iter().all(|s| !s.contains('\0')),
        "command contains NUL"
    );
    let executable = which::which_in(program, std::env::var_os("PATH"), cwd)
        .with_context(|| format!("resolve terminal command {program:?}"))?;
    let mut builder = platform_command(&executable, &command[1..], cwd)?;
    builder.cwd(cwd);
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    Ok(builder)
}

#[cfg(not(windows))]
fn platform_command(program: &Path, args: &[String], _cwd: &Path) -> Result<CommandBuilder> {
    let mut builder = CommandBuilder::new(program);
    builder.args(args);
    Ok(builder)
}

#[cfg(windows)]
fn platform_command(program: &Path, args: &[String], cwd: &Path) -> Result<CommandBuilder> {
    let batch = program
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if !batch {
        let mut builder = CommandBuilder::new(program);
        builder.args(args);
        return Ok(builder);
    }

    // CreateProcessW cannot execute batch files. Use cmd, but do not feed a
    // command string through portable-pty's CRT-style quote escaping (\").
    // A single environment expansion preserves spaced/Unicode paths and keeps
    // percent signs literal; /d disables AutoRun and /v:off preserves '!'.
    let shell = which::which_in("cmd.exe", std::env::var_os("PATH"), cwd)
        .context("resolve cmd.exe for batch command")?;
    let key = format!("TETHER_BATCH_{}", uuid::Uuid::new_v4().simple());
    let path = program.to_str().context("batch path must be UTF-8")?;
    ensure!(!path.contains(['"', '\r', '\n']), "invalid batch path");
    let mut line = format!("\"{path}\"");
    for arg in args {
        line.push(' ');
        line.push_str(&quote_batch_arg(arg)?);
    }
    let mut builder = CommandBuilder::new(shell);
    builder.args(["/d", "/v:off", "/s", "/c"]);
    builder.arg(format!("%{key}%"));
    builder.env(key, line);
    // portable-pty attaches STARTUPINFOEX to a pseudoconsole, not a visible
    // console window. It does not expose std::process creation_flags.
    Ok(builder)
}

#[cfg(windows)]
fn quote_batch_arg(arg: &str) -> Result<String> {
    // Keep cmd metacharacters inside quotes even for embedded literal quotes.
    // Doubled quotes work for CRT argv parsing without cmd's broken \" rules.
    // Batch files that forward %* (e.g. npm shims) then receive exact argv,
    // including literal quotes and trailing backslashes.
    ensure!(
        !arg.contains(['\r', '\n']),
        "batch arguments cannot contain newlines"
    );
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for ch in arg.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        quoted.extend(std::iter::repeat_n(
            '\\',
            if ch == '"' { slashes * 2 } else { slashes },
        ));
        slashes = 0;
        if ch == '"' {
            quoted.push_str("\"\"");
        } else {
            quoted.push(ch);
        }
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('"');
    Ok(quoted)
}

// Consume only the initial host DSR, including when it is split across reads.
// A phone must not answer a replayed startup query that we already answered;
// application-generated queries later in the stream remain untouched.
#[cfg(any(windows, test))]
fn read_conpty_output(mut reader: impl Read, output: &Mutex<Output>) {
    const QUERY: &[u8] = b"\x1b[6n";
    let mut prefix = [0; 4];
    let mut count = 0;
    while count < QUERY.len() {
        match reader.read(&mut prefix[count..]) {
            Ok(0) => break,
            Ok(n) => {
                count += n;
                if prefix[..count] != QUERY[..count] {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                tracing::debug!(%error, "ConPTY startup reader stopped");
                break;
            }
        }
    }
    if prefix[..count] != *QUERY {
        lock(output).publish(&prefix[..count], Instant::now());
    }
    read_output(reader, output);
}

fn read_output(mut reader: impl Read, output: &Mutex<Output>) {
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => lock(output).publish(&buffer[..count], Instant::now()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                // EIO on Unix / broken pipe on Windows can also mean PTY EOF.
                tracing::debug!(%error, "PTY output reader stopped");
                break;
            }
        }
    }
}

fn supervise(terminal: Arc<Terminal>, reader: thread::JoinHandle<()>, timing: Timing) {
    let code = loop {
        let status = {
            let mut child = lock(&terminal.child);
            match child.as_mut() {
                Some(child) => match child.try_wait() {
                    Ok(status) => status,
                    Err(error) => {
                        tracing::warn!(%error, "could not query terminal process");
                        let _ = child.kill();
                        break child.wait().ok().and_then(exit_code);
                    }
                },
                None => break None,
            }
        };
        if let Some(status) = status {
            break exit_code(status);
        }
        lock(&terminal.output).check_attention(Instant::now(), timing.attention_after);
        thread::sleep(timing.poll);
    };
    lock(&terminal.child).take();

    // Closing ConPTY can block while draining output. Never hold the output
    // mutex here. The reader must remain alive until master/writer are closed.
    let control = lock(&terminal.control).take();
    drop(control);
    if reader.join().is_err() {
        tracing::error!("PTY reader thread panicked");
    }
    // Data (including the final bytes) always precedes exactly one Exit.
    lock(&terminal.output).exit(code, Instant::now());
}

fn exit_code(status: portable_pty::ExitStatus) -> Option<i32> {
    status
        .signal()
        .is_none()
        .then_some(status.exit_code() as i32)
}

#[cfg(test)]
mod tests;

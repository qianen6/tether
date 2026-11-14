//! Where each agent's program is, when the person says so: typed in on the
//! desktop app for an agent Tether cannot find (installed outside PATH) or
//! should not use as found. A location is checked before it is kept (the file
//! is there, looks like that program, and answers `--version`), saved in
//! `locations.json`, and applies at once: every agent is wrapped in
//! [`Located`], which hands the chosen program to its adapter and its
//! terminal.
//!
//! DeepSeek Harness's own launcher logic takes its location
//! ([`tether_adapter_dsh::set_saved_launcher`]): its terminal runs a client
//! script beside DSH, not the launcher itself. For DSH the person may instead
//! give the address a running `dsh web` printed
//! ([`tether_adapter_dsh::set_saved_url`]), to use that one.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, bail};
use tether_agent_api::{
    AgentAdapter, AgentCommand, AgentOutput, BoxFuture, HistoryQuery, HistoryRead, ModelQuery,
    Permissions, SpawnOptions,
};
use tether_proto::{AgentInfo, AgentKind, HistoryData, HistoryReadData, ModelInfo};
use tokio::sync::mpsc::UnboundedSender;

pub(crate) const FILE: &str = "locations.json";

/// `--version` answers at once; a first run after an update may be slower.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// A location that passed the check.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramCheck {
    pub program: PathBuf,
    /// The first line `--version` printed; none for a shell (not run).
    pub version: Option<String>,
}

/// How an agent's location is checked and applied.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    /// An agent program: its file name has `name` in it, and it answers
    /// `--version`.
    Program { name: String },
    /// A shell or other terminal program: any program will do.
    Terminal,
    /// The built-in DeepSeek Harness: its launcher, checked by its adapter.
    Dsh,
}

/// The locations chosen, and which agents can have one.
pub(crate) struct Locations {
    file: Option<PathBuf>,
    chosen: RwLock<BTreeMap<String, PathBuf>>,
    kinds: RwLock<HashMap<String, Kind>>,
}

impl Locations {
    /// From `dir/locations.json`; a file that cannot be read is left alone
    /// and replaced at the next change.
    pub(crate) fn load(dir: &Path) -> Self {
        let file = dir.join(FILE);
        let chosen = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read {}", file.display());
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        };
        Self {
            file: Some(file),
            chosen: RwLock::new(chosen),
            kinds: RwLock::default(),
        }
    }

    pub(crate) fn get(&self, agent: &str) -> Option<PathBuf> {
        self.chosen.read().ok()?.get(agent).cloned()
    }

    /// Agent id → chosen program, for every agent that can have one (none
    /// chosen: `None`).
    pub(crate) fn all(&self) -> Vec<(String, Option<PathBuf>)> {
        let kinds = match self.kinds.read() {
            Ok(k) => k.keys().cloned().collect::<Vec<_>>(),
            Err(_) => return Vec::new(),
        };
        kinds
            .into_iter()
            .map(|id| (id.clone(), self.get(&id)))
            .collect()
    }

    fn kind(&self, agent: &str) -> Option<Kind> {
        self.kinds.read().ok()?.get(agent).cloned()
    }

    fn register(&self, agent: &str, kind: Kind) {
        if kind == Kind::Dsh {
            apply_dsh(self.get(agent));
        }
        if let Ok(mut kinds) = self.kinds.write() {
            kinds.insert(agent.to_owned(), kind);
        }
    }

    fn put(&self, agent: &str, program: Option<PathBuf>) -> anyhow::Result<()> {
        let mut next = self.chosen.read().map_err(poisoned)?.clone();
        match &program {
            Some(p) => next.insert(agent.to_owned(), p.clone()),
            None => next.remove(agent),
        };
        if let Some(file) = &self.file {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let temporary = file.with_extension("json.tmp");
            std::fs::write(&temporary, serde_json::to_vec_pretty(&next)?)?;
            std::fs::rename(&temporary, file)
                .with_context(|| format!("保存 {}", file.display()))?;
        }
        *self.chosen.write().map_err(poisoned)? = next;
        if self.kind(agent) == Some(Kind::Dsh) {
            apply_dsh(program);
        }
        Ok(())
    }

    /// Checks `input` as `agent`'s program and keeps it.
    pub(crate) async fn set(&self, agent: &str, input: &str) -> anyhow::Result<ProgramCheck> {
        let kind = self
            .kind(agent)
            .with_context(|| format!("{agent} 没有可以选择的程序位置"))?;
        let checked = check(&kind, input).await?;
        self.put(agent, Some(checked.program.clone()))?;
        Ok(checked)
    }

    /// Forgets `agent`'s location: Tether finds the program by itself again.
    pub(crate) fn clear(&self, agent: &str) -> anyhow::Result<()> {
        self.put(agent, None)
    }
}

/// DSH's choice is its launcher, or the address of a `dsh web` already
/// running (kept in the same place, told apart by `http`).
fn apply_dsh(chosen: Option<PathBuf>) {
    let text = chosen.as_ref().map(|p| p.to_string_lossy().into_owned());
    match text {
        Some(url) if tether_adapter_dsh::is_url(&url) => {
            tether_adapter_dsh::set_saved_launcher(None);
            tether_adapter_dsh::set_saved_url(Some(url));
        }
        _ => {
            tether_adapter_dsh::set_saved_url(None);
            tether_adapter_dsh::set_saved_launcher(chosen);
        }
    }
}

fn poisoned<T>(_: T) -> anyhow::Error {
    anyhow::anyhow!("位置设置的锁已损坏")
}

/// `inner`, using the location chosen for it, if it can have one.
pub(crate) fn locate(
    inner: Arc<dyn AgentAdapter>,
    locations: &Arc<Locations>,
) -> Arc<dyn AgentAdapter> {
    let info = inner.info();
    let Some(kind) = kind_of(&info) else {
        return inner;
    };
    locations.register(&info.id, kind.clone());
    Arc::new(Located {
        id: info.id,
        kind,
        inner,
        locations: locations.clone(),
    })
}

/// What can be chosen for an agent: its program, unless it has none (the
/// echo agent) or is configured in `agents.toml` as DSH.
pub(crate) fn kind_of(info: &AgentInfo) -> Option<Kind> {
    match info.kind {
        AgentKind::Echo => None,
        AgentKind::Dsh if info.id == "dsh" => Some(Kind::Dsh),
        AgentKind::Dsh => None,
        AgentKind::Terminal => Some(Kind::Terminal),
        _ => {
            let first = info.terminal_command.first()?;
            Some(Kind::Program {
                name: stem(Path::new(first))?,
            })
        }
    }
}

/// `C:\…\Codex.EXE` → `codex`.
fn stem(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().to_ascii_lowercase();
    (!stem.is_empty()).then_some(stem)
}

/// The program `input` names for an agent of `kind`, checked.
pub(crate) async fn check(kind: &Kind, input: &str) -> anyhow::Result<ProgramCheck> {
    if *kind == Kind::Dsh && tether_adapter_dsh::is_url(input) {
        let url = tether_adapter_dsh::check_url(input).await?;
        let origin = url.split('?').next().unwrap_or(&url).trim_end_matches('/');
        return Ok(ProgramCheck {
            program: PathBuf::from(&url),
            version: Some(format!("正在运行的 dsh web（{origin}）")),
        });
    }
    if *kind == Kind::Dsh {
        let checked = tether_adapter_dsh::check_launcher(input).await?;
        return Ok(ProgramCheck {
            program: checked.launcher,
            version: Some(checked.version),
        });
    }
    let program = program_at(kind, input)?;
    let version = match kind {
        Kind::Program { .. } => Some(version_of(&program).await?),
        _ => None,
    };
    Ok(ProgramCheck { program, version })
}

/// Extensions Windows runs directly.
const RUNNABLE: &[&str] = &["exe", "cmd", "bat", "com"];

fn runnable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if cfg!(windows) {
        return path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| RUNNABLE.contains(&e.to_ascii_lowercase().as_str()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
    }
    #[allow(unreachable_code)]
    true
}

/// The file `input` names, or in a folder the program called `name` (also
/// under `bin` and one folder down).
pub(crate) fn program_at(kind: &Kind, input: &str) -> anyhow::Result<PathBuf> {
    // Explorer's "复制为路径" puts quotes around it.
    let text = input.trim().trim_matches(['"', '\'']).trim();
    if text.is_empty() {
        bail!("请填写程序的位置");
    }
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        bail!("请填写完整路径，例如 {}", example(kind));
    }
    if !path.exists() {
        bail!("找不到 {}", path.display());
    }
    let name = match kind {
        Kind::Program { name } => Some(name.as_str()),
        _ => None,
    };
    if path.is_dir() {
        let Some(name) = name else {
            bail!("请选程序文件本身，而不是文件夹");
        };
        return in_folder(&path, name).with_context(|| {
            format!(
                "{} 里没有找到 {name}（也看了 bin 和下一层文件夹）",
                path.display()
            )
        });
    }
    if !runnable(&path) {
        bail!(
            "{} 不是可以运行的程序{}",
            path.display(),
            if cfg!(windows) {
                "（应为 .exe、.cmd 或 .bat）"
            } else {
                "（没有执行权限）"
            }
        );
    }
    if let Some(name) = name {
        let got = stem(&path).unwrap_or_default();
        if !got.contains(name) {
            bail!(
                "{} 看起来不是 {name}：文件名里没有 “{name}”",
                path.display()
            );
        }
    }
    Ok(path)
}

fn in_folder(dir: &Path, name: &str) -> Option<PathBuf> {
    let candidates = |d: &Path| -> Option<PathBuf> {
        let names: Vec<String> = if cfg!(windows) {
            RUNNABLE.iter().map(|e| format!("{name}.{e}")).collect()
        } else {
            vec![name.to_owned()]
        };
        names.iter().map(|n| d.join(n)).find(|p| runnable(p))
    };
    candidates(dir)
        .or_else(|| candidates(&dir.join("bin")))
        .or_else(|| {
            let mut subs: Vec<PathBuf> = std::fs::read_dir(dir)
                .ok()?
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect();
            subs.sort();
            subs.iter()
                .find_map(|d| candidates(d).or_else(|| candidates(&d.join("bin"))))
        })
}

fn example(kind: &Kind) -> String {
    let name = match kind {
        Kind::Program { name } => name.as_str(),
        Kind::Terminal => "pwsh",
        Kind::Dsh => "dsh",
    };
    if cfg!(windows) {
        format!(r"C:\Users\me\AppData\Roaming\npm\{name}.cmd")
    } else {
        format!("/usr/local/bin/{name}")
    }
}

/// What `program --version` says: its first line of output.
async fn version_of(program: &Path) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(home) = directories::BaseDirs::new() {
        cmd.current_dir(home.home_dir());
    }
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = tokio::time::timeout(CHECK_TIMEOUT, cmd.output())
        .await
        .with_context(|| {
            format!(
                "{} --version {} 秒内没有结束",
                program.display(),
                CHECK_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("运行 {} 失败", program.display()))?;
    let first = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(120).collect::<String>())
    };
    if !output.status.success() {
        let why = first(&output.stderr)
            .or_else(|| first(&output.stdout))
            .unwrap_or_else(|| "没有输出".into());
        bail!("运行 {} --version 出错：{why}", program.display());
    }
    first(&output.stdout)
        .or_else(|| first(&output.stderr))
        .with_context(|| format!("{} --version 没有任何输出", program.display()))
}

/// An agent that uses the program chosen for it, when there is one.
struct Located {
    id: String,
    kind: Kind,
    inner: Arc<dyn AgentAdapter>,
    locations: Arc<Locations>,
}

impl Located {
    /// The chosen program, for adapters that take one (not DSH: its adapter
    /// already has it).
    fn program(&self) -> Option<PathBuf> {
        match self.kind {
            Kind::Dsh => None,
            _ => self.locations.get(&self.id),
        }
    }

    fn or_chosen(&self, given: Option<PathBuf>) -> Option<PathBuf> {
        given.or_else(|| self.program())
    }
}

impl AgentAdapter for Located {
    fn info(&self) -> AgentInfo {
        let mut info = self.inner.info();
        match self.program() {
            Some(program) => {
                let there = program.is_file();
                info.available = there;
                info.detail = (!there).then(|| {
                    format!(
                        "填写的位置 {} 已经不存在了：请在电脑上重新选择（Tether 窗口的 Agent 列表，或运行 tether locate {} <位置>）",
                        program.display(),
                        self.id
                    )
                });
                let program = program.display().to_string();
                match info.terminal_command.first_mut() {
                    Some(first) => *first = program,
                    None => info.terminal_command = vec![program],
                }
            }
            None if !info.available && self.kind != Kind::Dsh => {
                let why = info.detail.take().unwrap_or_else(|| "未找到".into());
                info.detail = Some(format!(
                    "{why}。如果已经装了，可以在电脑上选择它的位置（Tether 窗口的 Agent 列表，或运行 tether locate {} <位置>）",
                    self.id
                ));
            }
            None => {}
        }
        info
    }

    fn spawn(
        &self,
        mut opts: SpawnOptions,
        out: UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
        opts.program = self.or_chosen(opts.program.take());
        self.inner.spawn(opts, out)
    }

    fn list_history(&self, mut query: HistoryQuery) -> BoxFuture<HistoryData> {
        query.program = self.or_chosen(query.program.take());
        self.inner.list_history(query)
    }

    fn read_history(&self, mut read: HistoryRead) -> BoxFuture<HistoryReadData> {
        read.program = self.or_chosen(read.program.take());
        self.inner.read_history(read)
    }

    fn list_models(&self, mut query: ModelQuery) -> BoxFuture<Vec<ModelInfo>> {
        query.program = self.or_chosen(query.program.take());
        self.inner.list_models(query)
    }

    fn list_permissions(&self, mut query: ModelQuery) -> BoxFuture<Permissions> {
        query.program = self.or_chosen(query.program.take());
        self.inner.list_permissions(query)
    }

    fn chat_dir(&self, hint: &str) -> anyhow::Result<Option<PathBuf>> {
        self.inner.chat_dir(hint)
    }

    fn reveal(&self, thread: &str) -> anyhow::Result<()> {
        self.inner.reveal(thread)
    }

    fn release_after(&self) -> Option<Duration> {
        self.inner.release_after()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program_kind(name: &str) -> Kind {
        Kind::Program { name: name.into() }
    }

    /// A fake program that prints `text` for `--version`.
    fn fake(dir: &Path, name: &str, text: &str) -> PathBuf {
        if cfg!(windows) {
            let path = dir.join(format!("{name}.cmd"));
            std::fs::write(&path, format!("@echo off\r\necho {text}\r\n")).unwrap();
            path
        } else {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\necho {text}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        }
    }

    #[test]
    fn kinds() {
        let info = |id: &str, kind: AgentKind, command: &[&str]| AgentInfo {
            id: id.into(),
            name: id.into(),
            kind,
            available: false,
            detail: None,
            chat: true,
            terminal_command: command.iter().map(|s| s.to_string()).collect(),
        };
        // A path as this system writes it: `\` only separates folders on Windows.
        let desktop = if cfg!(windows) {
            r"C:\Apps\Codex.EXE"
        } else {
            "/Applications/Codex.app/Contents/Resources/Codex"
        };
        assert_eq!(
            kind_of(&info("codex-desktop", AgentKind::Codex, &[desktop])),
            Some(program_kind("codex"))
        );
        assert_eq!(
            kind_of(&info("cursor", AgentKind::Acp, &["cursor-agent"])),
            Some(program_kind("cursor-agent"))
        );
        assert_eq!(
            kind_of(&info(
                "shell",
                AgentKind::Terminal,
                &["powershell.exe", "-NoLogo"]
            )),
            Some(Kind::Terminal)
        );
        assert_eq!(
            kind_of(&info("dsh", AgentKind::Dsh, &["dsh"])),
            Some(Kind::Dsh)
        );
        assert_eq!(kind_of(&info("dsh-work", AgentKind::Dsh, &["dsh"])), None);
        assert_eq!(kind_of(&info("echo", AgentKind::Echo, &[])), None);
    }

    #[test]
    fn what_is_typed() {
        let dir = tempfile::tempdir().unwrap();
        let codex = fake(dir.path(), "codex", "codex-cli 0.130.0");
        let claude = fake(dir.path(), "claude", "2.1.0 (Claude Code)");
        let kind = program_kind("codex");
        let error = |kind: &Kind, input: &str| program_at(kind, input).unwrap_err().to_string();

        let quoted = format!("\"{}\"", codex.display());
        assert_eq!(program_at(&kind, &quoted).unwrap(), codex);
        // A folder: the program in it.
        assert_eq!(
            program_at(&kind, &dir.path().display().to_string()).unwrap(),
            codex
        );
        assert!(error(&kind, "").contains("请填写"));
        assert!(error(&kind, "codex.exe").contains("完整路径"));
        assert!(
            error(&kind, &dir.path().join("gone.exe").display().to_string()).contains("找不到")
        );
        assert!(error(&kind, &claude.display().to_string()).contains("看起来不是 codex"));
        let empty = tempfile::tempdir().unwrap();
        assert!(error(&kind, &empty.path().display().to_string()).contains("没有找到 codex"));
        let text = dir.path().join("codex.txt");
        std::fs::write(&text, "").unwrap();
        assert!(error(&kind, &text.display().to_string()).contains("不是可以运行的程序"));
        // Any shell will do, but a file, not a folder.
        assert_eq!(
            program_at(&Kind::Terminal, &claude.display().to_string()).unwrap(),
            claude
        );
        assert!(error(&Kind::Terminal, &dir.path().display().to_string()).contains("文件本身"));
    }

    #[tokio::test]
    async fn checked_by_running_it() {
        let dir = tempfile::tempdir().unwrap();
        let codex = fake(dir.path(), "codex", "codex-cli 0.130.0");
        let checked = check(&program_kind("codex"), &codex.display().to_string())
            .await
            .unwrap();
        assert_eq!(checked.version.as_deref(), Some("codex-cli 0.130.0"));

        let broken = if cfg!(windows) {
            let p = dir.path().join("codex-broken.cmd");
            std::fs::write(&p, "@echo off\r\necho no such option 1>&2\r\nexit /b 2\r\n").unwrap();
            p
        } else {
            let p = dir.path().join("codex-broken");
            std::fs::write(&p, "#!/bin/sh\necho no such option >&2\nexit 2\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            p
        };
        let error = check(&program_kind("codex"), &broken.display().to_string())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("no such option"), "{error}");
    }

    #[tokio::test]
    async fn dsh_takes_a_running_dsh_web() {
        // Nothing listens on port 1: refused, said in words, nothing kept.
        let error = check(&Kind::Dsh, "dsh web: http://127.0.0.1:1/?token=abc")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("127.0.0.1:1"), "{error:#}");
        let error = check(&Kind::Dsh, "http://").await.unwrap_err();
        assert!(error.to_string().contains("http://"), "{error}");
        // A path is still a launcher.
        let error = check(&Kind::Dsh, "dsh.cmd").await.unwrap_err();
        assert!(error.to_string().contains("完整路径"), "{error}");
    }

    #[tokio::test]
    async fn kept_and_applied_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let codex = fake(dir.path(), "codex", "codex-cli 0.130.0");
        let locations = Arc::new(Locations::load(data.path()));
        let agent = locate(Arc::new(Missing), &locations);
        assert!(!agent.info().available);
        assert!(agent.info().detail.unwrap().contains("选择它的位置"));
        assert_eq!(locations.all(), vec![("missing".to_string(), None)]);

        assert!(locations.set("missing", "C:\\nope").await.is_err());
        assert_eq!(
            locations.get("missing"),
            None,
            "nothing kept when the check fails"
        );
        locations
            .set("missing", &codex.display().to_string())
            .await
            .unwrap();
        let info = agent.info();
        assert!(info.available);
        assert_eq!(
            info.terminal_command,
            vec![codex.display().to_string(), "--flag".into()]
        );

        // The adapter is handed the program, unless the caller names one.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = agent
            .spawn(SpawnOptions::default(), tx)
            .unwrap_err()
            .to_string();
        assert_eq!(error, format!("spawned {}", codex.display()));

        // Kept for the next start.
        let again = Arc::new(Locations::load(data.path()));
        assert_eq!(again.get("missing"), Some(codex.clone()));

        // Gone from the disk: said so.
        std::fs::remove_file(&codex).unwrap();
        let info = agent.info();
        assert!(!info.available);
        assert!(info.detail.unwrap().contains("已经不存在"));

        locations.clear("missing").unwrap();
        assert_eq!(Locations::load(data.path()).get("missing"), None);
        assert!(
            locations
                .set("nobody", "C:\\x")
                .await
                .unwrap_err()
                .to_string()
                .contains("没有可以选择")
        );
    }

    /// An agent whose program is never found; `spawn` says what it was given.
    struct Missing;

    impl AgentAdapter for Missing {
        fn info(&self) -> AgentInfo {
            AgentInfo {
                id: "missing".into(),
                name: "Missing".into(),
                kind: AgentKind::Acp,
                available: false,
                detail: Some("未找到命令 codex".into()),
                chat: true,
                terminal_command: vec!["codex".into(), "--flag".into()],
            }
        }

        fn spawn(
            &self,
            opts: SpawnOptions,
            _out: UnboundedSender<AgentOutput>,
        ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
            anyhow::bail!(
                "spawned {}",
                opts.program
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            )
        }
    }
}

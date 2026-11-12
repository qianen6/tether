//! The Codex desktop app's own `codex` executable, offered next to the one on
//! PATH. Both follow desktop-owned threads over IPC, otherwise run their own
//! app-server with the same `~/.codex`; the app's copy follows its updates.
//!
//! Where it lives:
//! - Windows (Microsoft Store, package `OpenAI.Codex`):
//!   `<PackageRootFolder>\app\resources\codex.exe`; the folder carries the
//!   version, so it is read from the registry each time.
//! - macOS: `Codex.app/Contents/Resources/codex` (unverified here).
//! - `TETHER_CODEX_DESKTOP` names the executable directly, anywhere.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use tether_agent_api::{
    AgentAdapter, AgentCommand, AgentOutput, BoxFuture, HistoryQuery, HistoryRead, ModelQuery,
    Permissions, SpawnOptions,
};
use tether_proto::{AgentInfo, HistoryData, HistoryReadData, ModelInfo};
use tokio::sync::mpsc::UnboundedSender;

pub(crate) const ID: &str = "codex-desktop";
pub(crate) const ENV: &str = "TETHER_CODEX_DESKTOP";

/// Codex as the desktop app ships it; `find` says where, each time it is asked.
pub(crate) struct CodexDesktop {
    inner: Arc<dyn AgentAdapter>,
    find: fn() -> Option<PathBuf>,
}

pub(crate) fn adapter() -> Arc<dyn AgentAdapter> {
    with_finder(find)
}

pub(crate) fn with_finder(find: fn() -> Option<PathBuf>) -> Arc<dyn AgentAdapter> {
    Arc::new(CodexDesktop {
        inner: tether_adapter_codex::adapter_with(crate::agents::via_desktop),
        find,
    })
}

impl AgentAdapter for CodexDesktop {
    fn info(&self) -> AgentInfo {
        let mut info = self.inner.info();
        info.id = ID.into();
        info.name = "Codex 桌面版".into();
        match (self.find)() {
            Some(program) => {
                info.available = true;
                info.detail = None;
                info.terminal_command = vec![program.to_string_lossy().into_owned()];
            }
            None => {
                info.available = false;
                info.detail = Some("未找到 Codex 桌面版".into());
            }
        }
        info
    }

    fn spawn(
        &self,
        mut opts: SpawnOptions,
        out: UnboundedSender<AgentOutput>,
    ) -> anyhow::Result<UnboundedSender<AgentCommand>> {
        opts.program = Some(self.program(opts.program.take())?);
        self.inner.spawn(opts, out)
    }

    fn list_history(&self, mut query: HistoryQuery) -> BoxFuture<HistoryData> {
        match self.program(query.program.take()) {
            Ok(program) => self.inner.list_history(HistoryQuery {
                program: Some(program),
                ..query
            }),
            Err(e) => Box::pin(async { Err(e) }),
        }
    }

    fn read_history(&self, mut read: HistoryRead) -> BoxFuture<HistoryReadData> {
        match self.program(read.program.take()) {
            Ok(program) => self.inner.read_history(HistoryRead {
                program: Some(program),
                ..read
            }),
            Err(e) => Box::pin(async { Err(e) }),
        }
    }

    fn list_models(&self, mut query: ModelQuery) -> BoxFuture<Vec<ModelInfo>> {
        match self.program(query.program.take()) {
            Ok(program) => self.inner.list_models(ModelQuery {
                program: Some(program),
            }),
            Err(e) => Box::pin(async { Err(e) }),
        }
    }

    fn list_permissions(&self, mut query: ModelQuery) -> BoxFuture<Permissions> {
        match self.program(query.program.take()) {
            Ok(program) => self.inner.list_permissions(ModelQuery {
                program: Some(program),
            }),
            Err(e) => Box::pin(async { Err(e) }),
        }
    }

    fn chat_dir(&self, hint: &str) -> anyhow::Result<Option<PathBuf>> {
        self.inner.chat_dir(hint)
    }

    fn reveal(&self, thread: &str) -> anyhow::Result<()> {
        self.inner.reveal(thread)
    }

    fn release_after(&self) -> Option<std::time::Duration> {
        self.inner.release_after()
    }
}

impl CodexDesktop {
    /// The given override, else where the app's executable is now.
    fn program(&self, given: Option<PathBuf>) -> anyhow::Result<PathBuf> {
        given.or_else(self.find).ok_or_else(|| {
            anyhow::anyhow!("未找到 Codex 桌面版（可用 {ENV} 指定 codex 可执行文件）")
        })
    }
}

/// The desktop app's `codex`, if this computer has one.
pub(crate) fn find() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(ENV).filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    platform::find()
}

/// `<root>\app\resources\codex.exe` of the newest `OpenAI.Codex_*` package root.
#[cfg_attr(not(windows), allow(dead_code))]
fn pick_store_package<'a>(
    packages: impl IntoIterator<Item = (&'a str, &'a Path)>,
) -> Option<PathBuf> {
    packages
        .into_iter()
        // `OpenAI.Codex_<version>_<arch>__<publisher>`, not `OpenAI.CodexPrimaryRuntime…`.
        .filter_map(|(name, root)| {
            let version = name.strip_prefix("OpenAI.Codex_")?.split('_').next()?;
            let version: Vec<u64> = version
                .split('.')
                .map(|n| n.parse().ok())
                .collect::<Option<_>>()?;
            Some((
                version,
                root.join("app").join("resources").join("codex.exe"),
            ))
        })
        .filter(|(_, exe)| exe.is_file())
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, exe)| exe)
}

#[cfg(windows)]
mod platform {
    use std::path::PathBuf;

    use winreg::{RegKey, enums::HKEY_CURRENT_USER};

    const PACKAGES: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

    pub(super) fn find() -> Option<PathBuf> {
        let packages = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(PACKAGES)
            .ok()?;
        let roots: Vec<(String, PathBuf)> = packages
            .enum_keys()
            .filter_map(Result::ok)
            .filter(|name| name.starts_with("OpenAI.Codex_"))
            .filter_map(|name| {
                let root: String = packages
                    .open_subkey(&name)
                    .ok()?
                    .get_value("PackageRootFolder")
                    .ok()?;
                Some((name, PathBuf::from(root)))
            })
            .collect();
        super::pick_store_package(roots.iter().map(|(n, r)| (n.as_str(), r.as_path())))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;

    pub(super) fn find() -> Option<PathBuf> {
        let mut apps = vec![PathBuf::from("/Applications")];
        if let Some(home) = std::env::home_dir() {
            apps.push(home.join("Applications"));
        }
        apps.into_iter()
            .map(|d| d.join("Codex.app/Contents/Resources/codex"))
            .find(|p| p.is_file())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    pub(super) fn find() -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_newest_store_package_that_has_codex() {
        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str, with_exe: bool| {
            let root = dir.path().join(name);
            let res = root.join("app").join("resources");
            std::fs::create_dir_all(&res).unwrap();
            if with_exe {
                std::fs::write(res.join("codex.exe"), b"").unwrap();
            }
            root
        };
        let old = make("OpenAI.Codex_26.902.1.0_x64__p", true);
        let new = make("OpenAI.Codex_26.1103.4239.0_x64__p", true);
        let broken = make("OpenAI.Codex_27.1.0.0_x64__p", false);
        let runtime = make("OpenAI.CodexPrimaryRuntime.v27_27.1.0.0_x64__q", true);
        let got = pick_store_package([
            ("OpenAI.Codex_26.902.1.0_x64__p", old.as_path()),
            ("OpenAI.Codex_26.1103.4239.0_x64__p", new.as_path()),
            ("OpenAI.Codex_27.1.0.0_x64__p", broken.as_path()),
            (
                "OpenAI.CodexPrimaryRuntime.v27_27.1.0.0_x64__q",
                runtime.as_path(),
            ),
        ]);
        assert_eq!(
            got,
            Some(new.join("app").join("resources").join("codex.exe"))
        );
        assert_eq!(pick_store_package([]), None);
    }

    #[test]
    fn info_follows_the_finder() {
        let missing = with_finder(|| None).info();
        assert_eq!(missing.id, ID);
        assert!(!missing.available);
        assert!(missing.chat);
        assert_eq!(missing.kind, tether_proto::AgentKind::Codex);
        assert_eq!(missing.detail.as_deref(), Some("未找到 Codex 桌面版"));

        let found = with_finder(|| Some(PathBuf::from("C:/x/codex.exe"))).info();
        assert!(found.available);
        assert_eq!(found.terminal_command, vec!["C:/x/codex.exe".to_string()]);
    }

    #[tokio::test]
    async fn spawn_without_the_app_is_an_error() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let err = with_finder(|| None)
            .spawn(SpawnOptions::default(), tx)
            .unwrap_err();
        assert!(err.to_string().contains("未找到 Codex 桌面版"));
    }

    /// Whatever this computer has; prints it (`cargo test -p tether-host -- --nocapture`).
    #[test]
    fn find_on_this_computer_does_not_panic() {
        println!("codex desktop: {:?}", find());
    }
}

//! The only native-process layer. The supervisor accepts an in-memory Spawner.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

use crate::{
    TunnelConfig,
    protocol::{Reader, Writer},
};

pub(crate) type Wait<'a> = Pin<Box<dyn Future<Output = io::Result<Exit>> + Send + 'a>>;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Exit {
    pub success: bool,
    pub code: Option<i32>,
}

pub(crate) trait Process: Send {
    fn wait(&mut self) -> Wait<'_>;
    fn kill(&mut self) -> io::Result<()>;
}

pub(crate) struct Helper {
    pub reader: Reader,
    pub writer: Writer,
    pub process: Box<dyn Process>,
}

pub(crate) trait Spawner: Send + 'static {
    fn spawn(&mut self, cfg: &TunnelConfig) -> Result<Helper>;
}

pub(crate) struct NativeSpawner;

impl Spawner for NativeSpawner {
    fn spawn(&mut self, cfg: &TunnelConfig) -> Result<Helper> {
        #[cfg(windows)]
        let helper = which(&cfg.helper)?;
        #[cfg(not(windows))]
        let helper = cfg.helper.clone();
        let mut command = Command::new(helper);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, including GUI callers
        let mut child = command.spawn().context("spawn tailpocket-host")?;
        let reader = child
            .stdout
            .take()
            .context("tailpocket-host stdout was not piped")?;
        let writer = child
            .stdin
            .take()
            .context("tailpocket-host stdin was not piped")?;
        Ok(Helper {
            reader: Box::new(reader),
            writer: Box::new(writer),
            process: Box::new(child),
        })
    }
}

impl Process for Child {
    fn wait(&mut self) -> Wait<'_> {
        Box::pin(async move {
            let status = Child::wait(self).await?;
            Ok(Exit {
                success: status.success(),
                code: status.code(),
            })
        })
    }

    fn kill(&mut self) -> io::Result<()> {
        self.start_kill()
    }
}

// A local `which` resolver keeps the frozen workspace lockfile unchanged. It
// honors PATH/PATHEXT rather than assuming a Unix executable or appending .exe
// unconditionally; explicit paths (including Unicode/spaces) are also resolved.
#[cfg(windows)]
fn which(program: &std::path::Path) -> Result<std::path::PathBuf> {
    use std::path::{Path, PathBuf};

    let direct = program.is_absolute() || program.components().count() > 1;
    let dirs: Vec<PathBuf> = if direct {
        vec![PathBuf::new()]
    } else {
        std::iter::once(PathBuf::from("."))
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ))
            .collect()
    };
    let extensions = std::env::var_os("PATHEXT").unwrap_or(".COM;.EXE;.BAT;.CMD".into());
    for dir in dirs {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return candidate.canonicalize().context("resolve helper path");
        }
        if program.extension().is_none() {
            for extension in extensions
                .to_string_lossy()
                .split(';')
                .filter(|s| !s.is_empty())
            {
                let mut name = candidate.as_os_str().to_os_string();
                name.push(extension);
                let path = Path::new(&name);
                if path.is_file() {
                    return path.canonicalize().context("resolve helper executable");
                }
            }
        }
    }
    anyhow::bail!(
        "tailpocket-host executable not found: {}",
        program.display()
    )
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn resolves_exe_cmd_and_unicode_paths_without_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        for extension in ["exe", "cmd"] {
            let path = dir.path().join(format!("随附 程序.{extension}"));
            std::fs::write(&path, b"fixture").unwrap();
            assert_eq!(which(&path).unwrap(), path.canonicalize().unwrap());
        }
        let exe = dir.path().join("native.EXE");
        std::fs::write(&exe, b"fixture").unwrap();
        assert_eq!(
            which(&dir.path().join("native")).unwrap(),
            exe.canonicalize().unwrap()
        );
        assert!(which(&dir.path().join("missing.exe")).is_err());
    }
}

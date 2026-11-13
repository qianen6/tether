//! Where DSH is, when the person says so: a launcher (or its folder) typed
//! in the desktop app, checked by running it, then used ahead of anything
//! found by looking around ([`crate::discover`]).

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::RwLock,
    time::Duration,
};

use anyhow::{Context, bail};
use tokio::process::Command;

use crate::{discover, hub};

/// Running `dsh --version` takes well under a second; a first run after an
/// update may be slower.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

static SAVED: RwLock<Option<PathBuf>> = RwLock::new(None);

/// The launcher the person chose (checked when chosen), or none to look
/// for DSH again. Applies at once to the built-in DSH agent.
pub fn set_saved_launcher(path: Option<PathBuf>) {
    if let Ok(mut saved) = SAVED.write() {
        *saved = path;
    }
}

pub(crate) fn saved_launcher() -> Option<PathBuf> {
    SAVED.read().ok().and_then(|s| s.clone())
}

/// A launcher that ran and said which DSH it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub launcher: PathBuf,
    /// What `dsh --version` printed, e.g. `0.1.1-rc.2`.
    pub version: String,
}

/// The launcher `input` names: the file itself, or the one in that folder
/// (or a folder under it). Errors say what is wrong in words for the person.
pub fn launcher_at(input: &str) -> anyhow::Result<PathBuf> {
    // Explorer's "复制为路径" puts quotes around it.
    let text = input.trim().trim_matches(['"', '\'']).trim();
    if text.is_empty() {
        bail!("请填写 dsh.cmd 的位置");
    }
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        bail!("请填写完整路径，例如 {}", example());
    }
    if !path.exists() {
        bail!("找不到 {}", path.display());
    }
    if path.is_dir() {
        return discover::launcher_under(&path).with_context(|| {
            format!(
                "{} 里没有找到 {}（也看了下一层文件夹）",
                path.display(),
                launcher_word()
            )
        });
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !discover::is_launcher_name(&name) {
        bail!(
            "{name} 不是 DSH 的启动程序：请选 {}。便携版的在 node.exe 旁边，npm 装的在 %APPDATA%\\npm 里",
            launcher_word()
        );
    }
    Ok(path)
}

/// Finds the launcher `input` names and runs `--version` on it.
pub async fn check_launcher(input: &str) -> anyhow::Result<Checked> {
    let launcher = launcher_at(input)?;
    let version = version_of(&launcher).await?;
    Ok(Checked { launcher, version })
}

async fn version_of(launcher: &Path) -> anyhow::Result<String> {
    let (exe, first) = hub::launcher(launcher);
    let mut cmd = Command::new(&exe);
    cmd.args(first)
        .arg("--version")
        .current_dir(hub::home_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
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
                launcher.display(),
                CHECK_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("运行 {} 失败", launcher.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty());
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let why = stderr
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .or(line)
            .unwrap_or("没有输出");
        bail!("运行 {} --version 出错：{why}", launcher.display());
    }
    match line {
        Some(v) if looks_like_version(v) => Ok(v.to_owned()),
        Some(other) => bail!(
            "{} 能运行，但输出的不像 DSH 的版本号：{other}",
            launcher.display()
        ),
        None => bail!("{} --version 没有输出版本号", launcher.display()),
    }
}

/// `0.1.1-rc.2`, `1.0.0`.
pub(crate) fn looks_like_version(line: &str) -> bool {
    let v = line.strip_prefix('v').unwrap_or(line);
    v.starts_with(|c: char| c.is_ascii_digit())
        && v.contains('.')
        && !v.contains(char::is_whitespace)
}

fn launcher_word() -> &'static str {
    if cfg!(windows) { "dsh.cmd" } else { "dsh" }
}

fn example() -> &'static str {
    if cfg!(windows) {
        r"C:\Tools\DeepSeekHarness\node-v24.19.0-win-x64\dsh.cmd"
    } else {
        "/usr/local/bin/dsh"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> &'static str {
        launcher_word()
    }

    #[test]
    fn what_is_typed() {
        let dir = tempfile::tempdir().unwrap();
        let portable = dir.path().join("node-v24.19.0-win-x64");
        std::fs::create_dir_all(&portable).unwrap();
        let launcher = portable.join(name());
        std::fs::write(&launcher, "").unwrap();
        let other = portable.join("node.exe");
        std::fs::write(&other, "").unwrap();

        // The file, quoted as Explorer copies it, or a folder above it.
        let quoted = format!("\"{}\"", launcher.display());
        assert_eq!(launcher_at(&quoted).unwrap(), launcher);
        assert_eq!(
            launcher_at(&dir.path().display().to_string()).unwrap(),
            launcher
        );

        let error = |input: &str| launcher_at(input).unwrap_err().to_string();
        assert!(error("  ").contains("请填写"));
        assert!(error("dsh.cmd").contains("完整路径"));
        assert!(error(&dir.path().join("nope").display().to_string()).contains("找不到"));
        assert!(error(&other.display().to_string()).contains("不是 DSH 的启动程序"));
        let empty = tempfile::tempdir().unwrap();
        assert!(error(&empty.path().display().to_string()).contains("没有找到"));
    }

    #[test]
    fn versions() {
        assert!(looks_like_version("0.1.1-rc.2"));
        assert!(looks_like_version("v1.0.0"));
        assert!(!looks_like_version("Usage: dsh [options]"));
        assert!(!looks_like_version("hello"));
    }

    #[tokio::test]
    async fn a_program_that_is_not_dsh() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join(name());
        if cfg!(windows) {
            std::fs::write(&fake, "@echo off\r\necho hello there\r\n").unwrap();
        } else {
            std::fs::write(&fake, "#!/bin/sh\necho hello there\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let error = check_launcher(&fake.display().to_string())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("不像 DSH 的版本号"), "{error}");
    }

    #[tokio::test]
    async fn a_launcher_that_answers() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join(name());
        if cfg!(windows) {
            std::fs::write(&fake, "@echo off\r\necho 0.1.1-rc.2\r\n").unwrap();
        } else {
            std::fs::write(&fake, "#!/bin/sh\necho 0.1.1-rc.2\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let checked = check_launcher(&fake.display().to_string()).await.unwrap();
        assert_eq!(checked.version, "0.1.1-rc.2");
        assert_eq!(checked.launcher, fake);
    }

    /// The DSH on this computer, if `TETHER_DSH` names it.
    #[tokio::test]
    #[ignore = "needs a real DSH: set TETHER_DSH"]
    async fn live_check() {
        let path = std::env::var("TETHER_DSH").expect("TETHER_DSH");
        let checked = check_launcher(&path).await.unwrap();
        eprintln!("{checked:?}");
    }
}

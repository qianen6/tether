//! Finding the `dsh` launcher when it is not on `PATH`.
//!
//! DSH is often installed as a portable folder (Node.js plus DSH, started by
//! `dsh.cmd` next to `node.exe`) that never touches `PATH`, so besides `PATH`
//! look in the folders such installs and the npm / pnpm global bins use.

use std::path::{Path, PathBuf};

/// Folder names a portable DSH install is unpacked into.
const INSTALL_DIRS: &[&str] = &["DeepSeekHarness", "DeepSeek Harness", "dsh"];

fn launcher_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["dsh.cmd", "dsh.exe", "dsh.bat"]
    } else {
        &["dsh"]
    }
}

/// The launcher directly in `dir`, if any.
fn launcher_in(dir: &Path) -> Option<PathBuf> {
    launcher_names()
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
}

/// `root/dsh*` or `root/<child>/dsh*` (`root/<child>/bin/dsh` on Unix).
/// Backups are skipped; among several children the newest launcher wins.
fn launcher_under(root: &Path) -> Option<PathBuf> {
    if let Some(found) = launcher_in(root) {
        return Some(found);
    }
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("backup")
        })
        .filter_map(|entry| {
            let dir = entry.path();
            launcher_in(&dir).or_else(|| launcher_in(&dir.join("bin")))
        })
        .map(|p| {
            let modified = p
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (modified, p)
        })
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

/// Where to look, most specific first.
fn search_roots() -> Vec<PathBuf> {
    let env = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let home = env(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    let mut parents: Vec<PathBuf> = Vec::new();
    let mut bins: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        // `C:\` and `C:\Tools` on the system drive.
        let drive = std::env::var("SystemDrive")
            .ok()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "C:".into());
        let drive_root = PathBuf::from(format!("{drive}\\"));
        parents.push(drive_root.join("Tools"));
        parents.push(drive_root);
        parents.extend(env("LOCALAPPDATA").map(|d| d.join("Programs")));
        parents.extend(env("LOCALAPPDATA"));
        parents.extend(env("ProgramFiles"));
        parents.extend(home.clone());
        parents.extend(home.as_ref().map(|h| h.join("Tools")));
        bins.extend(env("APPDATA").map(|d| d.join("npm")));
        bins.extend(env("PNPM_HOME"));
        bins.extend(env("LOCALAPPDATA").map(|d| d.join("pnpm")));
    } else {
        parents.extend(home.clone());
        parents.extend(home.as_ref().map(|h| h.join("Tools")));
        parents.push(PathBuf::from("/opt"));
        bins.extend(home.as_ref().map(|h| h.join(".local").join("bin")));
        bins.extend(home.as_ref().map(|h| h.join(".npm-global").join("bin")));
        bins.extend(env("PNPM_HOME"));
        bins.extend(
            home.as_ref()
                .map(|h| h.join(".local").join("share").join("pnpm")),
        );
        bins.push(PathBuf::from("/opt/homebrew/bin"));
        bins.push(PathBuf::from("/usr/local/bin"));
    }
    let mut roots: Vec<PathBuf> = parents
        .iter()
        .flat_map(|p| INSTALL_DIRS.iter().map(move |d| p.join(d)))
        .collect();
    roots.extend(bins);
    roots
}

/// The first launcher found under `roots`.
fn find_in(roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .filter(|r| r.is_dir())
        .find_map(|r| launcher_under(r))
}

/// `TETHER_DSH`, then `dsh` on `PATH`, then the usual install folders.
/// `None` when DSH is nowhere to be found.
pub(crate) fn locate(command_env: &str) -> Option<String> {
    if let Some(program) = std::env::var(command_env)
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        return Some(program);
    }
    if which::which("dsh").is_ok() {
        return Some("dsh".into());
    }
    let found = find_in(&search_roots())?;
    tracing::info!(path = %found.display(), "found dsh outside PATH");
    Some(found.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    fn name() -> &'static str {
        launcher_names()[0]
    }

    #[test]
    fn portable_install_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("DeepSeekHarness");
        let live = root.join("node-v24.19.0-win-x64").join(name());
        let old = root
            .join("node-v24.19.0-win-x64-0.1.0-rc.6-backup-20260825")
            .join(name());
        touch(&old);
        touch(&live);
        // A backup touched later still loses.
        std::fs::write(&old, "newer").unwrap();
        std::fs::create_dir_all(root.join("output")).unwrap();
        let missing = dir.path().join("nothing-here");
        assert_eq!(find_in(&[missing, root]), Some(live));
    }

    #[test]
    fn bin_folders_and_misses() {
        let dir = tempfile::tempdir().unwrap();
        let npm = dir.path().join("npm");
        std::fs::create_dir_all(&npm).unwrap();
        assert_eq!(find_in(std::slice::from_ref(&npm)), None);
        touch(&npm.join(name()));
        assert_eq!(find_in(std::slice::from_ref(&npm)), Some(npm.join(name())));

        let unix = dir.path().join("dsh");
        touch(&unix.join("dsh-0.1.1").join("bin").join(name()));
        assert_eq!(
            find_in(std::slice::from_ref(&unix)),
            Some(unix.join("dsh-0.1.1").join("bin").join(name()))
        );
    }

    #[test]
    fn roots_include_the_usual_places() {
        let roots = search_roots();
        assert!(
            roots
                .iter()
                .any(|r| r.ends_with(Path::new("Tools").join("DeepSeekHarness")))
        );
    }
}

//! What the phone's terminal runs for DSH: DSH's own `tui` profile when it
//! is installed, otherwise Tether's line-mode client (`assets/dsh-term.mjs`)
//! on the Node.js that runs DSH.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

const SCRIPT: &str = include_str!("../assets/dsh-term.mjs");

/// `$DSH_HOME`, or `~/.dsh`.
pub(crate) fn dsh_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("DSH_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(home).join(".dsh"))
}

fn has_tui(home: Option<&Path>) -> bool {
    home.is_some_and(|h| {
        h.join("profiles")
            .join("tui")
            .join("package.json")
            .is_file()
    })
}

/// The Node.js next to the `dsh` launcher (a portable install), else `node`.
fn node_for(dsh: &Path) -> Option<PathBuf> {
    let dir = dsh.parent()?;
    for name in ["node.exe", "node"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    which::which("node").ok()
}

/// The client script, written once per process to the temp folder.
fn script() -> Option<&'static Path> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let path =
            std::env::temp_dir().join(format!("tether-dsh-term-{}.mjs", env!("CARGO_PKG_VERSION")));
        match std::fs::write(&path, SCRIPT) {
            Ok(()) => Some(path),
            Err(error) => {
                tracing::warn!("cannot write {}: {error}", path.display());
                None
            }
        }
    })
    .as_deref()
}

/// `dsh` is the launcher as configured (`command[0]`); `resolved` its path.
pub(crate) fn command(dsh: &str, resolved: Option<&Path>) -> Vec<String> {
    command_with(dsh, resolved, dsh_home().as_deref(), script)
}

fn command_with(
    dsh: &str,
    resolved: Option<&Path>,
    home: Option<&Path>,
    script: impl FnOnce() -> Option<&'static Path>,
) -> Vec<String> {
    let launcher = resolved
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| dsh.to_owned());
    if has_tui(home) {
        return vec![launcher, "--profile".into(), "tui".into()];
    }
    let node = resolved.and_then(node_for);
    match (node, script()) {
        (Some(node), Some(script)) => vec![
            node.display().to_string(),
            script.display().to_string(),
            "--dsh".into(),
            launcher,
        ],
        _ => vec![launcher, "--profile".into(), "tui".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_profile_wins() {
        let dir = tempfile::tempdir().unwrap();
        let tui = dir.path().join("profiles").join("tui");
        std::fs::create_dir_all(&tui).unwrap();
        let none = || -> Option<&'static Path> { None };
        // Without the profile and without node: still names DSH.
        assert_eq!(
            command_with("dsh", None, Some(dir.path()), none),
            vec!["dsh", "--profile", "tui"]
        );
        std::fs::write(tui.join("package.json"), "{}").unwrap();
        assert_eq!(
            command_with("dsh", None, Some(dir.path()), || panic!("not needed")),
            vec!["dsh", "--profile", "tui"]
        );
    }

    #[test]
    fn client_on_the_bundled_node() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir
            .path()
            .join(if cfg!(windows) { "dsh.cmd" } else { "dsh" });
        let node = dir
            .path()
            .join(if cfg!(windows) { "node.exe" } else { "node" });
        std::fs::write(&launcher, "").unwrap();
        std::fs::write(&node, "").unwrap();
        static SCRIPT_PATH: OnceLock<PathBuf> = OnceLock::new();
        let script = SCRIPT_PATH.get_or_init(|| PathBuf::from("client.mjs"));
        let got = command_with("dsh", Some(&launcher), None, || Some(script.as_path()));
        assert_eq!(
            got,
            vec![
                node.display().to_string(),
                "client.mjs".to_string(),
                "--dsh".to_string(),
                launcher.display().to_string()
            ]
        );
    }

    #[test]
    fn script_is_written() {
        let path = script().expect("temp folder is writable");
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("events.mux"));
    }
}

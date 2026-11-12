//! The data directory: `config.toml`, `agents.toml`, and where the rest lives.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::{CustomAgent, CustomMode};

pub(crate) const CONFIG_FILE: &str = "config.toml";
pub(crate) const AGENTS_FILE: &str = "agents.toml";
pub(crate) const DATABASE_FILE: &str = "tether.db";
pub(crate) const TUNNEL_STATE_FILE: &str = "tunnel-state.json";

const CONFIG_TEMPLATE: &str = r#"# Tether settings. Command-line options override these.

# Computer name shown on phones (default: the host name).
# name = "My PC"

# Own DERP relay map instead of Tailcat's public relays.
# derp_map_url = "https://example.com/derpmap.json"
"#;

const AGENTS_TEMPLATE: &str = r#"# Extra agents for Tether.
#
# mode = "acp"       an Agent Client Protocol agent: chat and terminal
# mode = "terminal"  any command line program: terminal only
#
# [[agent]]
# id = "my-agent"
# name = "My Agent"
# command = ["my-agent", "--acp"]
# mode = "acp"
"#;

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub derp_map_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentsFile {
    #[serde(default)]
    agent: Vec<AgentEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    command: Vec<String>,
    mode: Mode,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Acp,
    Terminal,
}

/// Reads `name`, creating it from `template` first when missing.
fn read_or_create(path: &Path, template: &str) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(path, template)
                .with_context(|| format!("cannot create {}", path.display()))?;
            Ok(template.into())
        }
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

pub(crate) fn load_config(dir: &Path) -> anyhow::Result<FileConfig> {
    let path = dir.join(CONFIG_FILE);
    let text = read_or_create(&path, CONFIG_TEMPLATE)?;
    toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))
}

pub(crate) fn load_agents(dir: &Path) -> anyhow::Result<Vec<CustomAgent>> {
    let path = dir.join(AGENTS_FILE);
    let text = read_or_create(&path, AGENTS_TEMPLATE)?;
    let file: AgentsFile =
        toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
    file.agent
        .into_iter()
        .map(|a| {
            anyhow::ensure!(
                !a.id.trim().is_empty(),
                "{}: empty agent id",
                path.display()
            );
            anyhow::ensure!(
                a.command.first().is_some_and(|c| !c.trim().is_empty()),
                "{}: agent {} has an empty command",
                path.display(),
                a.id
            );
            Ok(CustomAgent {
                name: a.name.unwrap_or_else(|| a.id.clone()),
                id: a.id,
                command: a.command,
                mode: match a.mode {
                    Mode::Acp => CustomMode::Acp,
                    Mode::Terminal => CustomMode::Terminal,
                },
            })
        })
        .collect()
}

/// The computer's name, best effort.
pub(crate) fn host_name() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return v.trim().into();
        }
    }
    let mut command = std::process::Command::new("hostname");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    if let Ok(out) = command.output() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !name.is_empty() {
            return name;
        }
    }
    "Tether".into()
}

/// `tailpocket-host`: next to the executable (as Tauri bundles it, or under its
/// release name), then in the repository's `vendor/tailpocket/bin/`.
pub(crate) fn find_helper() -> Option<PathBuf> {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    let release = format!("tailpocket-host-{os}-{arch}{ext}");
    let plain = format!("tailpocket-host{ext}");
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(dir);
    }
    // The checkout it was built in: for development only, so a release does
    // not carry the build machine's paths.
    #[cfg(debug_assertions)]
    dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/tailpocket/bin"));
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("vendor/tailpocket/bin"));
    }
    dirs.iter()
        .flat_map(|d| [d.join(&plain), d.join(&release)])
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_templates_that_parse_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_config(dir.path()).unwrap(), FileConfig::default());
        assert!(load_agents(dir.path()).unwrap().is_empty());
        assert!(dir.path().join(CONFIG_FILE).is_file());
        assert!(dir.path().join(AGENTS_FILE).is_file());
    }

    #[test]
    fn reads_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "name = \"Desk\"\nderp_map_url = \"https://d/map.json\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(AGENTS_FILE),
            r#"
[[agent]]
id = "mine"
command = ["mine", "--acp"]
mode = "acp"

[[agent]]
id = "top"
name = "htop"
command = ["htop"]
mode = "terminal"
"#,
        )
        .unwrap();
        let c = load_config(dir.path()).unwrap();
        assert_eq!(c.name.as_deref(), Some("Desk"));
        assert_eq!(c.derp_map_url.as_deref(), Some("https://d/map.json"));
        let a = load_agents(dir.path()).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].name, "mine");
        assert_eq!(a[0].mode, CustomMode::Acp);
        assert_eq!(a[1].name, "htop");
        assert_eq!(a[1].mode, CustomMode::Terminal);
    }

    #[test]
    fn rejects_bad_agents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(AGENTS_FILE),
            "[[agent]]\nid = \"x\"\ncommand = []\nmode = \"acp\"\n",
        )
        .unwrap();
        assert!(load_agents(dir.path()).is_err());
        std::fs::write(
            dir.path().join(AGENTS_FILE),
            "[[agent]]\nid = \"x\"\ncommand = [\"x\"]\nmode = \"weird\"\n",
        )
        .unwrap();
        assert!(load_agents(dir.path()).is_err());
    }
}

//! Conversations without a project, the way the Codex app keeps its "Chats":
//! each gets a fresh folder `~/Documents/Codex/<YYYY-MM-DD>/<slug>`, so the
//! app lists ours next to its own. Slug rules follow the app (as mirrored by
//! Remodex, `phodex-bridge/src/project-handler.js`, Apache-2.0).

use std::path::{Path, PathBuf};

use anyhow::Context;

const FALLBACK: &str = "new-chat";
const MAX_WORDS: usize = 6;
const MAX_LEN: usize = 60;
const MAX_TRIES: u32 = 50;

/// `~/Documents/Codex`.
pub(crate) fn root() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join("Documents").join("Codex"))
}

/// Whether `cwd` is one of those folders.
pub(crate) fn is_chat(cwd: &str, root: &Path) -> bool {
    let cwd = Path::new(cwd);
    if cfg!(windows) {
        // Drive letters and folder names differ only in case there.
        let lower = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
        let (cwd, root) = (lower(cwd), lower(root));
        cwd.strip_prefix(&root)
            .is_some_and(|rest| rest.starts_with('\\') && rest.len() > 1)
    } else {
        cwd.starts_with(root) && cwd != root
    }
}

/// Kebab-case of the first words, ASCII only, as the app names them.
pub(crate) fn slug(hint: &str) -> String {
    let words: Vec<String> = hint
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(MAX_WORDS)
        .map(str::to_owned)
        .collect();
    let mut slug = words.join("-");
    if slug.len() > MAX_LEN {
        slug.truncate(MAX_LEN);
        slug = slug.trim_end_matches('-').to_owned();
    }
    if slug.is_empty() {
        FALLBACK.into()
    } else {
        slug
    }
}

/// Makes `<root>/<date>/<slug>`, `-2`, `-3`… when taken.
pub(crate) fn create(root: &Path, date: &str, hint: &str) -> anyhow::Result<PathBuf> {
    let day = root.join(date);
    std::fs::create_dir_all(&day).with_context(|| format!("create {}", day.display()))?;
    let base = slug(hint);
    for n in 1..=MAX_TRIES {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let path = day.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
        }
    }
    anyhow::bail!("too many chats named {base} on {date}")
}

/// Today on this computer's clock, as the app dates its folders.
pub(crate) fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_like_the_app() {
        assert_eq!(slug("mi dici la pwd corrente?"), "mi-dici-la-pwd-corrente");
        assert_eq!(
            slug("Fix the BUG in main.rs now please ok"),
            "fix-the-bug-in-main-rs"
        );
        assert_eq!(slug("你好"), "new-chat");
        assert_eq!(slug("  "), "new-chat");
        assert_eq!(slug("帮我看 v2ray 和 clash"), "v2ray-clash");
        let long = slug(&"abcdefghijklmnopqrstuvwxyz ".repeat(4));
        assert!(long.len() <= MAX_LEN && !long.ends_with('-'), "{long}");
    }

    #[test]
    fn creates_a_fresh_folder_each_time() {
        let dir = tempfile::tempdir().unwrap();
        let a = create(dir.path(), "2026-11-08", "hello world").unwrap();
        let b = create(dir.path(), "2026-11-08", "hello world").unwrap();
        assert_eq!(a, dir.path().join("2026-11-08").join("hello-world"));
        assert_eq!(b, dir.path().join("2026-11-08").join("hello-world-2"));
        assert!(a.is_dir() && b.is_dir());
    }

    #[test]
    fn knows_its_folders() {
        let root = Path::new(if cfg!(windows) {
            r"C:\Users\q\Documents\Codex"
        } else {
            "/home/q/Documents/Codex"
        });
        let inside = root.join("2026-11-03").join("ni-h");
        assert!(is_chat(&inside.to_string_lossy(), root));
        assert!(!is_chat(&root.to_string_lossy(), root));
        assert!(!is_chat(
            &root.with_file_name("Codex2").join("x").to_string_lossy(),
            root
        ));
        if cfg!(windows) {
            assert!(is_chat(r"c:\users\Q\documents\codex\2026-11-03\ni-h", root));
        }
    }

    #[test]
    fn today_is_a_date() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
    }
}

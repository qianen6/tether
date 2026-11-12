use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

const MAX_STATE_BYTES: u64 = 1 << 20;

// Deliberately not Debug: the identity contains private keys.
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct State {
    pub identity: String,
    pub allowed: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct StateFile(pub PathBuf);

impl StateFile {
    pub async fn load(&self) -> Result<State> {
        let path = self.0.clone();
        tokio::task::spawn_blocking(move || load(&path))
            .await
            .context("state reader panicked")?
    }

    pub async fn save(&self, state: State) -> Result<()> {
        let path = self.0.clone();
        tokio::task::spawn_blocking(move || save(&path, &state))
            .await
            .context("state writer panicked")?
    }
}

fn load(path: &Path) -> Result<State> {
    ensure!(!path.as_os_str().is_empty(), "state path is empty");
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(error) => return Err(error).context("open tunnel state"),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .context("protect tunnel state")?;
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("read tunnel state")?;
    ensure!(
        bytes.len() as u64 <= MAX_STATE_BYTES,
        "tunnel state is too large"
    );
    let state: State = serde_json::from_slice(&bytes).context("decode tunnel state")?;
    ensure!(!state.identity.is_empty(), "saved tunnel identity is empty");
    ensure!(state.allowed.len() <= 256, "too many saved phones");
    Ok(state)
}

fn save(path: &Path, state: &State) -> Result<()> {
    ensure!(
        !state.identity.is_empty(),
        "helper supplied an empty identity"
    );
    ensure!(
        state.allowed.len() <= 256,
        "helper supplied too many phones"
    );
    let bytes = serde_json::to_vec(state).context("encode tunnel state")?;
    ensure!(
        bytes.len() as u64 <= MAX_STATE_BYTES,
        "tunnel state is too large"
    );
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).context("create tunnel state directory")?;
    // Same-directory temporary file + replacement rename, including on Windows
    // (tempfile uses MoveFileExW/MOVEFILE_REPLACE_EXISTING, never delete+rename).
    let mut temporary = tempfile::Builder::new()
        .prefix(".tether-tunnel-")
        .tempfile_in(parent)
        .context("create temporary tunnel state")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .context("protect temporary tunnel state")?;
    }
    temporary.write_all(&bytes).context("write tunnel state")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync tunnel state")?;
    temporary
        .persist(path)
        .map_err(|e| e.error)
        .context("replace tunnel state")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_state_is_empty_but_corruption_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = StateFile(dir.path().join("state.json"));
        assert!(file.load().await.unwrap().identity.is_empty());
        std::fs::write(&file.0, b"{corrupt").unwrap();
        assert!(file.load().await.is_err());
        assert_eq!(std::fs::read(&file.0).unwrap(), b"{corrupt");
    }

    #[tokio::test]
    async fn atomic_replace_roundtrips_and_leaves_no_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = StateFile(dir.path().join("nested/state.json"));
        for phones in [vec!["nodekey:one".into()], vec![]] {
            let state = State {
                identity: "private identity".into(),
                allowed: phones,
            };
            file.save(state.clone()).await.unwrap();
            assert!(file.load().await.unwrap() == state);
        }
        assert_eq!(
            std::fs::read_dir(file.0.parent().unwrap()).unwrap().count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file.0).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn failed_replace_preserves_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = StateFile(dir.path().join("state.json"));
        let state = State {
            identity: "identity".into(),
            allowed: vec![],
        };
        file.save(state).await.unwrap();
        let before = std::fs::read(&file.0).unwrap();
        assert!(file.save(State::default()).await.is_err());
        assert_eq!(std::fs::read(&file.0).unwrap(), before);
        // An actual persist failure also cleans up the temporary file.
        let bad = StateFile(dir.path().join("directory"));
        std::fs::create_dir(&bad.0).unwrap();
        assert!(
            bad.save(State {
                identity: "x".into(),
                allowed: vec![]
            })
            .await
            .is_err()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::awake::AwakeController;

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct Preferences {
    autostart_initialized: bool,
    keep_awake: bool,
    /// A DSH launcher typed in by an earlier version; moved to the Host
    /// (which keeps every agent's location) on start, then cleared.
    dsh_path: Option<String>,
}

impl Preferences {
    fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("读取桌面设置"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).context("读取桌面设置"),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temporary, path).context("保存桌面设置")
    }
}

pub struct PowerState {
    path: PathBuf,
    preferences: Mutex<Preferences>,
    awake: AwakeController,
}

impl PowerState {
    pub fn load(path: PathBuf) -> Result<Self> {
        let preferences = Preferences::load(&path)?;
        Ok(Self {
            path,
            preferences: Mutex::new(preferences),
            awake: AwakeController::new()?,
        })
    }

    /// Enable once on first run, then leave the OS/plugin setting authoritative.
    /// A user disabling autostart must not have it re-enabled at next launch.
    pub async fn initialize_autostart(&self, enable: impl FnOnce() -> Result<()>) -> Result<()> {
        let mut preferences = self.preferences.lock().await;
        if !preferences.autostart_initialized {
            enable()?;
            let mut next = preferences.clone();
            next.autostart_initialized = true;
            next.save(&self.path)?;
            *preferences = next;
        }
        Ok(())
    }

    pub async fn apply_saved(&self) -> Result<()> {
        self.awake
            .set(self.preferences.lock().await.keep_awake)
            .await
    }

    pub fn keep_awake(&self) -> bool {
        self.awake.is_enabled()
    }

    pub async fn set_keep_awake(&self, enabled: bool) -> Result<bool> {
        let mut preferences = self.preferences.lock().await;
        let previous = self.keep_awake();
        self.awake.set(enabled).await?;
        let mut next = preferences.clone();
        next.keep_awake = enabled;
        if let Err(error) = next.save(&self.path) {
            self.awake.set(previous).await.context("恢复休眠设置")?;
            return Err(error);
        }
        *preferences = next;
        Ok(self.keep_awake())
    }

    pub async fn dsh_path(&self) -> Option<String> {
        self.preferences.lock().await.dsh_path.clone()
    }

    pub async fn set_dsh_path(&self, path: Option<String>) -> Result<()> {
        let mut preferences = self.preferences.lock().await;
        let mut next = preferences.clone();
        next.dsh_path = path;
        next.save(&self.path)?;
        *preferences = next;
        Ok(())
    }

    pub fn shutdown(&self) {
        self.awake.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct TestPath(PathBuf);
    impl TestPath {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "tether-desktop-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn settings(&self) -> PathBuf {
            self.0.join("settings.json")
        }
        fn power(&self) -> PowerState {
            PowerState {
                path: self.settings(),
                preferences: Mutex::new(Preferences::default()),
                awake: AwakeController::fake().unwrap(),
            }
        }
    }
    impl Drop for TestPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.settings());
            let _ = std::fs::remove_file(self.settings().with_extension("json.tmp"));
            let _ = std::fs::remove_dir(self.settings());
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    #[tokio::test]
    async fn the_dsh_location_is_kept() {
        let temp = TestPath::new();
        let power = temp.power();
        assert_eq!(power.dsh_path().await, None);
        power
            .set_dsh_path(Some(r"D:\DSH\dsh.cmd".into()))
            .await
            .unwrap();
        let saved = Preferences::load(&temp.settings()).unwrap();
        assert_eq!(saved.dsh_path.as_deref(), Some(r"D:\DSH\dsh.cmd"));
        power.set_dsh_path(None).await.unwrap();
        assert_eq!(Preferences::load(&temp.settings()).unwrap().dsh_path, None);
        // Settings from before there was a DSH location still load.
        std::fs::write(temp.settings(), br#"{"keep_awake":true}"#).unwrap();
        assert_eq!(Preferences::load(&temp.settings()).unwrap().dsh_path, None);
    }

    #[test]
    fn missing_settings_default_and_malformed_settings_report_an_error() {
        let temp = TestPath::new();
        assert_eq!(
            Preferences::load(&temp.settings()).unwrap(),
            Preferences::default()
        );
        std::fs::write(temp.settings(), b"not json").unwrap();
        assert!(Preferences::load(&temp.settings()).is_err());
    }

    #[tokio::test]
    async fn autostart_is_only_initialized_once_and_failure_is_retryable() {
        let temp = TestPath::new();
        let power = temp.power();
        assert!(
            power
                .initialize_autostart(|| anyhow::bail!("test registry error"))
                .await
                .is_err()
        );
        assert!(!temp.settings().exists());
        power.initialize_autostart(|| Ok(())).await.unwrap();
        power
            .initialize_autostart(|| panic!("must preserve a user's later disable"))
            .await
            .unwrap();
        let saved = Preferences::load(&temp.settings()).unwrap();
        assert!(saved.autostart_initialized);
        power.shutdown();
    }

    #[tokio::test]
    async fn keep_awake_commands_persist_both_values_and_release_on_exit() {
        let temp = TestPath::new();
        let power = temp.power();
        assert!(!power.keep_awake());
        assert!(power.set_keep_awake(true).await.unwrap());
        assert!(Preferences::load(&temp.settings()).unwrap().keep_awake);
        assert!(!power.set_keep_awake(false).await.unwrap());
        assert!(!Preferences::load(&temp.settings()).unwrap().keep_awake);
        power.set_keep_awake(true).await.unwrap();
        power.shutdown();
        assert!(!power.keep_awake());
        assert!(Preferences::load(&temp.settings()).unwrap().keep_awake);
    }

    #[tokio::test]
    async fn save_failure_restores_the_previous_power_state() {
        let temp = TestPath::new();
        let power = temp.power();
        std::fs::create_dir(temp.settings()).unwrap();
        assert!(power.set_keep_awake(true).await.is_err());
        assert!(!power.keep_awake());
    }

    #[tokio::test]
    async fn saved_power_choice_is_reapplied_on_restart_without_os_calls() {
        let temp = TestPath::new();
        let first = temp.power();
        first.set_keep_awake(true).await.unwrap();
        first.shutdown();
        let restored = PowerState {
            path: temp.settings(),
            preferences: Mutex::new(Preferences::load(&temp.settings()).unwrap()),
            awake: AwakeController::fake().unwrap(),
        };
        assert!(!restored.keep_awake());
        restored.apply_saved().await.unwrap();
        assert!(restored.keep_awake());
        restored.shutdown();
        assert!(!restored.keep_awake());
    }
}

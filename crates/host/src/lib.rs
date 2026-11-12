//! The computer side of Tether, assembled: storage, terminals, agents, the
//! gateway and the tunnel. Used by the CLI and the desktop app.

mod agents;
mod codex_desktop;
mod config;
mod link;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tether_agent_api::AgentAdapter;
use tether_proto::{AgentInfo, SessionState};
use tether_server::{Server, ServerConfig};
use tether_store::{Device, Store};
use tether_terminals::{DEFAULT_SCROLLBACK, TermManager};
use tether_tunnel::{QR_PREFIX, Tunnel, TunnelConfig};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use link::TunnelBackend;

#[derive(Debug, Clone, Default)]
pub struct HostConfig {
    /// See [`default_data_dir`].
    pub data_dir: PathBuf,
    /// Computer name shown on phones; defaults to the host name.
    pub name: String,
    /// `tailpocket-host`; by default looked up next to the executable, then in
    /// `vendor/tailpocket/bin/`.
    pub tailpocket_helper: Option<PathBuf>,
    pub derp_map_url: Option<String>,
    /// In addition to `data_dir/agents.toml`.
    pub custom_agents: Vec<CustomAgent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CustomAgent {
    pub id: String,
    pub name: String,
    pub command: Vec<String>,
    pub mode: CustomMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomMode {
    Acp,
    Terminal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PairInfo {
    pub qr: String,
    /// Unix milliseconds.
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostStatus {
    pub tunnel_address: Option<String>,
    pub gateway: SocketAddr,
    pub connected_devices: usize,
    pub sessions_working: usize,
    pub sessions_need_you: usize,
}

/// Windows `%APPDATA%\Tether`, macOS `~/Library/Application Support/Tether`,
/// Linux `~/.local/share/tether`.
pub fn default_data_dir() -> PathBuf {
    let name = if cfg!(any(windows, target_os = "macos")) {
        "Tether"
    } else {
        "tether"
    };
    match directories::BaseDirs::new() {
        Some(dirs) => dirs.data_dir().join(name),
        None => PathBuf::from(".").join(name),
    }
}

/// Longest pairing window, as the gateway and the tunnel allow.
const MAX_PAIR_TTL: Duration = Duration::from_secs(15 * 60);

pub struct Host {
    store: Arc<Store>,
    server: Option<Server>,
    gateway: SocketAddr,
    agents: Vec<Arc<dyn AgentAdapter>>,
    tunnel: Option<Arc<dyn TunnelBackend>>,
    joined: Option<mpsc::UnboundedSender<link::Joined>>,
    linker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("gateway", &self.gateway)
            .field("tunnel", &self.tunnel.is_some())
            .finish_non_exhaustive()
    }
}

impl Host {
    pub async fn start(cfg: HostConfig) -> anyhow::Result<Host> {
        let dir = prepare_dir(&cfg)?;
        let file = config::load_config(&dir)?;
        let derp_map_url = cfg.derp_map_url.clone().or(file.derp_map_url);
        let helper = cfg
            .tailpocket_helper
            .clone()
            .or_else(config::find_helper)
            .context(
                "未找到 tailpocket-host：放在 tether 可执行文件旁，或运行 scripts/fetch-vendor.sh",
            )?;
        let name = host_name(&cfg, file.name);
        Self::start_with(cfg, name, move |forward, state_path, name| async move {
            let tunnel = Tunnel::start(TunnelConfig {
                helper,
                forward,
                state_path,
                name,
                derp_map_url,
                qr_prefix: QR_PREFIX.into(),
            })
            .await
            .context("cannot start the TailPocket tunnel")?;
            Ok(Some(Arc::new(tunnel) as Arc<dyn TunnelBackend>))
        })
        .await
    }

    /// Everything but the tunnel comes from `cfg`; `tunnel` builds it once the
    /// gateway listens. `None` runs without one (tests).
    pub(crate) async fn start_with<F, Fut>(
        cfg: HostConfig,
        name: String,
        tunnel: F,
    ) -> anyhow::Result<Host>
    where
        F: FnOnce(SocketAddr, PathBuf, String) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Option<Arc<dyn TunnelBackend>>>>,
    {
        let dir = prepare_dir(&cfg)?;
        let mut custom = config::load_agents(&dir)?;
        custom.extend(cfg.custom_agents.iter().cloned());
        let echo = std::env::var(agents::ECHO_ENV).is_ok_and(|v| v == "1");
        let agents = agents::discover(&custom, echo);

        let store = Arc::new(Store::open(&dir.join(config::DATABASE_FILE))?);
        let terms = Arc::new(TermManager::new(DEFAULT_SCROLLBACK));
        let server = Server::start(
            ServerConfig {
                bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                host_name: name.clone(),
            },
            store.clone(),
            terms,
            agents.clone(),
        )
        .await?;
        let gateway = server.local_addr();
        tracing::info!(%gateway, "gateway listening");

        let tunnel = match tunnel(gateway, dir.join(config::TUNNEL_STATE_FILE), name).await {
            Ok(t) => t,
            Err(error) => {
                server.shutdown().await;
                return Err(error);
            }
        };
        let mut host = Host {
            store,
            server: Some(server),
            gateway,
            agents,
            tunnel,
            joined: None,
            linker: None,
        };
        if let Some(tunnel) = host.tunnel.clone() {
            // Phones removed while we were not running must not get back in.
            let synced = match host.phone_keys() {
                Ok(keys) => tunnel.set_allowed(keys).await,
                Err(error) => Err(error),
            };
            if let Err(error) = synced {
                host.shutdown().await?;
                return Err(error.context("cannot sync paired phones with the tunnel"));
            }
            let (tx, rx) = mpsc::unbounded_channel();
            host.joined = Some(tx);
            let paired = host.server()?.on_paired();
            host.linker = Some(tokio::spawn(link::run(
                host.store.clone(),
                tunnel,
                rx,
                paired,
            )));
        }
        Ok(host)
    }

    fn server(&self) -> anyhow::Result<&Server> {
        self.server.as_ref().context("host is shutting down")
    }

    fn phone_keys(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .store
            .list_devices()?
            .into_iter()
            .filter_map(|d| d.phone_key)
            .collect())
    }

    /// Opens a pairing window: a server secret delivered through a tunnel window.
    pub async fn pair(&self, ttl: Duration) -> anyhow::Result<PairInfo> {
        let ttl = ttl.clamp(Duration::from_secs(1), MAX_PAIR_TTL);
        let (Some(tunnel), Some(joined)) = (&self.tunnel, &self.joined) else {
            anyhow::bail!("no tunnel: tailpocket-host is not running");
        };
        let secret = self.server()?.new_pair_secret(ttl);
        let window = tunnel.pair(ttl, secret).await?;
        let info = PairInfo {
            qr: window.qr,
            expires_at: window.expires_at,
        };
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(info.expires_at.saturating_sub(tether_proto::now_ms()))
            + link::GRACE;
        let joined = joined.clone();
        let wait = window.joined;
        tokio::spawn(async move {
            // The sender is dropped without a phone when the window closes.
            if let Ok(phone) = wait.await {
                let _ = joined.send(link::Joined { phone, deadline });
            }
        });
        Ok(info)
    }

    pub fn devices(&self) -> anyhow::Result<Vec<Device>> {
        self.store.list_devices()
    }

    pub async fn revoke(&self, device_id: &str) -> anyhow::Result<()> {
        let device = self
            .server()?
            .revoke(device_id)
            .await?
            .with_context(|| format!("no device {device_id}"))?;
        if let Some(tunnel) = &self.tunnel {
            match &device.phone_key {
                Some(key) => tunnel.revoke(key).await?,
                // Never linked: keep only the phones of the remaining devices.
                None => tunnel.set_allowed(self.phone_keys()?).await?,
            }
        }
        Ok(())
    }

    pub fn agents(&self) -> Vec<AgentInfo> {
        self.agents.iter().map(|a| a.info()).collect()
    }

    pub fn status(&self) -> HostStatus {
        let sessions = self.store.list_sessions().unwrap_or_else(|error| {
            tracing::warn!(%error, "cannot list sessions");
            Vec::new()
        });
        let count = |s: SessionState| sessions.iter().filter(|r| r.info.state == s).count();
        HostStatus {
            tunnel_address: self.tunnel.as_ref().and_then(|t| t.address()),
            gateway: self.gateway,
            connected_devices: self
                .server
                .as_ref()
                .map_or(0, |s| s.connected_devices().len()),
            sessions_working: count(SessionState::Working),
            sessions_need_you: count(SessionState::NeedsYou),
        }
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.joined = None;
        if let Some(linker) = self.linker.take() {
            linker.abort();
            let _ = linker.await;
        }
        if let Some(server) = self.server.take() {
            server.shutdown().await;
        }
        match self.tunnel.take() {
            Some(tunnel) => tunnel.close().await,
            None => Ok(()),
        }
    }
}

fn prepare_dir(cfg: &HostConfig) -> anyhow::Result<PathBuf> {
    let dir = if cfg.data_dir.as_os_str().is_empty() {
        default_data_dir()
    } else {
        cfg.data_dir.clone()
    };
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create data directory {}", dir.display()))?;
    Ok(dir)
}

fn host_name(cfg: &HostConfig, file: Option<String>) -> String {
    [Some(cfg.name.clone()), file]
        .into_iter()
        .flatten()
        .map(|n| n.trim().to_string())
        .find(|n| !n.is_empty())
        .unwrap_or_else(config::host_name)
}

#[cfg(test)]
mod tests;

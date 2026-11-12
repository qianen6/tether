//! Desktop-only DTOs adapt the frozen Host API without changing any shared crate.

use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::Serialize;
use tether_host::{HostConfig, HostStatus, PairInfo};
use tether_proto::AgentInfo;
use tokio::sync::Mutex;

#[cfg(feature = "fake-host")]
use fake::Host;
#[cfg(not(feature = "fake-host"))]
use tether_host::Host;

pub const PAIR_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PairView {
    pub qr: String,
    pub expires_at: u64,
}

impl From<PairInfo> for PairView {
    fn from(pair: PairInfo) -> Self {
        Self {
            qr: pair.qr,
            expires_at: pair.expires_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub paired_at: u64,
    pub last_seen: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StatusView {
    pub tunnel_address: Option<String>,
    pub gateway: String,
    pub connected_devices: usize,
    pub sessions_working: usize,
    pub sessions_need_you: usize,
    pub fake_host: bool,
}

impl From<HostStatus> for StatusView {
    fn from(status: HostStatus) -> Self {
        Self {
            tunnel_address: status.tunnel_address,
            gateway: status.gateway.to_string(),
            connected_devices: status.connected_devices,
            sessions_working: status.sessions_working,
            sessions_need_you: status.sessions_need_you,
            fake_host: cfg!(feature = "fake-host"),
        }
    }
}

struct Inner {
    host: Option<Host>,
    pairing: Option<PairView>,
}

pub struct HostState {
    inner: Mutex<Inner>,
}

impl HostState {
    pub async fn start(config: HostConfig) -> Result<Self> {
        Ok(Self {
            inner: Mutex::new(Inner {
                host: Some(Host::start(config).await?),
                pairing: None,
            }),
        })
    }

    pub async fn pair(&self) -> Result<PairView> {
        let mut inner = self.inner.lock().await;
        let result: PairView = active(&inner)?.pair(PAIR_TTL).await?.into();
        inner.pairing = Some(result.clone());
        Ok(result)
    }

    pub async fn current_pair(&self) -> Option<PairView> {
        self.inner
            .lock()
            .await
            .pairing
            .clone()
            .filter(|pair| pair.expires_at > tether_proto::now_ms())
    }

    pub async fn devices(&self) -> Result<Vec<DeviceView>> {
        let inner = self.inner.lock().await;
        let devices = active(&inner)?.devices()?;
        #[cfg(feature = "fake-host")]
        return Ok(devices);
        #[cfg(not(feature = "fake-host"))]
        Ok(devices
            .into_iter()
            .map(|device| DeviceView {
                id: device.id,
                name: device.name,
                paired_at: device.paired_at,
                last_seen: device.last_seen,
            })
            .collect())
    }

    pub async fn revoke(&self, id: &str) -> Result<()> {
        if id.trim().is_empty() {
            return Err(anyhow!("设备 id 不能为空"));
        }
        let inner = self.inner.lock().await;
        active(&inner)?.revoke(id).await
    }

    pub async fn agents(&self) -> Result<Vec<AgentInfo>> {
        Ok(active(&*self.inner.lock().await)?.agents())
    }

    pub async fn status(&self) -> Result<StatusView> {
        Ok(active(&*self.inner.lock().await)?.status().into())
    }

    pub async fn shutdown(&self) -> Result<()> {
        let host = {
            let mut inner = self.inner.lock().await;
            inner.pairing = None;
            inner.host.take()
        };
        if let Some(host) = host {
            host.shutdown().await?;
        }
        Ok(())
    }
}

fn active(inner: &Inner) -> Result<&Host> {
    inner.host.as_ref().ok_or_else(|| anyhow!("Host 已停止"))
}

pub fn tray_text(status: &StatusView) -> String {
    format!("有 {} 个会话在等你", status.sessions_need_you)
}

#[cfg(feature = "fake-host")]
mod fake {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Mutex;

    use tether_proto::AgentKind;

    use super::*;

    pub struct Host {
        devices: Mutex<Vec<DeviceView>>,
    }

    impl Host {
        pub async fn start(_config: HostConfig) -> Result<Self> {
            Ok(Self {
                devices: Mutex::new(vec![
                    DeviceView {
                        id: "fake-pixel".into(),
                        name: "Pixel 9".into(),
                        paired_at: 1_730_000_000_000,
                        last_seen: Some(1_730_000_060_000),
                    },
                    DeviceView {
                        id: "fake-iphone".into(),
                        name: "iPhone".into(),
                        paired_at: 1_730_000_000_000,
                        last_seen: None,
                    },
                ]),
            })
        }

        pub async fn pair(&self, ttl: Duration) -> Result<PairInfo> {
            Ok(PairInfo {
                qr: "tether1:fake-host-pairing-for-ui-development".into(),
                expires_at: tether_proto::now_ms() + ttl.as_millis() as u64,
            })
        }

        pub fn devices(&self) -> Result<Vec<DeviceView>> {
            Ok(self
                .devices
                .lock()
                .map_err(|_| anyhow!("fake Host device lock poisoned"))?
                .clone())
        }

        pub async fn revoke(&self, id: &str) -> Result<()> {
            self.devices
                .lock()
                .map_err(|_| anyhow!("fake Host device lock poisoned"))?
                .retain(|device| device.id != id);
            Ok(())
        }

        pub fn agents(&self) -> Vec<AgentInfo> {
            vec![
                AgentInfo {
                    id: "codex".into(),
                    name: "Codex".into(),
                    kind: AgentKind::Codex,
                    available: true,
                    detail: None,
                    chat: true,
                    terminal_command: vec!["codex".into()],
                },
                AgentInfo {
                    id: "claude".into(),
                    name: "Claude Agent".into(),
                    kind: AgentKind::Claude,
                    available: false,
                    detail: Some("未找到 claude 命令（演示）".into()),
                    chat: true,
                    terminal_command: vec!["claude".into()],
                },
                AgentInfo {
                    id: "terminal".into(),
                    name: "Terminal".into(),
                    kind: AgentKind::Terminal,
                    available: true,
                    detail: None,
                    chat: false,
                    terminal_command: vec!["shell".into()],
                },
            ]
        }

        pub fn status(&self) -> HostStatus {
            HostStatus {
                tunnel_address: Some("fake-host.local".into()),
                gateway: SocketAddr::from((Ipv4Addr::LOCALHOST, 43123)),
                connected_devices: self.devices().map_or(0, |devices| {
                    devices
                        .iter()
                        .filter(|device| device.last_seen.is_some())
                        .count()
                }),
                sessions_working: 1,
                sessions_need_you: 2,
            }
        }

        pub async fn shutdown(self) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(all(test, feature = "fake-host"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pair_command_returns_qr_and_ten_minute_deadline() {
        let state = HostState::start(HostConfig::default()).await.unwrap();
        assert_eq!(state.current_pair().await, None);
        let before = tether_proto::now_ms();
        let pair = state.pair().await.unwrap();
        assert_eq!(pair.qr, "tether1:fake-host-pairing-for-ui-development");
        assert!(pair.expires_at >= before + 600_000);
        assert!(pair.expires_at <= tether_proto::now_ms() + 600_000);
        assert_eq!(state.current_pair().await, Some(pair));
    }

    #[tokio::test]
    async fn device_and_revoke_commands_change_the_same_host() {
        let state = HostState::start(HostConfig::default()).await.unwrap();
        let devices = state.devices().await.unwrap();
        assert_eq!(devices.len(), 2);
        assert_eq!(state.status().await.unwrap().connected_devices, 1);
        state.revoke(&devices[0].id).await.unwrap();
        assert_eq!(state.devices().await.unwrap(), devices[1..]);
        assert_eq!(state.status().await.unwrap().connected_devices, 0);
        state.revoke(&devices[0].id).await.unwrap();
        assert!(state.revoke(" ").await.is_err());
    }

    #[tokio::test]
    async fn agents_and_status_commands_serialize_for_the_frontend() {
        let state = HostState::start(HostConfig::default()).await.unwrap();
        let agents = state.agents().await.unwrap();
        assert_eq!(agents.len(), 3);
        assert_eq!(agents[1].name, "Claude Agent");
        assert!(!agents[1].available && agents[1].detail.is_some());
        let status = state.status().await.unwrap();
        assert_eq!(tray_text(&status), "有 2 个会话在等你");
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["fake_host"], true);
        assert_eq!(json["gateway"], "127.0.0.1:43123");
        assert_eq!(serde_json::to_value(agents).unwrap()[0]["kind"], "codex");
        let json = serde_json::to_value(state.devices().await.unwrap()).unwrap();
        assert_eq!(json[1]["last_seen"], serde_json::Value::Null);
        assert!(json[0].get("phone_key").is_none());
    }

    #[tokio::test]
    async fn shutdown_is_idempotent_and_commands_report_stopped_host() {
        let state = HostState::start(HostConfig::default()).await.unwrap();
        state.pair().await.unwrap();
        state.shutdown().await.unwrap();
        state.shutdown().await.unwrap();
        assert_eq!(state.current_pair().await, None);
        assert!(state.pair().await.is_err());
        assert!(state.devices().await.is_err());
        assert!(state.revoke("fake-pixel").await.is_err());
        assert!(state.agents().await.is_err());
        assert!(state.status().await.is_err());
    }
}

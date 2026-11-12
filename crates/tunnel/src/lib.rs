//! TailPocket host IPC, durable identity storage, and supervised helper lifetime.
//!
//! No Tailcat protocol is implemented here: the helper owns the tunnel. See
//! `README.md` for timeout, restart, and opt-in live-test behavior.

mod process;
mod protocol;
mod state;
mod supervisor;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use supervisor::Request;

/// Prefix of Tether's pairing QR codes.
pub const QR_PREFIX: &str = "tether1:";

#[derive(Debug, Clone)]
pub struct TunnelConfig {
    /// The `tailpocket-host` executable.
    pub helper: PathBuf,
    /// Where phone connections go: the gateway, `127.0.0.1:<port>`.
    pub forward: SocketAddr,
    /// Node identity and paired phones (private keys: 0600).
    pub state_path: PathBuf,
    /// Computer name shown on the phone.
    pub name: String,
    /// Own DERP relay map instead of Tailcat's public relays.
    pub derp_map_url: Option<String>,
    pub qr_prefix: String,
}

#[derive(Debug)]
pub struct Tunnel {
    requests: mpsc::Sender<Request>,
    address: watch::Receiver<Option<String>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<anyhow::Result<()>>>,
}

#[derive(Debug)]
pub struct PairWindow {
    /// Text for the QR code.
    pub qr: String,
    /// Unix milliseconds.
    pub expires_at: u64,
    /// Fires when a phone joins; the sender is dropped when the window closes.
    pub joined: oneshot::Receiver<PhoneJoined>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhoneJoined {
    /// "nodekey:…"
    pub phone_key: String,
    pub device_name: String,
}

impl Tunnel {
    pub async fn start(cfg: TunnelConfig) -> anyhow::Result<Tunnel> {
        supervisor::start(cfg, process::NativeSpawner, supervisor::Timing::default()).await
    }

    /// Opens a pairing window; `payload` reaches the phone inside the tunnel.
    pub async fn pair(&self, ttl: Duration, payload: String) -> anyhow::Result<PairWindow> {
        let (reply, result) = oneshot::channel();
        self.requests
            .send(Request::Pair {
                ttl,
                payload,
                reply,
            })
            .await
            .context("tunnel supervisor has stopped")?;
        result.await.context("tunnel helper disconnected")?
    }

    pub async fn revoke(&self, phone_key: &str) -> anyhow::Result<()> {
        let (reply, result) = oneshot::channel();
        self.requests
            .send(Request::Revoke {
                phone: phone_key.into(),
                reply,
            })
            .await
            .context("tunnel supervisor has stopped")?;
        result.await.context("tunnel helper disconnected")?
    }

    pub async fn set_allowed(&self, phone_keys: Vec<String>) -> anyhow::Result<()> {
        let (reply, result) = oneshot::channel();
        self.requests
            .send(Request::SetAllowed {
                allowed: phone_keys,
                reply,
            })
            .await
            .context("tunnel supervisor has stopped")?;
        result.await.context("tunnel helper disconnected")?
    }

    /// The node's Tailcat address; `None` while no phone is paired.
    pub fn address(&self) -> Option<String> {
        self.address.borrow().clone()
    }

    pub async fn close(self) -> anyhow::Result<()> {
        let mut tunnel = self;
        if let Some(shutdown) = tunnel.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = tunnel.task.take() {
            task.await.context("tunnel supervisor panicked")??;
        }
        Ok(())
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // Also covers a cancelled `start`/`close` future. The supervisor reaps
        // the child; dropping the JoinHandle alone would just detach its task.
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

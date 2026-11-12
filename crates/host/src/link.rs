//! Ties a phone that joined the tunnel to the device it then paired as.
//!
//! The tunnel reports `PhoneJoined` and the gateway reports the new `Device`
//! on separate channels, in either order, and neither carries the other's id
//! (same problem as `.ref/boop/src/pocket.ts` `link`). Phones must join the
//! tunnel before they can reach the gateway, so the two streams are matched
//! first-in, first-out. Unmatched entries expire; an expired phone that never
//! paired is removed from the tunnel again.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tether_store::{Device, Store};
use tether_tunnel::{PairWindow, PhoneJoined, Tunnel};
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the host needs from the tunnel; a fake in tests.
pub(crate) trait TunnelBackend: Send + Sync + 'static {
    fn pair(&self, ttl: Duration, payload: String) -> BoxFuture<'_, anyhow::Result<PairWindow>>;
    fn revoke<'a>(&'a self, phone_key: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;
    fn set_allowed(&self, phone_keys: Vec<String>) -> BoxFuture<'_, anyhow::Result<()>>;
    fn address(&self) -> Option<String>;
    fn close(self: Arc<Self>) -> BoxFuture<'static, anyhow::Result<()>>;
}

impl TunnelBackend for Tunnel {
    fn pair(&self, ttl: Duration, payload: String) -> BoxFuture<'_, anyhow::Result<PairWindow>> {
        Box::pin(Tunnel::pair(self, ttl, payload))
    }
    fn revoke<'a>(&'a self, phone_key: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(Tunnel::revoke(self, phone_key))
    }
    fn set_allowed(&self, phone_keys: Vec<String>) -> BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(Tunnel::set_allowed(self, phone_keys))
    }
    fn address(&self) -> Option<String> {
        Tunnel::address(self)
    }
    fn close(self: Arc<Self>) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async move {
            match Arc::try_unwrap(self) {
                Ok(tunnel) => tunnel.close().await,
                // Still borrowed somewhere; dropping the last one stops it.
                Err(_) => Ok(()),
            }
        })
    }
}

/// How long a joined phone or a new device waits for its other half.
pub(crate) const GRACE: Duration = Duration::from_secs(120);

pub(crate) struct Joined {
    pub phone: PhoneJoined,
    /// When the pairing window closes (plus [`GRACE`] for the gateway pair).
    pub deadline: Instant,
}

/// Runs until `joined` closes; returns then.
pub(crate) async fn run(
    store: Arc<Store>,
    tunnel: Arc<dyn TunnelBackend>,
    mut joined: mpsc::UnboundedReceiver<Joined>,
    mut paired: broadcast::Receiver<Device>,
) {
    let mut phones: VecDeque<Joined> = VecDeque::new();
    let mut devices: VecDeque<(Device, Instant)> = VecDeque::new();
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            j = joined.recv() => match j {
                Some(j) => phones.push_back(j),
                None => return,
            },
            d = paired.recv() => match d {
                Ok(d) if d.phone_key.is_none() => devices.push_back((d, Instant::now() + GRACE)),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "missed paired devices; their phone keys stay unknown");
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = tick.tick() => {
                let now = Instant::now();
                devices.retain(|(d, deadline)| {
                    let keep = *deadline > now;
                    if !keep {
                        tracing::warn!(device = %d.id, "paired without a tunnel join; phone key unknown");
                    }
                    keep
                });
                while phones.front().is_some_and(|p| p.deadline <= now) {
                    if let Some(p) = phones.pop_front() {
                        tracing::info!("a phone joined the tunnel but never paired; removing it");
                        if let Err(error) = tunnel.revoke(&p.phone.phone_key).await {
                            tracing::warn!(%error, "cannot remove unpaired phone from the tunnel");
                        }
                    }
                }
            }
        }
        while !phones.is_empty() && !devices.is_empty() {
            if let (Some(p), Some((d, _))) = (phones.pop_front(), devices.pop_front()) {
                match store.set_phone_key(&d.id, &p.phone.phone_key) {
                    Ok(()) => tracing::info!(device = %d.id, "phone linked to device"),
                    Err(error) => tracing::warn!(%error, device = %d.id, "cannot save phone key"),
                }
            }
        }
    }
}

//! Tether's loopback-only, authenticated WebSocket gateway.
//! The public API is frozen by `docs/CONTRACTS.md`; backend seams stay private.

use anyhow::ensure;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tether_agent_api::AgentAdapter;
use tether_proto::{ErrorCode, ServerMsg};
use tether_store::{Device, Store};
use tether_terminals::TermManager;
use tokio::{
    net::TcpListener,
    sync::{broadcast, watch},
    task::JoinHandle,
};

mod backends;
mod files;
mod gateway;
mod notify;
mod pairing;
mod sessions;
use backends::{StoreBackend, TermBackend};

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// `127.0.0.1:0` picks a free port. Other addresses are rejected.
    pub bind: SocketAddr,
    pub host_name: String,
}

pub struct Server {
    addr: SocketAddr,
    state: Arc<State>,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

pub(crate) struct State {
    host_name: String,
    store: Arc<dyn StoreBackend>,
    terms: Arc<dyn TermBackend>,
    agents: HashMap<String, Arc<dyn AgentAdapter>>,
    sessions: sessions::Sessions,
    pairing: Mutex<pairing::Pairing>,
    paired: broadcast::Sender<Device>,
    bus: broadcast::Sender<ServerMsg>,
    connections: Mutex<gateway::Connections>,
    connections_changed: tokio::sync::Notify,
    stop: watch::Sender<bool>,
    monitors: Mutex<HashMap<String, JoinHandle<()>>>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
pub(crate) struct Fault {
    code: ErrorCode,
    message: String,
}
impl Fault {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn response(self, req: Option<u64>) -> ServerMsg {
        ServerMsg::error(req, self.code, self.message)
    }
}
impl From<anyhow::Error> for Fault {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(%error, "server backend operation failed");
        Self::new(ErrorCode::Internal, "backend operation failed")
    }
}
type Result<T> = std::result::Result<T, Fault>;

impl Server {
    pub async fn start(
        cfg: ServerConfig,
        store: Arc<Store>,
        terms: Arc<TermManager>,
        agents: Vec<Arc<dyn AgentAdapter>>,
    ) -> anyhow::Result<Server> {
        Self::start_with(cfg, store, terms, agents).await
    }

    pub(crate) async fn start_with(
        cfg: ServerConfig,
        store: Arc<dyn StoreBackend>,
        terms: Arc<dyn TermBackend>,
        agents: Vec<Arc<dyn AgentAdapter>>,
    ) -> anyhow::Result<Server> {
        ensure!(
            cfg.bind.ip() == std::net::Ipv4Addr::LOCALHOST,
            "gateway must bind to 127.0.0.1"
        );
        let mut registry = HashMap::new();
        for agent in agents {
            let id = agent.info().id;
            ensure!(!id.is_empty(), "empty agent id");
            ensure!(registry.insert(id, agent).is_none(), "duplicate agent id");
        }
        let listener = TcpListener::bind(cfg.bind).await?;
        let addr = listener.local_addr()?;
        let (bus, _) = broadcast::channel(256);
        let (paired, _) = broadcast::channel(64);
        let (stop, _) = watch::channel(false);
        let sessions = sessions::Sessions::load(store.clone(), registry.clone(), bus.clone())?;
        let state = Arc::new(State {
            host_name: cfg.host_name,
            store,
            terms,
            agents: registry,
            sessions,
            pairing: Mutex::new(pairing::Pairing::default()),
            paired,
            bus,
            connections: Mutex::new(gateway::Connections::default()),
            connections_changed: tokio::sync::Notify::new(),
            stop,
            monitors: Mutex::new(HashMap::new()),
        });
        for term in state.terms.list().into_iter().filter(|t| t.running) {
            if let Err(error) = notify::monitor(&state, &term.id) {
                tracing::warn!(%error, term = %term.id, "terminal monitoring unavailable");
            }
        }
        let app = gateway::router(state.clone());
        let mut stopping = state.stop.subscribe();
        let task = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopping.wait_for(|v| *v).await;
                })
                .await;
            if let Err(error) = result {
                tracing::error!(%error, "gateway listener stopped");
            }
        });
        Ok(Self { addr, state, task })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn new_pair_secret(&self, ttl: Duration) -> String {
        lock(&self.state.pairing).create(ttl)
    }
    pub fn on_paired(&self) -> broadcast::Receiver<Device> {
        self.state.paired.subscribe()
    }

    pub async fn revoke(&self, device_id: &str) -> anyhow::Result<Option<Device>> {
        // Lookup/registration and revocation share this lock, closing the auth race.
        let device = {
            let mut connections = lock(&self.state.connections);
            let device = self.state.store.revoke_device(device_id)?;
            connections.revoke(device_id);
            device
        };
        self.state.wait_disconnected(Some(device_id)).await;
        Ok(device)
    }

    pub fn connected_devices(&self) -> Vec<String> {
        lock(&self.state.connections).devices()
    }

    pub async fn shutdown(self) {
        self.state.stop.send_replace(true);
        lock(&self.state.connections).shutdown();
        self.state.wait_disconnected(None).await;
        self.state.sessions.shutdown().await;
        for term in self.state.terms.list().into_iter().filter(|t| t.running) {
            if let Err(error) = self.state.terms.kill(&term.id) {
                tracing::warn!(%error, "terminal shutdown failed");
            }
        }
        let monitors = std::mem::take(&mut *lock(&self.state.monitors));
        for (_, task) in monitors {
            task.abort();
            let _ = task.await;
        }
        let _ = self.task.await;
    }
}

impl State {
    async fn wait_disconnected(&self, device: Option<&str>) {
        loop {
            let changed = self.connections_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !lock(&self.connections).contains(device) {
                return;
            }
            changed.await;
        }
    }
}

#[cfg(test)]
mod tests;

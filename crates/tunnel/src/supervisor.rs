use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, sleep, sleep_until, timeout};

use crate::process::{Exit, Process, Spawner};
use crate::protocol::{Command, Config, Connection, Event};
use crate::state::{State, StateFile};
use crate::{PairWindow, PhoneJoined, Tunnel, TunnelConfig};

pub(crate) enum Request {
    Pair {
        ttl: Duration,
        payload: String,
        reply: oneshot::Sender<Result<PairWindow>>,
    },
    Revoke {
        phone: String,
        reply: oneshot::Sender<Result<()>>,
    },
    SetAllowed {
        allowed: Vec<String>,
        reply: oneshot::Sender<Result<()>>,
    },
}

impl Request {
    fn fail(self, message: &str) {
        match self {
            Self::Pair { reply, .. } => {
                let _ = reply.send(Err(anyhow!("{message}")));
            }
            Self::Revoke { reply, .. } | Self::SetAllowed { reply, .. } => {
                let _ = reply.send(Err(anyhow!("{message}")));
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Timing {
    pub startup: Duration,
    pub request: Duration,
    pub close: Duration,
    pub restart_min: Duration,
    pub restart_max: Duration,
    pub stable: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            startup: Duration::from_secs(30),
            request: Duration::from_secs(30),
            close: Duration::from_secs(5),
            restart_min: Duration::from_secs(1),
            restart_max: Duration::from_secs(30),
            stable: Duration::from_secs(60),
        }
    }
}

pub(crate) async fn start(
    cfg: TunnelConfig,
    spawner: impl Spawner,
    timing: Timing,
) -> Result<Tunnel> {
    ensure!(
        cfg.forward.ip().is_loopback() && cfg.forward.port() != 0,
        "tunnel forward must be a nonzero loopback TCP address"
    );
    let file = StateFile(cfg.state_path.clone());
    let state = file.load().await?;
    let (requests, commands) = mpsc::channel(32);
    let (address_tx, address) = watch::channel(None);
    let (stop, shutdown) = oneshot::channel();
    let (ready, started) = oneshot::channel();
    let supervisor = Supervisor {
        cfg,
        state,
        file,
        commands,
        shutdown,
        ready: Some(ready),
        address: address_tx,
        timing,
        serial: 0,
        pending: HashMap::new(),
        joins: HashMap::new(),
    };
    let task = tokio::spawn(supervisor.run(spawner));
    let tunnel = Tunnel {
        requests,
        address,
        shutdown: Some(stop),
        task: Some(task),
    };
    match started
        .await
        .context("tunnel supervisor stopped before ready")?
    {
        Ok(()) => Ok(tunnel),
        Err(error) => {
            let _ = tunnel.close().await;
            Err(error)
        }
    }
}

enum Reply {
    Pair {
        reply: oneshot::Sender<Result<PairWindow>>,
        joined: oneshot::Receiver<PhoneJoined>,
    },
    Allowed(oneshot::Sender<Result<()>>),
}

struct Pending {
    reply: Reply,
    deadline: Instant,
}

impl Pending {
    fn fail(self, message: &str) {
        match self.reply {
            Reply::Pair { reply, .. } => {
                let _ = reply.send(Err(anyhow!("{message}")));
            }
            Reply::Allowed(reply) => {
                let _ = reply.send(Err(anyhow!("{message}")));
            }
        }
    }
}

struct Joining {
    sender: oneshot::Sender<PhoneJoined>,
    deadline: Option<Instant>,
}

struct Supervisor {
    cfg: TunnelConfig,
    state: State,
    file: StateFile,
    commands: mpsc::Receiver<Request>,
    shutdown: oneshot::Receiver<()>,
    ready: Option<oneshot::Sender<Result<()>>>,
    address: watch::Sender<Option<String>>,
    timing: Timing,
    serial: u64,
    pending: HashMap<String, Pending>,
    joins: HashMap<String, Joining>,
}

impl Supervisor {
    async fn run(mut self, mut spawner: impl Spawner) -> Result<()> {
        let mut backoff = Backoff::new(self.timing);
        let mut retry = false;
        loop {
            if retry && self.wait_retry(backoff.next()).await {
                return Ok(());
            }
            let result: Result<()> = match spawner.spawn(&self.cfg) {
                Ok(helper) => {
                    let mut connection = Connection::new(helper.reader, helper.writer);
                    let mut process = helper.process;
                    let began = Instant::now();
                    let result = match connection.send(&Config::new(&self.cfg, &self.state)) {
                        Ok(()) => self.serve(&mut connection, process.as_mut()).await,
                        Err(error) => Err(error),
                    };
                    self.clear("tailpocket-host disconnected; request was not replayed");
                    connection.stop().await; // EOF is the host's shutdown command
                    match result {
                        Ok(()) => {
                            return finish_process(process.as_mut(), self.timing.close, false)
                                .await;
                        }
                        Err(error) => {
                            if let Err(cleanup) =
                                finish_process(process.as_mut(), self.timing.close, true).await
                            {
                                // Never launch two helpers with the same identity when
                                // termination of the previous process is unconfirmed.
                                if let Some(ready) = self.ready.take() {
                                    let _ = ready
                                        .send(Err(anyhow!("helper cleanup failed: {cleanup:#}")));
                                }
                                return Err(cleanup.context("stop old helper before restarting"));
                            }
                            if began.elapsed() >= self.timing.stable {
                                backoff = Backoff::new(self.timing);
                            }
                            Err(error)
                        }
                    }
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                if let Some(ready) = self.ready.take() {
                    let _ = ready.send(Err(error));
                    return Ok(()); // Initial configuration/spawn errors are not retried forever.
                }
                tracing::warn!(error = %error, "tailpocket-host lost; restarting with saved state");
            }
            retry = true;
        }
    }

    async fn wait_retry(&mut self, delay: Duration) -> bool {
        let wait = sleep(delay);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                biased;
                _ = &mut self.shutdown => return true,
                _ = &mut wait => return false,
                request = self.commands.recv() => match request {
                    Some(request) => request.fail("tailpocket-host is restarting; request was not queued"),
                    None => return true,
                }
            }
        }
    }

    async fn serve(
        &mut self,
        connection: &mut Connection,
        process: &mut dyn Process,
    ) -> Result<()> {
        let mut ready = false;
        let startup_deadline = Instant::now() + self.timing.startup;
        loop {
            let deadline = if ready {
                self.next_deadline()
            } else {
                Some(startup_deadline)
            };
            tokio::select! {
                _ = &mut self.shutdown => return Ok(()),
                exit = process.wait() => {
                    let exit = exit.context("wait for tailpocket-host")?;
                    bail!("tailpocket-host exited unexpectedly (code {:?})", exit.code);
                }
                event = connection.next() => {
                    match event? {
                        Some(event) => self.event(event, connection, &mut ready).await?,
                        None => bail!("tailpocket-host stdout closed unexpectedly"),
                    }
                }
                command = self.commands.recv() => match command {
                    None => return Ok(()),
                    Some(command) if !ready => command.fail("tailpocket-host is not ready"),
                    Some(command) => self.command(command, connection)?,
                },
                _ = until(deadline) => {
                    ensure!(ready, "tailpocket-host ready timed out");
                    self.expire(connection)?;
                }
            }
        }
    }

    fn command(&mut self, request: Request, connection: &Connection) -> Result<()> {
        self.serial = self
            .serial
            .checked_add(1)
            .context("helper request id exhausted")?;
        let id = format!("t{}", self.serial);
        let reply = match request {
            Request::Pair {
                ttl,
                payload,
                reply,
            } => {
                if reply.is_closed() {
                    return Ok(());
                }
                // Match upstream: zero means its 120-second default; cap at 900.
                // Round nonzero subsecond TTLs up, never accidentally send zero.
                let seconds = if ttl.is_zero() {
                    0
                } else {
                    ttl.as_secs()
                        .saturating_add(u64::from(ttl.subsec_nanos() != 0))
                        .min(900)
                };
                let (sender, joined) = oneshot::channel();
                self.joins.insert(
                    id.clone(),
                    Joining {
                        sender,
                        deadline: None,
                    },
                );
                self.pending.insert(
                    id.clone(),
                    Pending {
                        deadline: Instant::now() + self.timing.request,
                        reply: Reply::Pair { reply, joined },
                    },
                );
                let result = connection.send(&Command::Pair {
                    id: &id,
                    ttl: seconds,
                    payload: &payload,
                    qr_prefix: &self.cfg.qr_prefix,
                });
                if let Err(error) = result {
                    if let Some(pending) = self.pending.remove(&id) {
                        pending.fail(&error.to_string());
                    }
                    self.joins.remove(&id);
                    return Err(error);
                }
                return Ok(());
            }
            Request::Revoke { phone, reply } => {
                if reply.is_closed() {
                    return Ok(());
                }
                if let Err(error) = connection.send(&Command::Revoke {
                    id: &id,
                    phone: &phone,
                }) {
                    let _ = reply.send(Err(anyhow!("{error}")));
                    return Err(error);
                }
                Reply::Allowed(reply)
            }
            Request::SetAllowed { allowed, reply } => {
                if reply.is_closed() {
                    return Ok(());
                }
                if let Err(error) = connection.send(&Command::SetAllowed {
                    id: &id,
                    allowed: &allowed,
                }) {
                    let _ = reply.send(Err(anyhow!("{error}")));
                    return Err(error);
                }
                Reply::Allowed(reply)
            }
        };
        self.pending.insert(
            id,
            Pending {
                reply,
                deadline: Instant::now() + self.timing.request,
            },
        );
        Ok(())
    }

    async fn event(
        &mut self,
        event: Event,
        connection: &mut Connection,
        ready: &mut bool,
    ) -> Result<()> {
        match event {
            Event::Ready { address } => {
                self.set_address(address);
                *ready = true;
                if let Some(started) = self.ready.take() {
                    let _ = started.send(Ok(()));
                }
            }
            Event::Save { id, state } => {
                let saved = self.file.save(state.clone()).await;
                let ok = saved.is_ok();
                match saved {
                    Ok(()) => self.state = state,
                    Err(error) => {
                        tracing::error!(error = %error, "refusing unsaved tunnel state change")
                    }
                }
                connection.send(&Command::Saved { id: &id, ok })?;
            }
            Event::Pairing { id, qr, expires_at } => {
                let expires_at = expires_at
                    .checked_mul(1000)
                    .context("pairing expiry overflows milliseconds")?;
                ensure!(
                    expires_at <= now_ms().saturating_add(901_000),
                    "helper pairing expiry exceeds 15 minutes"
                );
                match self.pending.remove(&id) {
                    Some(Pending {
                        reply: Reply::Pair { reply, joined },
                        ..
                    }) => {
                        if let Some(join) = self.joins.get_mut(&id) {
                            join.deadline = Some(
                                Instant::now()
                                    + Duration::from_millis(expires_at.saturating_sub(now_ms())),
                            );
                        }
                        if reply
                            .send(Ok(PairWindow {
                                qr,
                                expires_at,
                                joined,
                            }))
                            .is_err()
                        {
                            self.joins.remove(&id);
                            connection.send(&Command::Cancel { id: &id })?;
                        }
                    }
                    Some(pending) => {
                        pending.fail("unexpected pairing response");
                        bail!("helper response type mismatch");
                    }
                    None => {
                        connection.send(&Command::Cancel { id: &id })?;
                    }
                }
            }
            Event::Paired {
                id,
                phone,
                device,
                address,
            } => {
                self.set_address(address);
                if let Some(join) = self.joins.remove(&id) {
                    let _ = join.sender.send(PhoneJoined {
                        phone_key: phone,
                        device_name: device,
                    });
                }
            }
            Event::PairingClosed { id, reason } => {
                self.joins.remove(&id);
                if let Some(pending) = self.pending.remove(&id) {
                    pending.fail(&format!("pairing closed: {reason}"));
                }
            }
            Event::Allowed {
                id,
                allowed,
                address,
            } => {
                ensure!(allowed.len() <= 256, "helper allowlist exceeds 256 phones");
                self.set_address(address);
                match self.pending.remove(&id) {
                    Some(Pending {
                        reply: Reply::Allowed(reply),
                        ..
                    }) => {
                        let _ = reply.send(Ok(()));
                    }
                    Some(pending) => {
                        pending.fail("unexpected allowed response");
                        bail!("helper response type mismatch");
                    }
                    None => {}
                }
            }
            Event::Error { id: Some(id), code } => {
                self.joins.remove(&id);
                if let Some(pending) = self.pending.remove(&id) {
                    pending.fail(&format!("tailpocket-host: {code}"));
                }
            }
            Event::Error { id: None, code } => bail!("tailpocket-host: {code}"),
            Event::Unknown => {}
        }
        Ok(())
    }

    fn set_address(&self, address: String) {
        self.address
            .send_replace((!address.is_empty()).then_some(address));
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|p| p.deadline)
            .chain(self.joins.values().filter_map(|p| p.deadline))
            .min()
    }

    fn expire(&mut self, connection: &Connection) -> Result<()> {
        let now = Instant::now();
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(pending) = self.pending.remove(&id) {
                pending.fail("tailpocket-host request timed out; not replayed");
            }
            if self.joins.remove(&id).is_some() {
                connection.send(&Command::Cancel { id: &id })?;
            }
        }
        let expired: Vec<_> = self
            .joins
            .iter()
            .filter(|(_, p)| p.deadline.is_some_and(|d| d <= now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.joins.remove(&id); // Drop sender even if helper never emits pairingClosed.
            connection.send(&Command::Cancel { id: &id })?;
        }
        Ok(())
    }

    fn clear(&mut self, message: &str) {
        self.address.send_replace(None);
        for (_, pending) in self.pending.drain() {
            pending.fail(message);
        }
        self.joins.clear();
    }
}

async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

async fn finish_process(process: &mut dyn Process, grace: Duration, force: bool) -> Result<()> {
    if force {
        process
            .kill()
            .context("kill disconnected tailpocket-host")?;
    }
    match timeout(grace, process.wait()).await {
        Ok(exit) => {
            let Exit { success, code } = exit.context("reap tailpocket-host")?;
            ensure!(
                force || success,
                "tailpocket-host shutdown failed (code {code:?})"
            );
        }
        Err(_) => {
            process
                .kill()
                .context("kill unresponsive tailpocket-host")?;
            timeout(grace, process.wait())
                .await
                .context("tailpocket-host did not exit after kill")?
                .context("reap killed tailpocket-host")?;
        }
    }
    Ok(())
}

struct Backoff {
    next: Duration,
    maximum: Duration,
}

impl Backoff {
    fn new(timing: Timing) -> Self {
        Self {
            next: timing.restart_min,
            maximum: timing.restart_max,
        }
    }
    fn next(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(self.maximum);
        delay
    }
}

#[cfg(test)]
mod tests;

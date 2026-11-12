//! KeepAwake's Windows implementation is thread-affine. Create AND drop its guard
//! on one dedicated thread, never on Tauri/Tokio's migrating command workers.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};

use anyhow::{Result, anyhow};
use tokio::sync::oneshot;

enum Request {
    Set(bool, oneshot::Sender<Result<()>>),
    Stop,
}

pub struct AwakeController {
    sender: mpsc::Sender<Request>,
    enabled: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl AwakeController {
    #[cfg(test)]
    pub fn fake() -> Result<Self> {
        Self::start_with(|| Ok(()))
    }

    pub fn new() -> Result<Self> {
        Self::start_with(|| {
            Ok(keepawake::Builder::default()
                .idle(true)
                .reason("Tether remote agent sessions")
                .app_name("Tether")
                .app_reverse_domain("dev.tether.desktop")
                .create()?)
        })
    }

    fn start_with<F, G>(create: F) -> Result<Self>
    where
        F: Fn() -> Result<G> + Send + 'static,
        G: 'static,
    {
        let (sender, receiver) = mpsc::channel();
        let enabled = Arc::new(AtomicBool::new(false));
        let state = enabled.clone();
        let worker = thread::Builder::new()
            .name("tether-keepawake".into())
            .spawn(move || {
                let mut guard = None;
                while let Ok(request) = receiver.recv() {
                    match request {
                        Request::Set(value, reply) => {
                            let result = if value && guard.is_none() {
                                create().map(|created| {
                                    guard = Some(created);
                                })
                            } else {
                                if !value {
                                    guard = None;
                                }
                                Ok(())
                            };
                            state.store(guard.is_some(), Ordering::Release);
                            let _ = reply.send(result);
                        }
                        Request::Stop => break,
                    }
                }
                drop(guard);
                state.store(false, Ordering::Release);
            })?;
        Ok(Self {
            sender,
            enabled,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub async fn set(&self, enabled: bool) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Request::Set(enabled, reply))
            .map_err(|_| anyhow!("休眠控制线程已停止"))?;
        result
            .await
            .map_err(|_| anyhow!("休眠控制线程未返回结果"))?
    }

    pub fn shutdown(&self) {
        let _ = self.sender.send(Request::Stop);
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
            && worker.join().is_err()
        {
            tracing::error!("keep-awake worker panicked");
        }
    }
}

impl Drop for AwakeController {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Guard(mpsc::Sender<thread::ThreadId>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.send(thread::current().id()).unwrap();
        }
    }

    #[tokio::test]
    async fn enable_disable_and_exit_release_on_the_creating_thread() {
        let (tx, rx) = mpsc::channel();
        let awake = AwakeController::start_with(move || {
            tx.send(thread::current().id()).unwrap();
            Ok(Guard(tx.clone()))
        })
        .unwrap();
        awake.set(true).await.unwrap();
        let created = rx.recv().unwrap();
        awake.set(true).await.unwrap(); // idempotent: no second guard
        assert!(rx.try_recv().is_err());
        assert!(awake.is_enabled());
        awake.set(false).await.unwrap();
        assert_eq!(rx.recv().unwrap(), created);
        assert!(!awake.is_enabled());
        awake.set(true).await.unwrap();
        assert_eq!(rx.recv().unwrap(), created);
        awake.shutdown();
        assert_eq!(rx.recv().unwrap(), created);
        assert!(!awake.is_enabled());
        assert!(awake.set(true).await.is_err());
    }

    #[tokio::test]
    async fn failed_enable_is_reported_without_changing_state() {
        let awake =
            AwakeController::start_with(|| Err::<(), _>(anyhow!("test power error"))).unwrap();
        assert_eq!(
            awake.set(true).await.unwrap_err().to_string(),
            "test power error"
        );
        assert!(!awake.is_enabled());
        awake.set(false).await.unwrap();
    }
}

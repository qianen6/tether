use crate::{
    backends::{StoreBackend, TermBackend},
    lock,
};
use anyhow::{Context, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use subtle::ConstantTimeEq;
use tether_proto::*;
use tether_store::{Device, SessionRecord};
use tether_terminals::TermChunk;
use tokio::sync::broadcast;

#[derive(Default)]
pub(super) struct MemoryStore {
    data: Mutex<Data>,
    pub fail_append: AtomicBool,
    pub fail_upsert: AtomicBool,
    pub fail_auth: AtomicBool,
    pub fail_delete: AtomicBool,
    pub reads: AtomicUsize,
    pub history_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    pub delete_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
#[derive(Default)]
struct Data {
    devices: HashMap<String, (Device, [u8; 32])>,
    sessions: HashMap<String, SessionRecord>,
    events: HashMap<String, BTreeMap<u64, Event>>,
}
fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl MemoryStore {
    pub fn records(&self) -> Vec<SessionRecord> {
        self.list_sessions().unwrap()
    }
    pub fn history(&self, id: &str) -> Vec<Event> {
        self.events_after(id, 0, usize::MAX).unwrap()
    }
    pub fn seed(&self, record: SessionRecord, events: Vec<Event>) {
        let mut data = lock(&self.data);
        data.events.insert(
            record.info.id.clone(),
            events.into_iter().map(|e| (e.seq, e)).collect(),
        );
        data.sessions.insert(record.info.id.clone(), record);
    }
}
impl StoreBackend for MemoryStore {
    fn issue_device(&self, name: &str) -> anyhow::Result<(Device, String)> {
        ensure!(!self.fail_auth.load(Ordering::SeqCst), "auth failed");
        let device = Device {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            phone_key: Some("fixture-phone-key".into()),
            paired_at: now_ms(),
            last_seen: None,
        };
        let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        lock(&self.data)
            .devices
            .insert(device.id.clone(), (device.clone(), hash(&token)));
        Ok((device, token))
    }
    fn device_by_token(&self, token: &str) -> anyhow::Result<Option<Device>> {
        ensure!(!self.fail_auth.load(Ordering::SeqCst), "auth failed");
        let wanted = hash(token);
        Ok(lock(&self.data)
            .devices
            .values()
            .find(|(_, stored)| bool::from(stored.ct_eq(&wanted)))
            .map(|(d, _)| d.clone()))
    }
    fn touch_device(&self, id: &str) -> anyhow::Result<()> {
        lock(&self.data)
            .devices
            .get_mut(id)
            .context("device not found")?
            .0
            .last_seen = Some(now_ms());
        Ok(())
    }
    fn revoke_device(&self, id: &str) -> anyhow::Result<Option<Device>> {
        Ok(lock(&self.data).devices.remove(id).map(|(d, _)| d))
    }
    fn list_sessions(&self) -> anyhow::Result<Vec<SessionRecord>> {
        Ok(lock(&self.data).sessions.values().cloned().collect())
    }
    fn upsert_session(&self, record: &SessionRecord) -> anyhow::Result<()> {
        ensure!(!self.fail_upsert.load(Ordering::SeqCst), "upsert failed");
        lock(&self.data)
            .sessions
            .insert(record.info.id.clone(), record.clone());
        Ok(())
    }
    fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        ensure!(!self.fail_delete.load(Ordering::SeqCst), "delete failed");
        {
            let mut data = lock(&self.data);
            data.sessions.remove(id);
            data.events.remove(id);
        }
        let hook = lock(&self.delete_hook).take();
        if let Some(hook) = hook {
            hook();
        }
        Ok(())
    }
    fn append_event(&self, id: &str, event: &Event) -> anyhow::Result<()> {
        ensure!(!self.fail_append.load(Ordering::SeqCst), "append failed");
        ensure!(!event.kind.is_transient(), "transient event persisted");
        let mut data = lock(&self.data);
        ensure!(data.sessions.contains_key(id), "session not found");
        let events = data.events.entry(id.into()).or_default();
        ensure!(
            events
                .last_key_value()
                .is_none_or(|(seq, _)| *seq < event.seq),
            "seq must increase"
        );
        events.insert(event.seq, event.clone());
        Ok(())
    }
    fn events_after(&self, id: &str, after: u64, limit: usize) -> anyhow::Result<Vec<Event>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let events = lock(&self.data)
            .events
            .get(id)
            .map(|events| {
                events
                    .values()
                    .filter(|e| e.seq > after)
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let hook = lock(&self.history_hook).take();
        if let Some(hook) = hook {
            hook();
        }
        Ok(events)
    }
}

#[derive(Default)]
pub(super) struct MemoryTerms {
    data: Mutex<HashMap<String, Terminal>>,
    pub fail_open: AtomicBool,
}
struct Terminal {
    info: TermInfo,
    scrollback: Vec<u8>,
    chunks: broadcast::Sender<TermChunk>,
}
impl MemoryTerms {
    pub fn emit(&self, id: &str, chunk: TermChunk) {
        let mut terms = lock(&self.data);
        let terminal = terms.get_mut(id).unwrap();
        if let TermChunk::Data(bytes) = &chunk {
            terminal.scrollback.extend(bytes);
        }
        if let TermChunk::Exit(_) = &chunk {
            terminal.info.running = false;
        }
        let _ = terminal.chunks.send(chunk);
    }
}
impl TermBackend for MemoryTerms {
    fn open(
        &self,
        command: Vec<String>,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<TermInfo> {
        ensure!(!self.fail_open.load(Ordering::SeqCst), "open failed");
        let info = TermInfo {
            id: uuid::Uuid::new_v4().to_string(),
            command,
            cwd: cwd.to_string_lossy().into_owned(),
            cols,
            rows,
            running: true,
            created_at: now_ms(),
        };
        let (chunks, _) = broadcast::channel(256);
        lock(&self.data).insert(
            info.id.clone(),
            Terminal {
                info: info.clone(),
                scrollback: b"ready\n".to_vec(),
                chunks,
            },
        );
        Ok(info)
    }
    fn attach(&self, id: &str) -> anyhow::Result<(Vec<u8>, broadcast::Receiver<TermChunk>)> {
        let terms = lock(&self.data);
        let terminal = terms.get(id).context("term not found")?;
        Ok((terminal.scrollback.clone(), terminal.chunks.subscribe()))
    }
    fn input(&self, id: &str, bytes: &[u8]) -> anyhow::Result<()> {
        ensure!(
            lock(&self.data)
                .get(id)
                .context("term not found")?
                .info
                .running,
            "term exited"
        );
        self.emit(id, TermChunk::Data(bytes.to_vec().into()));
        Ok(())
    }
    fn resize(&self, id: &str, cols: u16, rows: u16) -> anyhow::Result<()> {
        let mut terms = lock(&self.data);
        let terminal = terms.get_mut(id).context("term not found")?;
        terminal.info.cols = cols;
        terminal.info.rows = rows;
        Ok(())
    }
    fn kill(&self, id: &str) -> anyhow::Result<()> {
        ensure!(lock(&self.data).contains_key(id), "term not found");
        self.emit(id, TermChunk::Exit(Some(0)));
        Ok(())
    }
    fn list(&self) -> Vec<TermInfo> {
        lock(&self.data).values().map(|t| t.info.clone()).collect()
    }
}

//! Private seams let T03 run while T02/T07 are still scaffolds.
use std::path::PathBuf;
use tether_proto::{Event, TermInfo};
use tether_store::{Device, SessionRecord, Store};
use tether_terminals::{TermChunk, TermManager};
use tokio::sync::broadcast;

pub(crate) trait StoreBackend: Send + Sync {
    fn issue_device(&self, name: &str) -> anyhow::Result<(Device, String)>;
    fn device_by_token(&self, token: &str) -> anyhow::Result<Option<Device>>;
    fn touch_device(&self, id: &str) -> anyhow::Result<()>;
    fn revoke_device(&self, id: &str) -> anyhow::Result<Option<Device>>;
    fn list_sessions(&self) -> anyhow::Result<Vec<SessionRecord>>;
    fn upsert_session(&self, record: &SessionRecord) -> anyhow::Result<()>;
    fn delete_session(&self, id: &str) -> anyhow::Result<()>;
    fn append_event(&self, id: &str, event: &Event) -> anyhow::Result<()>;
    fn events_after(&self, id: &str, after: u64, limit: usize) -> anyhow::Result<Vec<Event>>;
}
impl StoreBackend for Store {
    fn issue_device(&self, name: &str) -> anyhow::Result<(Device, String)> {
        Store::issue_device(self, name)
    }
    fn device_by_token(&self, token: &str) -> anyhow::Result<Option<Device>> {
        Store::device_by_token(self, token)
    }
    fn touch_device(&self, id: &str) -> anyhow::Result<()> {
        Store::touch_device(self, id)
    }
    fn revoke_device(&self, id: &str) -> anyhow::Result<Option<Device>> {
        Store::revoke_device(self, id)
    }
    fn list_sessions(&self) -> anyhow::Result<Vec<SessionRecord>> {
        Store::list_sessions(self)
    }
    fn upsert_session(&self, record: &SessionRecord) -> anyhow::Result<()> {
        Store::upsert_session(self, record)
    }
    fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        Store::delete_session(self, id)
    }
    fn append_event(&self, id: &str, event: &Event) -> anyhow::Result<()> {
        Store::append_event(self, id, event)
    }
    fn events_after(&self, id: &str, after: u64, limit: usize) -> anyhow::Result<Vec<Event>> {
        Store::events_after(self, id, after, limit)
    }
}

pub(crate) trait TermBackend: Send + Sync {
    fn open(
        &self,
        command: Vec<String>,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<TermInfo>;
    fn attach(&self, term: &str) -> anyhow::Result<(Vec<u8>, broadcast::Receiver<TermChunk>)>;
    fn input(&self, term: &str, bytes: &[u8]) -> anyhow::Result<()>;
    fn resize(&self, term: &str, cols: u16, rows: u16) -> anyhow::Result<()>;
    fn kill(&self, term: &str) -> anyhow::Result<()>;
    fn list(&self) -> Vec<TermInfo>;
}
impl TermBackend for TermManager {
    fn open(
        &self,
        command: Vec<String>,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<TermInfo> {
        TermManager::open(self, command, cwd, cols, rows)
    }
    fn attach(&self, term: &str) -> anyhow::Result<(Vec<u8>, broadcast::Receiver<TermChunk>)> {
        TermManager::attach(self, term)
    }
    fn input(&self, term: &str, bytes: &[u8]) -> anyhow::Result<()> {
        TermManager::input(self, term, bytes)
    }
    fn resize(&self, term: &str, cols: u16, rows: u16) -> anyhow::Result<()> {
        TermManager::resize(self, term, cols, rows)
    }
    fn kill(&self, term: &str) -> anyhow::Result<()> {
        TermManager::kill(self, term)
    }
    fn list(&self) -> Vec<TermInfo> {
        TermManager::list(self)
    }
}

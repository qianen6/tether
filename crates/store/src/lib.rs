//! SQLite storage for devices, sessions and their events.
//!
//! A connection is protected by a mutex; write transactions also serialize
//! read/modify/write operations across independently opened stores. Sequence
//! numbers are checked against persisted events, so gaps left by transient
//! events (and session snapshots saved before an append) are supported.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{TryRng, rngs::SysRng};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params, types::Type};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tether_proto::{Event, SessionInfo};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 4;

const SCHEMA_V1: &str = r#"
CREATE TABLE devices (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    token_hash  BLOB NOT NULL UNIQUE CHECK (typeof(token_hash) = 'blob' AND length(token_hash) = 32),
    phone_key   TEXT,
    paired_at   INTEGER NOT NULL CHECK (paired_at >= 0),
    last_seen   INTEGER CHECK (last_seen >= 0)
);
CREATE TABLE sessions (
    id          TEXT PRIMARY KEY NOT NULL,
    agent       TEXT NOT NULL,
    title       TEXT NOT NULL,
    cwd         TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('idle', 'working', 'needs_you', 'error', 'exited')),
    status_line TEXT,
    created_at  INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at  INTEGER NOT NULL CHECK (updated_at >= 0),
    last_seq    INTEGER NOT NULL CHECK (last_seq >= 0),
    native_id   TEXT
);
CREATE TABLE events (
    session     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL CHECK (seq > 0),
    at          INTEGER NOT NULL CHECK (at >= 0),
    json        TEXT NOT NULL,
    PRIMARY KEY (session, seq)
);
"#;

/// v2: the model a session was told to use; chats without a project.
const SCHEMA_V2: &str = r#"
ALTER TABLE sessions ADD COLUMN model TEXT;
ALTER TABLE sessions ADD COLUMN effort TEXT;
ALTER TABLE sessions ADD COLUMN chat INTEGER NOT NULL DEFAULT 0;
"#;

/// v3: the speed a session was told to run at.
const SCHEMA_V3: &str = r#"
ALTER TABLE sessions ADD COLUMN speed TEXT;
"#;

/// v4: what a session's agent may do without asking.
const SCHEMA_V4: &str = r#"
ALTER TABLE sessions ADD COLUMN permission TEXT;
"#;

/// Synchronous; internally `Mutex<rusqlite::Connection>`. Every call is quick,
/// so async callers use it directly.
#[derive(Debug)]
pub struct Store {
    connection: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// The phone's TailPocket key ("nodekey:…"), once known.
    pub phone_key: Option<String>,
    pub paired_at: u64,
    pub last_seen: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub info: SessionInfo,
    /// The agent's own session id, for resuming.
    pub native_id: Option<String>,
}

impl Store {
    /// Opens or creates the database and migrates it.
    pub fn open(path: &Path) -> anyhow::Result<Store> {
        Self::from_connection(Connection::open(path).context("open store database")?)
    }

    pub fn open_in_memory() -> anyhow::Result<Store> {
        Self::from_connection(Connection::open_in_memory().context("open in-memory store")?)
    }

    fn from_connection(mut connection: Connection) -> anyhow::Result<Store> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let journal_mode: String =
            connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        // SQLite in-memory databases use `memory` instead of WAL.
        ensure!(
            journal_mode == "wal" || journal_mode == "memory",
            "store requires WAL (or an in-memory database)"
        );

        // Acquire the write lock *before* reading user_version: another Store
        // may be opening the same new file at the same time.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            (0..=SCHEMA_VERSION).contains(&version),
            "unsupported store schema version {version} (expected {SCHEMA_VERSION})"
        );
        if version == 0 {
            transaction
                .execute_batch(SCHEMA_V1)
                .context("migrate store to schema v1")?;
        }
        if version < 2 {
            transaction
                .execute_batch(SCHEMA_V2)
                .context("migrate store to schema v2")?;
        }
        if version < 3 {
            transaction
                .execute_batch(SCHEMA_V3)
                .context("migrate store to schema v3")?;
        }
        if version < 4 {
            transaction
                .execute_batch(SCHEMA_V4)
                .context("migrate store to schema v4")?;
        }
        if version != SCHEMA_VERSION {
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        transaction.commit()?;

        Ok(Store {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> anyhow::Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow::anyhow!("store connection mutex poisoned"))
    }

    // ---- devices

    /// Returns the new device and its token in clear text; only the token's
    /// SHA-256 is stored.
    pub fn issue_device(&self, name: &str) -> anyhow::Result<(Device, String)> {
        let mut bytes = [0_u8; 32];
        SysRng
            .try_fill_bytes(&mut bytes)
            .context("generate device token")?;
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let device = Device {
            id: Uuid::new_v4().to_string(),
            name: name.to_owned(),
            phone_key: None,
            paired_at: tether_proto::now_ms(),
            last_seen: None,
        };
        self.lock()?.execute(
            "INSERT INTO devices (id, name, token_hash, phone_key, paired_at, last_seen)
             VALUES (?1, ?2, ?3, NULL, ?4, NULL)",
            params![
                device.id,
                device.name,
                token_hash.as_slice(),
                sqlite_integer(device.paired_at)?
            ],
        )?;
        Ok((device, token))
    }

    /// Constant-time comparison of the token's hash.
    pub fn device_by_token(&self, token: &str) -> anyhow::Result<Option<Device>> {
        let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let connection = self.lock()?;
        // Do not put token_hash in a SQL WHERE clause: SQLite's equality
        // comparison is not constant-time. Compare every fixed-size hash,
        // without stopping early when a matching device is found.
        let mut statement = connection
            .prepare("SELECT id, name, phone_key, paired_at, last_seen, token_hash FROM devices")?;
        let mut rows = statement.query([])?;
        let mut found = None;
        while let Some(row) = rows.next()? {
            let stored_hash: [u8; 32] = row.get(5)?;
            let device = device_from_row(row)?;
            if bool::from(stored_hash.ct_eq(&token_hash)) {
                found = Some(device);
            }
        }
        Ok(found)
    }

    pub fn set_phone_key(&self, device_id: &str, phone_key: &str) -> anyhow::Result<()> {
        let changed = self.lock()?.execute(
            "UPDATE devices SET phone_key = ?2 WHERE id = ?1",
            params![device_id, phone_key],
        )?;
        ensure!(changed == 1, "device not found");
        Ok(())
    }

    /// Updates `last_seen` to now.
    pub fn touch_device(&self, device_id: &str) -> anyhow::Result<()> {
        let changed = self.lock()?.execute(
            "UPDATE devices SET last_seen = ?2 WHERE id = ?1",
            params![device_id, sqlite_integer(tether_proto::now_ms())?],
        )?;
        ensure!(changed == 1, "device not found");
        Ok(())
    }

    pub fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id, name, phone_key, paired_at, last_seen
             FROM devices ORDER BY paired_at, id",
        )?;
        statement
            .query_map([], device_from_row)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Into::into)
    }

    /// Deletes the device and returns it (with its phone key), if it existed.
    pub fn revoke_device(&self, device_id: &str) -> anyhow::Result<Option<Device>> {
        Ok(self
            .lock()?
            .query_row(
                "DELETE FROM devices WHERE id = ?1
             RETURNING id, name, phone_key, paired_at, last_seen",
                [device_id],
                device_from_row,
            )
            .optional()?)
    }

    // ---- sessions

    pub fn upsert_session(&self, s: &SessionRecord) -> anyhow::Result<()> {
        let state = serde_json::to_value(s.info.state)?;
        let state = state
            .as_str()
            .context("session state must serialize as a string")?;
        self.lock()?.execute(
            "INSERT INTO sessions
                (id, agent, title, cwd, state, status_line, created_at, updated_at, last_seq, native_id, model, effort, chat, speed, permission)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(id) DO UPDATE SET
                agent = excluded.agent, title = excluded.title, cwd = excluded.cwd,
                state = excluded.state, status_line = excluded.status_line,
                created_at = excluded.created_at, updated_at = excluded.updated_at,
                last_seq = MAX(sessions.last_seq, excluded.last_seq), native_id = excluded.native_id,
                model = excluded.model, effort = excluded.effort, chat = excluded.chat,
                speed = excluded.speed, permission = excluded.permission",
            params![s.info.id, s.info.agent, s.info.title, s.info.cwd, state,
                s.info.status_line, sqlite_integer(s.info.created_at)?, sqlite_integer(s.info.updated_at)?,
                sqlite_integer(s.info.last_seq)?, s.native_id, s.info.model, s.info.effort, s.info.chat, s.info.speed, s.info.permission],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionRecord>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT id, agent, title, cwd, state, status_line, created_at, updated_at, last_seq, native_id, model, effort, chat, speed, permission
             FROM sessions ORDER BY updated_at DESC, id",
        )?;
        statement
            .query_map([], session_from_row)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Into::into)
    }

    /// Also deletes the session's events.
    pub fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        self.lock()?
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- events (never transient ones)

    /// Fails for transient events and for a `seq` not above the last one.
    pub fn append_event(&self, session: &str, e: &Event) -> anyhow::Result<()> {
        ensure!(
            !e.kind.is_transient(),
            "transient events must not be persisted"
        );
        let seq = sqlite_integer(e.seq)?;
        let at = sqlite_integer(e.at)?;
        let json = serde_json::to_string(e).context("serialize event")?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let last_seq: i64 = transaction
            .query_row(
                "SELECT COALESCE((SELECT MAX(seq) FROM events WHERE session = sessions.id), 0)
             FROM sessions WHERE id = ?1",
                [session],
                |row| row.get(0),
            )
            .optional()?
            .context("session not found")?;
        ensure!(
            seq > last_seq,
            "event seq must be strictly increasing (last: {last_seq})"
        );
        transaction.execute(
            "INSERT INTO events (session, seq, at, json) VALUES (?1, ?2, ?3, ?4)",
            params![session, seq, at, json],
        )?;
        transaction.execute(
            "UPDATE sessions SET last_seq = MAX(last_seq, ?2), updated_at = MAX(updated_at, ?3)
             WHERE id = ?1",
            params![session, seq, at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Ascending by seq, at most `limit`.
    pub fn events_after(
        &self,
        session: &str,
        after: u64,
        limit: usize,
    ) -> anyhow::Result<Vec<Event>> {
        if limit == 0 || after >= i64::MAX as u64 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT json FROM events WHERE session = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
        )?;
        let mut rows = statement.query(params![session, sqlite_integer(after)?, limit])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            events.push(serde_json::from_str(&json).context("deserialize stored event")?);
        }
        Ok(events)
    }
}

// SQLite INTEGERs are signed. Check both directions rather than allowing
// protocol u64s to wrap, truncate, or silently become floating-point values.
fn sqlite_integer(value: u64) -> anyhow::Result<i64> {
    i64::try_from(value).context("value exceeds SQLite INTEGER range")
}

fn row_unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}

fn device_from_row(row: &Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        id: row.get(0)?,
        name: row.get(1)?,
        phone_key: row.get(2)?,
        paired_at: row_unsigned(row, 3)?,
        last_seen: row
            .get::<_, Option<i64>>(4)?
            .map(|value| {
                u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, value))
            })
            .transpose()?,
    })
}

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    let state: String = row.get(4)?;
    let state = serde_json::from_value(serde_json::Value::String(state)).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(error))
    })?;
    Ok(SessionRecord {
        info: SessionInfo {
            id: row.get(0)?,
            agent: row.get(1)?,
            title: row.get(2)?,
            cwd: row.get(3)?,
            state,
            status_line: row.get(5)?,
            created_at: row_unsigned(row, 6)?,
            updated_at: row_unsigned(row, 7)?,
            last_seq: row_unsigned(row, 8)?,
            model: row.get(10)?,
            effort: row.get(11)?,
            chat: row.get(12)?,
            speed: row.get(13)?,
            permission: row.get(14)?,
            // The same column: the agent's own id is the conversation's.
            thread: row.get(9)?,
        },
        native_id: row.get(9)?,
    })
}

#[cfg(test)]
mod tests;

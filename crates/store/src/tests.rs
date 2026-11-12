use std::sync::{Arc, Barrier};
use std::thread;

use tether_proto::{Approval, ApprovalKind, Decision, EventKind, NoticeLevel, SessionState};

use super::*;

fn record(id: &str) -> SessionRecord {
    SessionRecord {
        info: SessionInfo {
            id: id.into(),
            agent: "echo".into(),
            title: "测试会话".into(),
            cwd: "C:\\工作\\项目".into(),
            state: SessionState::Idle,
            status_line: None,
            created_at: 10,
            updated_at: 10,
            last_seq: 0,
            model: None,
            effort: None,
            chat: false,
            speed: None,
            thread: None,
            permission: None,
        },
        native_id: None,
    }
}

fn event(seq: u64) -> Event {
    Event {
        seq,
        at: 10 + seq,
        kind: EventKind::UserText {
            text: format!("消息 {seq}\n' ? ;"),
            images: None,
        },
    }
}

fn schema_version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

#[test]
fn store_is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<Store>();
}

#[test]
fn device_lifecycle() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    assert!(store.list_devices()?.is_empty());
    let before = tether_proto::now_ms();
    let (mut device, token) = store.issue_device("手机 '); DROP TABLE devices;--")?;
    let after = tether_proto::now_ms();
    assert!((before..=after).contains(&device.paired_at));
    assert_eq!(device.phone_key, None);
    assert_eq!(device.last_seen, None);
    Uuid::parse_str(&device.id)?;
    assert_eq!(token.len(), 43);
    assert_eq!(URL_SAFE_NO_PAD.decode(&token)?.len(), 32);
    assert!(!token.contains(['+', '/', '=']));
    assert_eq!(store.device_by_token(&token)?, Some(device.clone()));
    for invalid in ["", "invalid-token", "💻", "\0", &(token.clone() + "=")] {
        assert_eq!(store.device_by_token(invalid)?, None);
    }

    store.set_phone_key(&device.id, "nodekey:一")?;
    store.set_phone_key(&device.id, "nodekey:二")?;
    device.phone_key = Some("nodekey:二".into());
    let before_touch = tether_proto::now_ms();
    store.touch_device(&device.id)?;
    device.last_seen = store.list_devices()?[0].last_seen;
    assert!((before_touch..=tether_proto::now_ms()).contains(&device.last_seen.unwrap()));
    assert_eq!(store.list_devices()?, vec![device.clone()]);
    assert_eq!(store.device_by_token(&token)?, Some(device.clone()));

    let (other, other_token) = store.issue_device("第二部")?;
    assert_ne!(token, other_token);
    assert_ne!(device.id, other.id);
    assert_eq!(store.list_devices()?.len(), 2);
    assert_eq!(store.revoke_device(&device.id)?, Some(device.clone()));
    assert_eq!(store.revoke_device(&device.id)?, None);
    assert_eq!(store.device_by_token(&token)?, None);
    assert_eq!(store.device_by_token(&other_token)?, Some(other));
    Ok(())
}

#[test]
fn only_sha256_of_returned_token_is_stored() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    let (device, token) = store.issue_device("phone")?;
    let connection = store.lock()?;
    let hash: Vec<u8> = connection.query_row(
        "SELECT token_hash FROM devices WHERE id = ?1",
        [&device.id],
        |row| row.get(0),
    )?;
    assert_eq!(hash, Sha256::digest(token.as_bytes()).as_slice());
    assert_eq!(hash.len(), 32);
    let columns = connection
        .prepare("PRAGMA table_info(devices)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        columns,
        [
            "id",
            "name",
            "token_hash",
            "phone_key",
            "paired_at",
            "last_seen"
        ]
    );
    assert!(
        connection
            .execute(
                "UPDATE devices SET token_hash = ?2 WHERE id = ?1",
                params![device.id, b"too short"],
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn token_lookup_checks_all_devices() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    let mut issued = Vec::new();
    for i in 0..32 {
        issued.push(store.issue_device(&format!("phone-{i}"))?);
    }
    for (device, token) in &issued {
        assert_eq!(store.device_by_token(token)?, Some(device.clone()));
    }
    assert_eq!(store.device_by_token("not any token")?, None);
    Ok(())
}

#[test]
fn missing_device_mutations_report_errors() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    assert!(store.set_phone_key("missing", "nodekey:x").is_err());
    assert!(store.touch_device("missing").is_err());
    assert_eq!(store.revoke_device("missing")?, None);
    assert!(store.list_devices()?.is_empty());
    Ok(())
}

#[test]
fn session_upsert_updates_every_field_without_deleting_history() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    let mut session = record("s");
    store.upsert_session(&session)?;
    assert_eq!(store.list_sessions()?, vec![session.clone()]);
    store.append_event("s", &event(1))?;
    session.info.agent = "codex".into();
    session.info.title = "改名 ' ;".into();
    session.info.cwd = "/tmp/工作目录".into();
    session.info.state = SessionState::NeedsYou;
    session.info.status_line = Some("等待审批".into());
    session.info.created_at = 20;
    session.info.updated_at = 30;
    session.info.last_seq = 4;
    session.native_id = Some("native-42".into());
    session.info.thread = session.native_id.clone();
    store.upsert_session(&session)?;
    assert_eq!(store.list_sessions()?, vec![session.clone()]);
    assert_eq!(store.events_after("s", 0, 10)?, vec![event(1)]);
    session.info.status_line = None;
    session.native_id = None;
    session.info.thread = None;
    store.upsert_session(&session)?;
    assert_eq!(store.list_sessions()?, vec![session]);
    Ok(())
}

#[test]
fn session_states_and_optional_fields_round_trip() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    let states = [
        SessionState::Idle,
        SessionState::Working,
        SessionState::NeedsYou,
        SessionState::Error,
        SessionState::Exited,
    ];
    for (i, state) in states.into_iter().enumerate() {
        let mut session = record(&format!("s-{i}"));
        session.info.state = state;
        session.info.updated_at = i as u64;
        session.info.status_line = Some("".into());
        session.native_id = Some(format!("resume-{i}"));
        session.info.thread = session.native_id.clone();
        store.upsert_session(&session)?;
        assert_eq!(store.list_sessions()?[0], session);
    }
    assert_eq!(store.list_sessions()?.len(), 5);
    Ok(())
}

#[test]
fn delete_session_cascades_only_its_events() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    for id in ["delete", "keep"] {
        store.upsert_session(&record(id))?;
        store.append_event(id, &event(1))?;
        store.append_event(id, &event(2))?;
    }
    store.delete_session("delete")?;
    store.delete_session("missing")?;
    assert!(store.events_after("delete", 0, 100)?.is_empty());
    assert_eq!(
        store.events_after("keep", 0, 100)?,
        vec![event(1), event(2)]
    );
    assert_eq!(store.list_sessions()?[0].info.id, "keep");
    let count: i64 = store
        .lock()?
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
    assert_eq!(count, 2);
    store.upsert_session(&record("delete"))?;
    store.append_event("delete", &event(1))?;
    assert_eq!(store.events_after("delete", 0, 100)?, vec![event(1)]);
    Ok(())
}

#[test]
fn events_are_paginated_in_strict_sequence_order() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    store.upsert_session(&record("s"))?;
    for seq in [1, 3, 4, 10] {
        store.append_event("s", &event(seq))?;
    }
    assert_eq!(store.events_after("s", 0, 2)?, vec![event(1), event(3)]);
    assert_eq!(store.events_after("s", 3, 2)?, vec![event(4), event(10)]);
    assert_eq!(store.events_after("s", 4, 1)?, vec![event(10)]);
    assert_eq!(store.events_after("s", 0, usize::MAX)?.len(), 4);
    assert!(store.events_after("s", 10, 2)?.is_empty());
    assert!(store.events_after("s", u64::MAX, 2)?.is_empty());
    assert!(store.events_after("s", 0, 0)?.is_empty());
    assert!(store.events_after("missing", 0, 10)?.is_empty());
    Ok(())
}

#[test]
fn transient_rejection_has_no_side_effects() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    let session = record("s");
    store.upsert_session(&session)?;
    let delta = Event {
        seq: 2,
        at: 100,
        kind: EventKind::AssistantDelta {
            item: "a".into(),
            text: "live only".into(),
        },
    };
    assert!(store.append_event("s", &delta).is_err());
    assert!(store.events_after("s", 0, 10)?.is_empty());
    assert_eq!(store.list_sessions()?, vec![session]);
    store.append_event("s", &event(2))?;
    assert!(store.append_event("s", &delta).is_err());
    assert_eq!(store.events_after("s", 0, 10)?, vec![event(2)]);
    Ok(())
}

#[test]
fn duplicate_decreasing_and_zero_sequences_are_rejected() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    store.upsert_session(&record("s"))?;
    assert!(store.append_event("s", &event(0)).is_err());
    store.append_event("s", &event(2))?;
    assert!(store.append_event("s", &event(2)).is_err());
    assert!(store.append_event("s", &event(1)).is_err());
    store.append_event("s", &event(5))?;
    assert_eq!(store.events_after("s", 0, 10)?, vec![event(2), event(5)]);
    assert_eq!(store.list_sessions()?[0].info.last_seq, 5);
    Ok(())
}

#[test]
fn missing_session_cannot_acquire_orphan_events() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    assert!(store.append_event("missing", &event(1)).is_err());
    assert!(store.events_after("missing", 0, 100)?.is_empty());
    assert!(
        store
            .lock()?
            .execute(
                "INSERT INTO events (session, seq, at, json) VALUES ('missing', 1, 1, '{}')",
                [],
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn preupdated_session_and_stale_snapshots_do_not_regress_cursor() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    let mut session = record("s");
    session.info.last_seq = 5;
    session.info.updated_at = 200;
    store.upsert_session(&session)?;
    store.append_event("s", &event(5))?;
    assert_eq!(store.list_sessions()?, vec![session]);
    store.upsert_session(&record("s"))?;
    assert_eq!(store.list_sessions()?[0].info.last_seq, 5);
    assert!(store.append_event("s", &event(4)).is_err());
    Ok(())
}

#[test]
fn every_persistent_event_kind_round_trips_as_flat_json() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    store.upsert_session(&record("s"))?;
    let kinds = vec![
        EventKind::UserText {
            text: "你好\n\0".into(),
            images: None,
        },
        EventKind::AssistantText {
            item: "a".into(),
            text: "complete".into(),
        },
        EventKind::Thinking {
            item: "t".into(),
            text: "think".into(),
        },
        EventKind::ToolStart {
            item: "cmd".into(),
            tool: "shell".into(),
            summary: "pwd".into(),
        },
        EventKind::ToolEnd {
            item: "cmd".into(),
            ok: false,
            output: "结果".into(),
        },
        EventKind::Approval {
            approval: Approval {
                id: "approval".into(),
                kind: ApprovalKind::Command,
                title: "批准？".into(),
                detail: "command".into(),
                cwd: Some("/tmp".into()),
                choices: vec![
                    Decision::AllowOnce,
                    Decision::AllowSession,
                    Decision::Deny,
                    Decision::Abort,
                ],
            },
        },
        EventKind::ApprovalDone {
            approval: "approval".into(),
            decision: Decision::Deny,
        },
        EventKind::State {
            state: SessionState::Working,
            status_line: Some("running".into()),
        },
        EventKind::State {
            state: SessionState::Idle,
            status_line: None,
        },
        EventKind::TurnDone { summary: None },
        EventKind::TurnDone {
            summary: Some("完成".into()),
        },
        EventKind::Notice {
            level: NoticeLevel::Warn,
            text: "注意".into(),
        },
    ];
    let expected: Vec<Event> = kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| Event {
            seq: i as u64 + 1,
            at: 1000 + i as u64,
            kind,
        })
        .collect();
    for e in &expected {
        store.append_event("s", e)?;
    }
    assert_eq!(store.events_after("s", 0, 100)?, expected);
    let json: String = store.lock()?.query_row(
        "SELECT json FROM events WHERE session = 's' AND seq = 1",
        [],
        |row| row.get(0),
    )?;
    let value: serde_json::Value = serde_json::from_str(&json)?;
    assert_eq!(value["e"], "user_text");
    assert_eq!(value["seq"], 1);
    assert!(value.get("kind").is_none());
    Ok(())
}

#[test]
fn integer_overflows_fail_without_wrapping_or_partial_writes() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    store.upsert_session(&record("s"))?;
    let mut invalid = event(1);
    invalid.seq = u64::MAX;
    assert!(store.append_event("s", &invalid).is_err());
    invalid.seq = 1;
    invalid.at = u64::MAX;
    assert!(store.append_event("s", &invalid).is_err());
    assert!(store.events_after("s", 0, 10)?.is_empty());
    assert_eq!(store.list_sessions()?, vec![record("s")]);
    let mut invalid = record("s");
    invalid.info.title = "must not be saved".into();
    invalid.info.last_seq = u64::MAX;
    assert!(store.upsert_session(&invalid).is_err());
    assert_eq!(store.list_sessions()?, vec![record("s")]);
    let max = Event {
        seq: i64::MAX as u64,
        at: i64::MAX as u64,
        kind: EventKind::TurnDone { summary: None },
    };
    store.append_event("s", &max)?;
    assert_eq!(store.events_after("s", i64::MAX as u64 - 1, 1)?, vec![max]);
    assert!(store.events_after("s", i64::MAX as u64, 1)?.is_empty());
    Ok(())
}

#[test]
fn event_insert_and_session_cursor_commit_atomically() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    store.upsert_session(&record("s"))?;
    store.lock()?.execute_batch(
        "CREATE TRIGGER fail_session_update BEFORE UPDATE ON sessions
         BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
    )?;
    assert!(store.append_event("s", &event(1)).is_err());
    assert!(store.events_after("s", 0, 10)?.is_empty());
    assert_eq!(store.list_sessions()?, vec![record("s")]);
    store
        .lock()?
        .execute_batch("DROP TRIGGER fail_session_update")?;
    store.append_event("s", &event(1))?;
    assert_eq!(store.events_after("s", 0, 10)?, vec![event(1)]);
    Ok(())
}

#[test]
fn corrupt_stored_event_returns_error() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    store.upsert_session(&record("s"))?;
    store.append_event("s", &event(1))?;
    store
        .lock()?
        .execute("UPDATE events SET json = 'not json'", [])?;
    assert!(store.events_after("s", 0, 10).is_err());
    Ok(())
}

#[test]
fn file_store_reopens_with_data_wal_foreign_keys_and_version() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("持久化.sqlite3");
    let (device, token, session) = {
        let store = Store::open(&path)?;
        let (device, token) = store.issue_device("手机")?;
        store.set_phone_key(&device.id, "nodekey:persistent")?;
        store.touch_device(&device.id)?;
        store.upsert_session(&record("s"))?;
        store.append_event("s", &event(1))?;
        let device = store.device_by_token(&token)?.unwrap();
        (device, token, store.list_sessions()?.remove(0))
    };
    let store = Store::open(&path)?;
    assert_eq!(store.device_by_token(&token)?, Some(device.clone()));
    assert_eq!(store.list_sessions()?, vec![session]);
    assert_eq!(store.events_after("s", 0, 100)?, vec![event(1)]);
    assert!(store.append_event("s", &event(1)).is_err());
    {
        let connection = store.lock()?;
        assert_eq!(schema_version(&connection), SCHEMA_VERSION);
        assert_eq!(
            connection.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?,
            "wal"
        );
        assert_eq!(
            connection.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))?,
            1
        );
    }
    store.delete_session("s")?;
    assert_eq!(store.revoke_device(&device.id)?, Some(device));
    drop(store);
    let bytes = std::fs::read(&path)?;
    assert!(
        !bytes
            .windows(token.len())
            .any(|window| window == token.as_bytes())
    );
    let store = Store::open(&path)?;
    assert_eq!(store.device_by_token(&token)?, None);
    assert!(store.events_after("s", 0, 10)?.is_empty());
    Ok(())
}

#[test]
fn migrate_version_zero_database() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("migrate.sqlite3");
    let connection = Connection::open(&path)?;
    assert_eq!(schema_version(&connection), 0);
    drop(connection);
    let store = Store::open(&path)?;
    assert_eq!(schema_version(&*store.lock()?), SCHEMA_VERSION);
    assert!(store.list_devices()?.is_empty());
    assert!(store.list_sessions()?.is_empty());
    Ok(())
}

#[test]
fn incompatible_versions_are_not_downgraded() -> anyhow::Result<()> {
    for version in [-1, SCHEMA_VERSION + 1] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("future.sqlite3");
        let connection = Connection::open(&path)?;
        connection.pragma_update(None, "user_version", version)?;
        drop(connection);
        assert!(Store::open(&path).is_err());
        let connection = Connection::open(&path)?;
        assert_eq!(schema_version(&connection), version);
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
    }
    Ok(())
}

#[test]
fn failed_migration_rolls_back_schema_and_version() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("conflict.sqlite3");
    let connection = Connection::open(&path)?;
    connection.execute_batch("CREATE TABLE sessions (sentinel TEXT)")?;
    drop(connection);
    assert!(Store::open(&path).is_err());
    let connection = Connection::open(&path)?;
    assert_eq!(schema_version(&connection), 0);
    let names = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(names, vec!["sessions"]);
    Ok(())
}

#[test]
fn concurrent_writers_persist_1000_events() -> anyhow::Result<()> {
    let store = Arc::new(Store::open_in_memory()?);
    let barrier = Arc::new(Barrier::new(10));
    let workers: Vec<_> = (0..10)
        .map(|worker| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> anyhow::Result<()> {
                let id = format!("session-{worker}");
                store.upsert_session(&record(&id))?;
                barrier.wait();
                for seq in 1..=100 {
                    store.append_event(&id, &event(seq))?;
                }
                Ok(())
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap()?;
    }
    let sessions = store.list_sessions()?;
    assert_eq!(sessions.len(), 10);
    let mut total = 0;
    for session in sessions {
        let events = store.events_after(&session.info.id, 0, usize::MAX)?;
        assert_eq!(events, (1..=100).map(event).collect::<Vec<_>>());
        assert_eq!(session.info.last_seq, 100);
        total += events.len();
    }
    assert_eq!(total, 1000);
    Ok(())
}

#[test]
fn concurrent_duplicate_sequence_has_exactly_one_winner() -> anyhow::Result<()> {
    let store = Arc::new(Store::open_in_memory()?);
    store.upsert_session(&record("s"))?;
    let barrier = Arc::new(Barrier::new(10));
    let workers: Vec<_> = (0..10)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store.append_event("s", &event(1)).is_ok()
            })
        })
        .collect();
    let successes = workers
        .into_iter()
        .map(|worker| usize::from(worker.join().unwrap()))
        .sum::<usize>();
    assert_eq!(successes, 1);
    assert_eq!(store.events_after("s", 0, 10)?, vec![event(1)]);
    Ok(())
}

#[test]
fn independent_connections_serialize_sequence_checks() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("shared.sqlite3");
    let first = Store::open(&path)?;
    first.upsert_session(&record("s"))?;
    let second = Store::open(&path)?;
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [first, second]
        .into_iter()
        .map(|store| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store.append_event("s", &event(1)).is_ok()
            })
        })
        .collect();
    let successes = workers
        .into_iter()
        .map(|worker| usize::from(worker.join().unwrap()))
        .sum::<usize>();
    assert_eq!(successes, 1);
    assert_eq!(
        Store::open(&path)?.events_after("s", 0, 10)?,
        vec![event(1)]
    );
    Ok(())
}

#[test]
fn schema_v1_database_gains_model_columns_and_keeps_sessions() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("v1.db");
    {
        let connection = Connection::open(&path)?;
        connection.execute_batch(SCHEMA_V1)?;
        connection.pragma_update(None, "user_version", 1)?;
        connection.execute(
            "INSERT INTO sessions (id, agent, title, cwd, state, created_at, updated_at, last_seq, native_id)
             VALUES ('old', 'codex', 't', '/w', 'idle', 1, 2, 3, 'thread-1')",
            [],
        )?;
    }
    let store = Store::open(&path)?;
    let sessions = store.list_sessions()?;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].native_id.as_deref(), Some("thread-1"));
    assert_eq!(
        (sessions[0].info.model.as_deref(), sessions[0].info.last_seq),
        (None, 3)
    );

    let mut session = sessions[0].clone();
    session.info.model = Some("gpt-6.1-sol".into());
    session.info.effort = Some("high".into());
    session.info.speed = Some("priority".into());
    session.info.permission = Some(":danger-full-access".into());
    store.upsert_session(&session)?;
    drop(store);
    let store = Store::open(&path)?;
    let again = &store.list_sessions()?[0];
    assert_eq!(
        (again.info.model.as_deref(), again.info.effort.as_deref()),
        (Some("gpt-6.1-sol"), Some("high"))
    );
    assert_eq!(again.info.speed.as_deref(), Some("priority"));
    assert_eq!(
        again.info.permission.as_deref(),
        Some(":danger-full-access")
    );
    assert_eq!(schema_version(&*store.lock()?), SCHEMA_VERSION);
    Ok(())
}

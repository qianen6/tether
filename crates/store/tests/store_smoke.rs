use tether_proto::{Event, EventKind, SessionInfo, SessionState};
use tether_store::{SessionRecord, Store};

#[test]
fn store_api_smoke() -> anyhow::Result<()> {
    let store = Store::open_in_memory()?;
    let (device, token) = store.issue_device("测试手机")?;
    assert_eq!(store.device_by_token(&token)?, Some(device.clone()));
    store.set_phone_key(&device.id, "nodekey:smoke")?;
    store.touch_device(&device.id)?;
    assert_eq!(
        store
            .revoke_device(&device.id)?
            .unwrap()
            .phone_key
            .as_deref(),
        Some("nodekey:smoke")
    );
    assert_eq!(store.device_by_token(&token)?, None);

    store.upsert_session(&SessionRecord {
        info: SessionInfo {
            id: "smoke".into(),
            agent: "echo".into(),
            title: "冒烟测试".into(),
            cwd: ".".into(),
            state: SessionState::Idle,
            status_line: None,
            created_at: 1,
            updated_at: 1,
            last_seq: 0,
            model: None,
            effort: None,
            chat: false,
            speed: None,
            thread: None,
            permission: None,
        },
        native_id: Some("native-smoke".into()),
    })?;
    let event = Event {
        seq: 1,
        at: 2,
        kind: EventKind::UserText {
            text: "hello".into(),
            images: None,
        },
    };
    store.append_event("smoke", &event)?;
    assert_eq!(store.events_after("smoke", 0, 10)?, vec![event.clone()]);
    assert!(store.append_event("smoke", &event).is_err());
    assert!(
        store
            .append_event(
                "smoke",
                &Event {
                    seq: 2,
                    at: 3,
                    kind: EventKind::AssistantDelta {
                        item: "a".into(),
                        text: "transient".into()
                    },
                }
            )
            .is_err()
    );
    store.delete_session("smoke")?;
    assert!(store.list_sessions()?.is_empty());
    assert!(store.events_after("smoke", 0, 10)?.is_empty());
    Ok(())
}

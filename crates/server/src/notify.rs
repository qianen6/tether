use crate::{State, lock};
use std::{sync::Arc, time::Duration};
use tether_proto::{EventKind, NoticeLevel, NotifyKind, ServerMsg, SessionInfo, SessionState};
use tether_terminals::TermChunk;
use tokio::{sync::broadcast, time::Instant};

pub(crate) fn session(
    info: &SessionInfo,
    previous: SessionState,
    event: &EventKind,
    subscribers: usize,
) -> Option<ServerMsg> {
    let kind = match event {
        EventKind::State {
            state: SessionState::NeedsYou,
            ..
        } if previous != SessionState::NeedsYou => NotifyKind::NeedsYou,
        EventKind::State {
            state: SessionState::Error,
            ..
        } if previous != SessionState::Error => NotifyKind::Failed,
        EventKind::Notice {
            level: NoticeLevel::Error,
            ..
        } => NotifyKind::Failed,
        EventKind::TurnDone { .. } if subscribers == 0 => NotifyKind::Done,
        _ => return None,
    };
    let body = match event {
        EventKind::Notice { text, .. } => text.clone(),
        EventKind::TurnDone { summary } => summary.clone().unwrap_or_default(),
        _ => info.status_line.clone().unwrap_or_default(),
    };
    Some(ServerMsg::Notify {
        kind,
        session: Some(info.id.clone()),
        term: None,
        title: info.title.clone(),
        body,
    })
}

/// One observer per terminal, not per attached connection. Attention therefore
/// continues while detached, with one shared cooldown for all connections.
pub(crate) fn monitor(state: &Arc<State>, id: &str) -> anyhow::Result<()> {
    let mut monitors = lock(&state.monitors);
    if monitors.contains_key(id) {
        return Ok(());
    }
    let (_, mut receiver) = state.terms.attach(id)?;
    let term = id.to_owned();
    let bus = state.bus.clone();
    let mut stopping = state.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut last: Option<Instant> = None;
        loop {
            tokio::select! {
                _ = stopping.changed() => break,
                chunk = receiver.recv() => match chunk {
                    Ok(TermChunk::Attention) if last.is_none_or(|at| at.elapsed() >= Duration::from_secs(60)) => {
                        last = Some(Instant::now());
                        let _ = bus.send(ServerMsg::Notify {
                            kind: NotifyKind::NeedsYou, session: None, term: Some(term.clone()),
                            title: "终端需要你".into(), body: "终端可能正在等待输入".into(),
                        });
                    }
                    Ok(TermChunk::Exit(_)) | Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) | Ok(_) => {},
                }
            }
        }
    });
    monitors.insert(id.to_owned(), task);
    Ok(())
}

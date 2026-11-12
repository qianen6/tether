//! Tether wire protocol v1. `docs/PROTOCOL.md` is the specification; this crate
//! is its Rust form. Every type here is exported to TypeScript by ts-rs
//! (`cargo test -p tether-proto export_bindings`) into
//! `packages/client/src/generated/`.
//!
//! Text WebSocket frames carry one JSON object tagged by `t`; binary frames
//! carry terminal bytes (see [`binary`]).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const PROTOCOL_VERSION: u32 = 1;
/// Largest frame either side accepts.
pub const MAX_FRAME: usize = 8 << 20;
/// `tool_end.output` is cut to this many bytes, keeping the end.
pub const MAX_TOOL_OUTPUT: usize = 8 << 10;

/// WebSocket close codes.
pub mod close {
    /// No `pair`/`hello` within 5 seconds, or a malformed first frame.
    pub const HANDSHAKE: u16 = 4400;
    /// Bad secret/token, or the device was revoked.
    pub const UNAUTHORIZED: u16 = 4401;
    pub const VERSION_MISMATCH: u16 = 4426;
}

// ---------------------------------------------------------------- phone → computer

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "t", rename_all = "snake_case")]
#[ts(export, optional_fields = nullable)]
pub enum ClientMsg {
    /// First frame of the very first connection: spend the one-time pairing secret.
    Pair {
        version: u32,
        secret: String,
        device_name: String,
    },
    /// First frame of every later connection.
    Hello {
        version: u32,
        token: String,
        device_name: String,
    },

    ListAgents {
        req: u64,
    },
    ListSessions {
        req: u64,
    },
    CreateSession {
        req: u64,
        agent: String,
        cwd: Option<String>,
        title: Option<String>,
        /// Continue this conversation of the agent's own (`HistoryThread.id`),
        /// from its latest record on the computer.
        resume: Option<String>,
        /// A `ModelInfo.id`, or any name the agent's provider accepts.
        model: Option<String>,
        /// One of the model's `efforts`.
        effort: Option<String>,
        /// A conversation without a project (the Codex app's "Chats"): the
        /// agent gets a fresh folder of its own, `cwd` is ignored, and it may
        /// only read, asking before anything else.
        chat: Option<bool>,
        /// The agent's own project (`ProjectInfo.id`) to file it under; `cwd`
        /// is then one of the project's roots.
        project: Option<String>,
        /// All of the project's folders (`ProjectInfo.roots`), so the agent
        /// may work in each.
        roots: Option<Vec<String>>,
        /// A `SpeedInfo.id` of the model, or "default" for standard speed.
        speed: Option<String>,
        /// A `PermissionInfo.id` from `list_models`: what the agent may do
        /// without asking. `null`: as before (a chat reads only, other work
        /// may change its folder), for agents that list none.
        permission: Option<String>,
        /// Show the conversation in the agent's desktop app once it has its
        /// first message (`reveal`).
        reveal: Option<bool>,
        /// With `resume`: continue in a new copy of that conversation instead
        /// (the Codex app's "fork"), for one another program has open.
        fork: Option<bool>,
    },
    /// Replay events with `seq > after`, then stream live ones.
    Subscribe {
        req: u64,
        session: String,
        after: u64,
    },
    Unsubscribe {
        req: u64,
        session: String,
    },
    SendMessage {
        req: u64,
        session: String,
        text: String,
        /// Pictures on the computer to send with it (`put_file` paths).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<String>>,
    },
    Answer {
        req: u64,
        session: String,
        approval: String,
        decision: Decision,
    },
    Interrupt {
        req: u64,
        session: String,
    },
    CloseSession {
        req: u64,
        session: String,
    },

    /// Either `command` or `agent` (its `terminal_command`) must be given.
    TermOpen {
        req: u64,
        command: Option<Vec<String>>,
        agent: Option<String>,
        cwd: Option<String>,
        cols: u16,
        rows: u16,
    },
    TermAttach {
        req: u64,
        term: String,
    },
    TermDetach {
        req: u64,
        term: String,
    },
    TermResize {
        req: u64,
        term: String,
        cols: u16,
        rows: u16,
    },
    TermKill {
        req: u64,
        term: String,
    },
    ListTerms {
        req: u64,
    },

    /// The agent's own saved conversations (Codex threads), newest first.
    ListHistory {
        req: u64,
        agent: String,
        search: Option<String>,
        /// `next_cursor` of the previous page.
        cursor: Option<String>,
    },
    /// One saved conversation, as it is on the computer now.
    ReadHistory {
        req: u64,
        agent: String,
        thread: String,
    },
    /// The models the agent offers on this computer.
    ListModels {
        req: u64,
        agent: String,
    },
    /// From the next turn on. `null` leaves that one as it is.
    SetModel {
        req: u64,
        session: String,
        model: Option<String>,
        effort: Option<String>,
        speed: Option<String>,
        /// A `PermissionInfo.id`.
        permission: Option<String>,
    },
    /// Open one of the agent's conversations in its desktop app on the
    /// computer, so it is listed there too.
    Reveal {
        req: u64,
        agent: String,
        thread: String,
    },

    /// Saves a picture from the phone on the computer, for `send_message`.
    /// `data` is base64; at most `MAX_FILE` bytes once decoded.
    PutFile {
        req: u64,
        name: String,
        data: String,
    },
    /// A picture on the computer a conversation shows (`user_text.images`).
    GetFile {
        req: u64,
        path: String,
    },

    Ping {
        n: u64,
    },
}

/// The largest picture `put_file` takes and `get_file` sends, in bytes: as
/// base64 it still fits a frame (`MAX_FRAME`).
pub const MAX_FILE: usize = 5 << 20;

impl ClientMsg {
    /// The request number, for messages that expect `ok`/`error`.
    pub fn req(&self) -> Option<u64> {
        use ClientMsg::*;
        match self {
            Pair { .. } | Hello { .. } | Ping { .. } => None,
            ListAgents { req }
            | ListSessions { req }
            | CreateSession { req, .. }
            | Subscribe { req, .. }
            | Unsubscribe { req, .. }
            | SendMessage { req, .. }
            | Answer { req, .. }
            | Interrupt { req, .. }
            | CloseSession { req, .. }
            | TermOpen { req, .. }
            | TermAttach { req, .. }
            | TermDetach { req, .. }
            | TermResize { req, .. }
            | TermKill { req, .. }
            | ListTerms { req }
            | ListHistory { req, .. }
            | ReadHistory { req, .. }
            | ListModels { req, .. }
            | SetModel { req, .. }
            | Reveal { req, .. }
            | PutFile { req, .. }
            | GetFile { req, .. } => Some(*req),
        }
    }
}

// ---------------------------------------------------------------- computer → phone

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "t", rename_all = "snake_case")]
#[ts(export, optional_fields = nullable)]
pub enum ServerMsg {
    Paired {
        device_id: String,
        token: String,
    },
    Welcome {
        version: u32,
        host_name: String,
        device_id: String,
        server_time: u64,
    },
    Rejected {
        reason: RejectReason,
    },

    Ok {
        req: u64,
        data: serde_json::Value,
    },
    Error {
        req: Option<u64>,
        code: ErrorCode,
        message: String,
    },

    Event {
        session: String,
        event: Event,
    },
    SessionUpdate {
        session: SessionInfo,
    },
    SessionClosed {
        session: String,
    },
    TermExit {
        term: String,
        code: Option<i32>,
    },
    Notify {
        kind: NotifyKind,
        session: Option<String>,
        term: Option<String>,
        title: String,
        body: String,
    },

    Pong {
        n: u64,
    },
}

impl ServerMsg {
    /// `ok` with a typed payload (one of the `*Data` structs below, or `Empty`).
    pub fn ok<T: Serialize>(req: u64, data: &T) -> ServerMsg {
        ServerMsg::Ok {
            req,
            data: serde_json::to_value(data).unwrap_or(serde_json::Value::Null),
        }
    }
    pub fn error(req: Option<u64>, code: ErrorCode, message: impl Into<String>) -> ServerMsg {
        ServerMsg::Error {
            req,
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RejectReason {
    InvalidSecret,
    InvalidToken,
    Revoked,
    VersionMismatch,
    /// Too many failed pairing attempts; pairing is refused for 60 seconds.
    Locked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    NotFound,
    AgentUnavailable,
    Busy,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NotifyKind {
    NeedsYou,
    Done,
    Failed,
}

// ---------------------------------------------------------------- `ok` payloads

/// `ok` payload for requests that return nothing (`{}`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Empty {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentsData {
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionsData {
    pub sessions: Vec<SessionInfo>,
}

/// `create_session` and `subscribe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionData {
    pub session: SessionInfo,
}

/// `put_file`: where the picture now is on the computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileData {
    pub path: String,
}

/// `get_file`: the picture, base64.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileContent {
    pub name: String,
    /// `image/png` and so on.
    pub mime: String,
    pub data: String,
}

/// `term_open` and `term_attach`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TermData {
    pub term: TermInfo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TermsData {
    pub terms: Vec<TermInfo>,
}

/// `list_history`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, optional_fields = nullable)]
pub struct HistoryData {
    pub threads: Vec<HistoryThread>,
    pub next_cursor: Option<String>,
    /// The agent's projects, on the first page only (no `cursor`).
    #[serde(default)]
    pub projects: Vec<ProjectInfo>,
}

/// A project as the agent's desktop app defines it: a name and one or more
/// folders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    /// The first one is where its conversations start.
    pub roots: Vec<String>,
}

/// A speed the model can run at, e.g. `{id: "priority", name: "Fast"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, optional_fields = nullable)]
pub struct SpeedInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// `read_history`: the conversation's turns as events, `seq` from 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, optional_fields = nullable)]
pub struct HistoryReadData {
    pub thread: HistoryThread,
    pub events: Vec<Event>,
    /// Older turns left out to fit one frame.
    pub truncated: bool,
}

/// `list_models`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelsData {
    pub models: Vec<ModelInfo>,
    /// What the agent may be allowed to do, as this computer's agent offers
    /// it (versions differ); empty when it offers no choice.
    #[serde(default)]
    pub permissions: Vec<PermissionInfo>,
    /// The `PermissionInfo.id` its own app uses now on this computer.
    #[serde(default)]
    pub default_permission: Option<String>,
}

/// One level of what an agent may do without asking (Codex's permission
/// profiles: read only, its workspace, everything).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, optional_fields = nullable)]
pub struct PermissionInfo {
    /// What `create_session` / `set_model` take.
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, optional_fields = nullable)]
pub struct ModelInfo {
    /// What `create_session` / `set_model` take.
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// Reasoning efforts it accepts, e.g. "low", "medium", "high".
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    /// The agent's default on this computer.
    pub is_default: bool,
    /// Faster speeds besides the standard one ("default").
    #[serde(default)]
    pub speeds: Vec<SpeedInfo>,
    /// The speed used when none is chosen; `null` is standard.
    #[serde(default)]
    pub default_speed: Option<String>,
}

/// A conversation the agent keeps itself, wherever it was started
/// (desktop app, command line, Tether).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, optional_fields = nullable)]
pub struct HistoryThread {
    pub id: String,
    pub title: String,
    /// The start of the first message.
    pub preview: String,
    pub cwd: String,
    /// Who started it, e.g. "Codex Desktop", "codex_cli_rs", "tether".
    pub origin: Option<String>,
    /// Unix milliseconds.
    pub created_at: u64,
    pub updated_at: u64,
    /// A conversation without a project (the Codex app's "Chats").
    #[serde(default)]
    pub chat: bool,
    /// The `ProjectInfo.id` it belongs to, when known.
    #[serde(default)]
    pub project: Option<String>,
}

// ---------------------------------------------------------------- shared types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentKind {
    Codex,
    Claude,
    Acp,
    /// DeepSeek Harness, through its `dsh web` gateway.
    Dsh,
    Terminal,
    Echo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, optional_fields = nullable)]
pub struct AgentInfo {
    /// Stable id used in `create_session` / `term_open`.
    pub id: String,
    pub name: String,
    pub kind: AgentKind,
    /// False when the command is not installed; `detail` says why.
    pub available: bool,
    pub detail: Option<String>,
    /// Can run as a chat session. Terminal-only agents (a plain shell) cannot.
    pub chat: bool,
    /// Command line that opens it in a terminal.
    pub terminal_command: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SessionState {
    Idle,
    Working,
    /// Waiting on an approval from the phone.
    NeedsYou,
    Error,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, optional_fields = nullable)]
pub struct SessionInfo {
    pub id: String,
    pub agent: String,
    pub title: String,
    pub cwd: String,
    pub state: SessionState,
    /// One line saying what the agent is doing now.
    pub status_line: Option<String>,
    /// Unix milliseconds.
    pub created_at: u64,
    pub updated_at: u64,
    /// Highest event seq so far.
    pub last_seq: u64,
    /// Chosen in `create_session` / `set_model`; `null` is the agent's default.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Started with `create_session { chat: true }`.
    #[serde(default)]
    pub chat: bool,
    /// Chosen in `create_session` / `set_model`; `null` is the agent's default.
    #[serde(default)]
    pub speed: Option<String>,
    /// The agent's own conversation (`HistoryThread.id`) once it has one.
    #[serde(default)]
    pub thread: Option<String>,
    /// Chosen in `create_session` / `set_model` (`PermissionInfo.id`);
    /// `null`: as the agent was told before there was a choice.
    #[serde(default)]
    pub permission: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TermInfo {
    pub id: String,
    pub command: Vec<String>,
    pub cwd: String,
    pub cols: u16,
    pub rows: u16,
    pub running: bool,
    pub created_at: u64,
}

/// A numbered session event. JSON is flat: `{"seq":1,"at":…,"e":"user_text","text":"…"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Event {
    pub seq: u64,
    /// Unix milliseconds.
    pub at: u64,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "e", rename_all = "snake_case")]
#[ts(export, optional_fields = nullable)]
pub enum EventKind {
    UserText {
        text: String,
        /// Pictures sent with it: paths on the computer (`get_file`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<String>>,
    },
    /// Live-only piece of a reply; never stored. `assistant_text` with the same
    /// `item` replaces all of them.
    AssistantDelta {
        item: String,
        text: String,
    },
    AssistantText {
        item: String,
        text: String,
    },
    Thinking {
        item: String,
        text: String,
    },
    ToolStart {
        item: String,
        tool: String,
        summary: String,
    },
    ToolEnd {
        item: String,
        ok: bool,
        output: String,
    },
    Approval {
        approval: Approval,
    },
    ApprovalDone {
        approval: String,
        decision: Decision,
    },
    State {
        state: SessionState,
        status_line: Option<String>,
    },
    TurnDone {
        summary: Option<String>,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
}

impl EventKind {
    /// Transient events are streamed but not stored or replayed.
    pub fn is_transient(&self) -> bool {
        matches!(self, EventKind::AssistantDelta { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, optional_fields = nullable)]
pub struct Approval {
    pub id: String,
    pub kind: ApprovalKind,
    /// Short line for a notification ("运行：npm test").
    pub title: String,
    /// Exactly what will happen: full command, diff, tool input.
    pub detail: String,
    pub cwd: Option<String>,
    /// The decisions this agent supports for this request.
    pub choices: Vec<Decision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalKind {
    Command,
    FileChange,
    Tool,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Decision {
    AllowOnce,
    /// Allow this and similar requests for the rest of the session.
    AllowSession,
    Deny,
    /// Deny and stop the current turn.
    Abort,
}

// ---------------------------------------------------------------- binary frames

/// Terminal bytes travel in binary WebSocket frames:
/// `[kind: u8][id_len: u8][id: id_len bytes UTF-8][data…]`.
pub mod binary {
    use anyhow::{Context, bail, ensure};

    /// Terminal output, computer → phone.
    pub const TERM_OUTPUT: u8 = 0x01;
    /// Keystrokes, phone → computer.
    pub const TERM_INPUT: u8 = 0x02;

    /// Panics if `term` is empty or longer than 255 bytes (ids are uuids).
    pub fn encode(kind: u8, term: &str, data: &[u8]) -> Vec<u8> {
        let id = term.as_bytes();
        assert!(
            !id.is_empty() && id.len() <= 255,
            "terminal id must be 1-255 bytes"
        );
        let mut out = Vec::with_capacity(2 + id.len() + data.len());
        out.push(kind);
        out.push(id.len() as u8);
        out.extend_from_slice(id);
        out.extend_from_slice(data);
        out
    }

    pub fn decode(frame: &[u8]) -> anyhow::Result<(u8, &str, &[u8])> {
        ensure!(frame.len() >= 2, "binary frame too short");
        let kind = frame[0];
        if kind != TERM_OUTPUT && kind != TERM_INPUT {
            bail!("unknown binary frame kind {kind:#04x}");
        }
        let n = frame[1] as usize;
        ensure!(n > 0, "empty terminal id");
        ensure!(
            frame.len() >= 2 + n,
            "binary frame shorter than its terminal id"
        );
        let id = std::str::from_utf8(&frame[2..2 + n]).context("terminal id is not UTF-8")?;
        Ok((kind, id, &frame[2 + n..]))
    }
}

/// Cut `s` to at most `max` bytes, keeping the end and respecting UTF-8.
pub fn truncate_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: Uuid,
    pub name: String,
    pub cwd: String,
    pub shell: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub status: SessionStatus,
    pub pid: Option<u32>,
    pub exit_code: Option<u32>,
    #[serde(default)]
    pub archived: bool,
    pub attached_clients: u32,
    pub last_activity_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Exited,
    Starting,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionRequest {
    pub name: String,
    pub cwd: String,
    pub shell: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchSessionRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Hello {
        cols: u16,
        rows: u16,
        resume_from_seq: Option<u64>,
        capabilities: ClientCapabilities,
    },
    Input {
        data_b64: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Keyboard {
        action: KeyboardAction,
    },
    Ping {
        ts_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientCapabilities {
    pub mobile: bool,
    pub keyboard_overlay: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardAction {
    ToggleCtrlLock,
    ToggleAltLock,
    ToggleShiftLock,
    SendEsc,
    SendTab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Function(u8),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    HelloAck {
        protocol_version: u16,
        session_id: Uuid,
        next_seq: u64,
    },
    Snapshot {
        from_seq: u64,
        chunks: Vec<OutputChunk>,
    },
    Output {
        seq: u64,
        data_b64: String,
    },
    Status {
        running: bool,
        attached_clients: u32,
    },
    SessionUpdated {
        session: SessionSummary,
    },
    Pong {
        ts_ms: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputChunk {
    pub seq: u64,
    pub data_b64: String,
}

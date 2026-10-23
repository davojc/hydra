//! Messages exchanged over the link. Enums are tagged with `type` and use snake_case,
//! because the web UI reads this JSON.

use serde::{Deserialize, Serialize};

/// The ALPN the link speaks over iroh.
pub const ALPN: &[u8] = b"hydra/link/1";
/// The protocol version a peer announces in `Hello` / `Welcome`.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Backlog,
    Pickup,
    InProgress,
    Verify,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireCard {
    pub text: String,
    pub taker: Option<String>,
    pub commit: Option<String>,
}

/// Names a card by its column, text and which of equal texts it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardRef {
    pub column: Column,
    pub text: String,
    pub nth: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardInfo {
    pub project: String,
    pub name: String,
    /// Five entries, in `Column` order.
    pub columns: Vec<Vec<WireCard>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Working,
    NeedsYou,
    Finished,
    Idle,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Claude,
    Shell,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u64,
    pub env: String,
    /// "#rrggbb"
    pub env_color: String,
    pub label: String,
    pub cwd: String,
    pub project: Option<String>,
    pub kind: Kind,
    pub status: Status,
    pub card: Option<String>,
    pub remote_control: bool,
    /// Unix seconds.
    pub changed_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    NeedsYou,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        version: u32,
    },
    Pair {
        secret: String,
    },
    Snapshot,
    Board {
        project: String,
    },
    AddCard {
        project: String,
        text: String,
        to: Column,
    },
    MoveCard {
        project: String,
        card: CardRef,
        to: Column,
    },
    RemoteControl {
        session: u64,
    },
    Seen {
        session: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub msg: ClientMsg,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Snapshot {
        sessions: Vec<SessionInfo>,
        projects: Vec<String>,
    },
    Board(BoardInfo),
    Refused {
        reason: String,
    },
    Missing,
    Paired {
        word: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome { version: u32, window: String },
    Reply { id: u64, outcome: Outcome },
    Session(SessionInfo),
    SessionGone { id: u64 },
    Board(BoardInfo),
    Alert { session: u64, kind: AlertKind },
    Notice { text: String },
    TooOld { need: u32 },
}

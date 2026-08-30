//! Versioned DTOs exchanged over the local runtime socket.

use serde::{Deserialize, Serialize};

use super::PROTOCOL_VERSION;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RequestEnvelope {
    pub(crate) protocol_version: u32,
    pub(crate) request: Request,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Request {
    RuntimeInfo,
    ListSessions,
    OpenSession {
        session: SessionReference,
    },
    ExecuteTurn {
        session: SessionReference,
        prompt: String,
    },
    ExecuteCommand {
        session: SessionReference,
        text: String,
        sender_id: Option<String>,
    },
    StageFollowup {
        session: SessionReference,
        input: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResponseEnvelope {
    pub(crate) protocol_version: u32,
    pub(crate) response: Response,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Response {
    RuntimeInfo {
        egopulse_version: String,
    },
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    Session {
        session: SessionView,
    },
    TurnEvent {
        event: TurnEvent,
    },
    TurnFinished,
    CommandFinished {
        outcome: CommandOutcome,
        effective_provider: String,
        effective_model: String,
    },
    FollowupFinished {
        outcome: FollowupOutcome,
    },
    ProtocolMismatch {
        expected: u32,
        actual: u32,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SessionReference {
    Existing { chat_id: i64 },
    Named { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionSummary {
    pub(crate) chat_id: i64,
    pub(crate) channel: String,
    pub(crate) surface_thread: String,
    pub(crate) chat_title: Option<String>,
    pub(crate) last_message_time: String,
    pub(crate) last_message_preview: Option<String>,
    pub(crate) agent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionView {
    pub(crate) reference: SessionReference,
    pub(crate) channel: String,
    pub(crate) surface_thread: String,
    pub(crate) chat_type: String,
    pub(crate) agent_id: String,
    pub(crate) effective_provider: String,
    pub(crate) effective_model: String,
    pub(crate) transcript: Vec<TranscriptEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TranscriptEntry {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    System {
        text: String,
    },
    ToolStarted {
        call_id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolFinished {
        call_id: String,
        name: String,
        is_error: bool,
        preview: String,
        duration_ms: Option<u128>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TurnEvent {
    Iteration {
        iteration: usize,
    },
    Delta {
        text: String,
    },
    ToolStart {
        name: String,
        input: serde_json::Value,
        call_id: String,
    },
    ToolResult {
        name: String,
        is_error: bool,
        preview: String,
        duration_ms: u128,
        call_id: String,
    },
    UserInputInjected {
        message_id: String,
        sender_id: String,
        text: String,
        timestamp: String,
    },
    FinalResponse {
        text: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CommandOutcome {
    Respond { text: String },
    Error { message: String },
    NotHandled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FollowupOutcome {
    Accepted,
    NoToolPhase,
}

impl RequestEnvelope {
    pub(crate) fn new(request: Request) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request,
        }
    }
}

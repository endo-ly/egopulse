//! SSE イベント表現を定義するモジュール。
//!
//! agent loop の内部イベントを定義する。

use serde::Serialize;

/// Represents internal events emitted while the agent processes a turn.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AgentEvent {
    /// Iteration counter.
    Iteration { iteration: usize },
    /// Tool execution started.
    ToolStart {
        name: String,
        input: serde_json::Value,
    },
    /// Tool execution completed.
    ToolResult {
        name: String,
        is_error: bool,
        preview: String,
        duration_ms: u128,
    },
    /// Final response.
    FinalResponse { text: String },
    /// Error occurred.
    Error { message: String },
}

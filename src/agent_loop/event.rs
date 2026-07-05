//! エージェントループの内部ライフサイクルイベントを定義するモジュール。
//!
//! チャネル層（Web SSE / Discord / Telegram）はこれらのイベントを購読して、
//! それぞれの表示形式へ変換する。イベントの正統な居住場所は agent loop であり、
//! 各チャネルは受動的な消費者にとどまる。

use serde::Serialize;

/// Represents internal events emitted while the agent processes a turn.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AgentEvent {
    /// Iteration counter.
    Iteration { iteration: usize },
    /// Incremental text chunk from LLM streaming.
    Delta { text: String },
    /// Tool execution started.
    ToolStart {
        name: String,
        input: serde_json::Value,
        /// LLM-issued tool call id. Disambiguates concurrent same-name tools.
        call_id: String,
    },
    /// Tool execution completed.
    ToolResult {
        name: String,
        is_error: bool,
        preview: String,
        duration_ms: u128,
        /// LLM-issued tool call id. Disambiguates concurrent same-name tools.
        call_id: String,
    },
    /// Final response.
    FinalResponse { text: String },
    /// Error occurred.
    Error { message: String },
}

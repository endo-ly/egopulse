//! エージェントループの内部ライフサイクルイベントを定義するモジュール。
//!
//! チャネル層（Web SSE / Discord / Telegram）はこれらのイベントを購読して、
//! それぞれの表示形式へ変換する。イベントの正統な居住場所は agent loop であり、
//! 各チャネルは受動的な消費者にとどまる。

use serde::Serialize;
use std::sync::Arc;

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
    /// A human message accepted during the Tool phase and committed after its
    /// Tool Results. The event is emitted only after the database commit.
    UserInputInjected {
        message_id: String,
        sender_id: String,
        text: String,
        timestamp: String,
    },
    /// Final response.
    FinalResponse { text: String },
    /// Error occurred.
    Error { message: String },
}

/// Type-erased callback for agent lifecycle events.
#[derive(Clone)]
pub(crate) struct EventEmitter(Option<Arc<dyn Fn(AgentEvent) + Send + Sync>>);

impl EventEmitter {
    /// Creates a no-op emitter that discards all events.
    pub(crate) fn none() -> Self {
        Self(None)
    }

    /// Creates an emitter from a concrete callback.
    pub(crate) fn new<F>(f: F) -> Self
    where
        F: Fn(AgentEvent) + Send + Sync + 'static,
    {
        Self(Some(Arc::new(f)))
    }

    /// Emits a single event if a callback is registered.
    pub(crate) fn emit(&self, event: AgentEvent) {
        if let Some(f) = &self.0 {
            f(event);
        }
    }
}

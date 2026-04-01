//! SSE (Server-Sent Events) utilities.
//!
//! Based on Microclaw's implementation.

use serde::Serialize;

/// Events emitted during agent processing.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
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
    /// Text delta from LLM (streaming).
    TextDelta { delta: String },
    /// Final response.
    FinalResponse { text: String },
    /// Error occurred.
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_event_serialization() {
        let event = AgentEvent::TextDelta {
            delta: "Hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("text_delta"));
        assert!(json.contains("Hello"));
    }
}

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

/// Sanitized events exposed to browser clients over SSE.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicAgentEvent {
    /// Iteration counter.
    Iteration { iteration: usize },
    /// Tool execution started.
    ToolStart { name: String, input_redacted: bool },
    /// Tool execution completed.
    ToolResult {
        name: String,
        is_error: bool,
        duration_ms: u128,
        preview_redacted: bool,
    },
    /// Text delta from LLM (streaming).
    TextDelta { delta: String },
    /// Final response.
    FinalResponse { text: String },
    /// Error occurred.
    Error { message: String },
}

impl From<AgentEvent> for PublicAgentEvent {
    fn from(event: AgentEvent) -> Self {
        match event {
            AgentEvent::Iteration { iteration } => Self::Iteration { iteration },
            AgentEvent::ToolStart { name, .. } => Self::ToolStart {
                name,
                input_redacted: true,
            },
            AgentEvent::ToolResult {
                name,
                is_error,
                duration_ms,
                ..
            } => Self::ToolResult {
                name,
                is_error,
                duration_ms,
                preview_redacted: true,
            },
            AgentEvent::TextDelta { delta } => Self::TextDelta { delta },
            AgentEvent::FinalResponse { text } => Self::FinalResponse { text },
            AgentEvent::Error { message } => Self::Error { message },
        }
    }
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

    #[test]
    fn public_tool_event_redacts_sensitive_fields() {
        let event = PublicAgentEvent::from(AgentEvent::ToolResult {
            name: "web_fetch".to_string(),
            is_error: false,
            preview: "secret".to_string(),
            duration_ms: 42,
        });

        let json = serde_json::to_string(&event).unwrap();

        assert!(json.contains("tool_result"));
        assert!(json.contains("\"preview_redacted\":true"));
        assert!(!json.contains("secret"));
    }
}

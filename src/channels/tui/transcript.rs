//! Conversation transcript and active-turn state.

use crate::agent_loop::event::AgentEvent;
use crate::llm::Message;

/// A committed conversation block that can be sent to terminal scrollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    User(String),
    Assistant(String),
    Tool(ToolBlock),
    Error(String),
}

/// The stable representation of one tool card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolBlock {
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) input: String,
    pub(crate) status: ToolStatus,
}

/// Tool card lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolStatus {
    Running,
    Completed {
        is_error: bool,
        preview: String,
        duration_ms: u128,
    },
}

/// State that is redrawn while a turn is running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ActiveTurn {
    pub(crate) buffer: String,
    pub(crate) tool_cards: Vec<ToolBlock>,
    pub(crate) iteration: Option<usize>,
}

/// Owns committed blocks and the currently streaming turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Transcript {
    committed: Vec<Block>,
    pending_commit: Vec<Block>,
    active: Option<ActiveTurn>,
}

impl Transcript {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Builds a transcript from messages already persisted for a session.
    pub(crate) fn from_messages(messages: &[Message]) -> Self {
        let mut transcript = Self::new();
        for message in messages {
            let content = message.content.as_text_lossy();
            match message.role.as_str() {
                "user" => transcript.commit(Block::User(content)),
                "assistant" | "system" | "tool" => transcript.commit(Block::Assistant(content)),
                _ => transcript.commit(Block::Assistant(content)),
            }
        }
        transcript
    }

    pub(crate) fn active(&self) -> Option<&ActiveTurn> {
        self.active.as_ref()
    }

    pub(crate) fn begin_turn(&mut self, prompt: impl Into<String>) {
        self.commit(Block::User(prompt.into()));
        self.active = Some(ActiveTurn {
            buffer: String::new(),
            tool_cards: Vec::new(),
            iteration: None,
        });
    }

    pub(crate) fn clear(&mut self) {
        self.committed.clear();
        self.pending_commit.clear();
        self.active = None;
    }

    /// Returns blocks that have not yet been rendered into scrollback.
    pub(crate) fn drain_pending(&mut self) -> Vec<Block> {
        std::mem::take(&mut self.pending_commit)
    }

    /// Applies one agent lifecycle event to the pure UI state.
    pub(crate) fn apply_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Iteration { iteration } => {
                self.ensure_active().iteration = Some(iteration);
            }
            AgentEvent::Delta { text } => {
                self.ensure_active().buffer.push_str(&text);
            }
            AgentEvent::ToolStart {
                call_id,
                name,
                input,
            } => {
                self.ensure_active().tool_cards.push(ToolBlock {
                    call_id,
                    name,
                    input: summarize_json(&input),
                    status: ToolStatus::Running,
                });
            }
            AgentEvent::ToolResult {
                call_id,
                name,
                is_error,
                preview,
                duration_ms,
            } => {
                let active = self.ensure_active();
                if let Some(card) = active
                    .tool_cards
                    .iter_mut()
                    .find(|card| card.call_id == call_id)
                {
                    card.status = ToolStatus::Completed {
                        is_error,
                        preview,
                        duration_ms,
                    };
                } else {
                    active.tool_cards.push(ToolBlock {
                        call_id,
                        name,
                        input: String::new(),
                        status: ToolStatus::Completed {
                            is_error,
                            preview,
                            duration_ms,
                        },
                    });
                }
            }
            AgentEvent::FinalResponse { text } => {
                let active = self.active.take().unwrap_or_default();
                for card in active.tool_cards {
                    self.commit(Block::Tool(card));
                }
                self.commit(Block::Assistant(text));
            }
            AgentEvent::Error { message } => {
                let active = self.active.take().unwrap_or_default();
                for card in active.tool_cards {
                    self.commit(Block::Tool(card));
                }
                self.commit(Block::Error(message));
            }
        }
    }

    fn ensure_active(&mut self) -> &mut ActiveTurn {
        self.active.get_or_insert_with(|| ActiveTurn {
            buffer: String::new(),
            tool_cards: Vec::new(),
            iteration: None,
        })
    }

    fn commit(&mut self, block: Block) {
        self.committed.push(block.clone());
        self.pending_commit.push(block);
    }
}

fn summarize_json(value: &serde_json::Value) -> String {
    let text = value.to_string().replace(['\n', '\r'], " ");
    crate::channels::utils::text::truncate_by_chars(&text, 120)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delta_accumulates_into_active_turn() {
        // Arrange
        let mut transcript = Transcript::new();
        transcript.begin_turn("hello");

        // Act
        transcript.apply_agent_event(AgentEvent::Delta {
            text: "one".to_string(),
        });
        transcript.apply_agent_event(AgentEvent::Delta {
            text: " two".to_string(),
        });

        // Assert
        assert_eq!(
            transcript.active().map(|active| active.buffer.as_str()),
            Some("one two")
        );
    }

    #[test]
    fn tool_card_transitions_to_result() {
        // Arrange
        let mut transcript = Transcript::new();
        transcript.begin_turn("run");
        transcript.apply_agent_event(AgentEvent::ToolStart {
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            input: json!({"command": "pwd"}),
        });

        // Act
        transcript.apply_agent_event(AgentEvent::ToolResult {
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            is_error: false,
            preview: "/tmp".to_string(),
            duration_ms: 12,
        });

        // Assert
        assert_eq!(
            transcript
                .active()
                .and_then(|active| active.tool_cards.first()),
            Some(&ToolBlock {
                call_id: "call-1".to_string(),
                name: "shell".to_string(),
                input: "{\"command\":\"pwd\"}".to_string(),
                status: ToolStatus::Completed {
                    is_error: false,
                    preview: "/tmp".to_string(),
                    duration_ms: 12,
                },
            })
        );
    }

    #[test]
    fn final_response_commits_and_clears_active() {
        // Arrange
        let mut transcript = Transcript::new();
        transcript.begin_turn("hello");
        transcript.apply_agent_event(AgentEvent::Delta {
            text: "partial".to_string(),
        });

        // Act
        transcript.apply_agent_event(AgentEvent::FinalResponse {
            text: "**final**".to_string(),
        });

        // Assert
        assert!(transcript.active().is_none());
        let pending = transcript.drain_pending();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending.last(),
            Some(&Block::Assistant("**final**".to_string()))
        );
    }

    #[test]
    fn error_event_commits_error_block() {
        // Arrange
        let mut transcript = Transcript::new();
        transcript.begin_turn("hello");

        // Act
        transcript.apply_agent_event(AgentEvent::Error {
            message: "failed".to_string(),
        });

        // Assert
        assert!(transcript.active().is_none());
        assert_eq!(
            transcript.drain_pending().last(),
            Some(&Block::Error("failed".to_string()))
        );
    }
}

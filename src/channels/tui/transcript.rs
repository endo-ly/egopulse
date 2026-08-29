//! Conversation transcript and active-turn state.

use crate::agent_loop::event::AgentEvent;
use crate::agent_loop::message_format::{is_tool_error_message, tool_result_body};
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
        duration_ms: Option<u128>,
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
        let mut blocks = Vec::new();
        for message in messages {
            let content = message.content.as_text_lossy();
            match message.role.as_str() {
                "user" => blocks.push(Block::User(content)),
                "assistant" => {
                    if !content.trim().is_empty() {
                        blocks.push(Block::Assistant(content));
                    }
                    blocks.extend(message.tool_calls.iter().map(|tool_call| {
                        Block::Tool(ToolBlock {
                            call_id: tool_call.id.clone(),
                            name: tool_call.name.clone(),
                            input: summarize_json(&tool_call.arguments),
                            status: ToolStatus::Running,
                        })
                    }));
                }
                "tool" => restore_tool_result(&mut blocks, message),
                "system" => blocks.push(Block::Assistant(content)),
                _ => blocks.push(Block::Assistant(content)),
            }
        }
        for block in blocks {
            transcript.commit(block);
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
                        duration_ms: Some(duration_ms),
                    };
                } else {
                    active.tool_cards.push(ToolBlock {
                        call_id,
                        name,
                        input: String::new(),
                        status: ToolStatus::Completed {
                            is_error,
                            preview,
                            duration_ms: Some(duration_ms),
                        },
                    });
                }
            }
            AgentEvent::UserInputInjected { text, .. } => {
                let (buffer, cards) = {
                    let active = self.ensure_active();
                    (
                        std::mem::take(&mut active.buffer),
                        std::mem::take(&mut active.tool_cards),
                    )
                };
                if !buffer.trim().is_empty() {
                    self.commit(Block::Assistant(buffer));
                }
                for card in cards {
                    self.commit(Block::Tool(card));
                }
                self.commit(Block::User(text));
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

fn restore_tool_result(blocks: &mut Vec<Block>, message: &Message) {
    let preview = crate::channels::utils::text::truncate_by_chars(
        &tool_result_body(&message.content.as_text_lossy()),
        120,
    );
    let status = ToolStatus::Completed {
        is_error: is_tool_error_message(message),
        preview,
        duration_ms: None,
    };

    if let Some(tool_call_id) = message.tool_call_id.as_deref()
        && let Some(Block::Tool(tool)) = blocks
            .iter_mut()
            .rev()
            .find(|block| matches!(block, Block::Tool(tool) if tool.call_id == tool_call_id))
    {
        tool.status = status;
        return;
    }

    blocks.push(Block::Tool(ToolBlock {
        call_id: message.tool_call_id.clone().unwrap_or_default(),
        name: "tool".to_string(),
        input: String::new(),
        status,
    }));
}

fn summarize_json(value: &serde_json::Value) -> String {
    let text = value.to_string().replace(['\n', '\r'], " ");
    crate::channels::utils::text::truncate_by_chars(&text, 120)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Message, MessageContent, ToolCall};
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
                    duration_ms: Some(12),
                },
            })
        );
    }

    #[test]
    fn persisted_tool_messages_restore_as_tool_cards() {
        // Arrange
        let messages = vec![
            Message::text("user", "inspect"),
            Message {
                role: "assistant".to_string(),
                content: MessageContent::text(""),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "shell".to_string(),
                    arguments: json!({"command": "pwd"}),
                }],
                tool_call_id: None,
            },
            Message {
                role: "tool".to_string(),
                content: MessageContent::text(
                    r#"{"tool":"shell","status":"success","result":"/tmp"}"#,
                ),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: Some("call-1".to_string()),
            },
            Message::text("assistant", "The working directory is /tmp."),
        ];

        // Act
        let mut transcript = Transcript::from_messages(&messages);
        let pending = transcript.drain_pending();

        // Assert
        assert_eq!(pending.len(), 3);
        assert!(matches!(pending[0], Block::User(ref text) if text == "inspect"));
        assert!(matches!(
            pending[1],
            Block::Tool(ToolBlock {
                ref name,
                ref status,
                ..
            }) if name == "shell"
                && *status == ToolStatus::Completed {
                    is_error: false,
                    preview: "/tmp".to_string(),
                    duration_ms: None,
                }
        ));
        assert!(matches!(
            pending[2],
            Block::Assistant(ref text) if text == "The working directory is /tmp."
        ));
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
    fn injected_user_input_commits_after_tool_cards() {
        // Arrange
        let mut transcript = Transcript::new();
        transcript.begin_turn("hello");
        transcript.apply_agent_event(AgentEvent::Delta {
            text: "before".to_string(),
        });
        transcript.apply_agent_event(AgentEvent::ToolStart {
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            input: json!({"command": "pwd"}),
        });
        transcript.apply_agent_event(AgentEvent::ToolResult {
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            is_error: false,
            preview: "/tmp".to_string(),
            duration_ms: 12,
        });

        // Act
        transcript.apply_agent_event(AgentEvent::UserInputInjected {
            message_id: "tui:follow-up".to_string(),
            sender_id: "user-b".to_string(),
            text: "follow-up".to_string(),
            timestamp: "2026-08-28T12:00:00Z".to_string(),
        });
        transcript.apply_agent_event(AgentEvent::Delta {
            text: "after".to_string(),
        });
        transcript.apply_agent_event(AgentEvent::FinalResponse {
            text: "after".to_string(),
        });

        // Assert
        let pending = transcript.drain_pending();
        assert_eq!(pending.len(), 5);
        assert_eq!(pending[1], Block::Assistant("before".to_string()));
        assert!(matches!(pending[2], Block::Tool(_)));
        assert_eq!(pending[3], Block::User("follow-up".to_string()));
        assert_eq!(pending[4], Block::Assistant("after".to_string()));
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

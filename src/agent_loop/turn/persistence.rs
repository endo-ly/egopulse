//! Turn-scoped Message and Session persistence.

use std::sync::Arc;

use crate::agent_loop::TurnRuntime;
use crate::agent_loop::compaction::{PromptContext, maybe_compact_messages};
use crate::agent_loop::event::{AgentEvent, EventEmitter};
use crate::agent_loop::model_step::AssistantToolPhase;
use crate::agent_loop::session::{PersistedTurn, persist_phase, persist_phase_messages};
use crate::agent_loop::tool_execution::ToolResultPhase;
use crate::conversation::{ConversationScope, SurfaceContext};
use crate::error::{EgoPulseError, StorageError};
use crate::llm::{LlmProvider, Message};
use crate::storage::StoredMessage;

/// Owns Message and Session persistence for one Turn.
pub(super) struct TurnPersistence<'a> {
    runtime: &'a TurnRuntime,
    context: &'a SurfaceContext,
    chat_id: i64,
    turn_id: String,
    agent_id: String,
}

impl<'a> TurnPersistence<'a> {
    /// Creates a persistence boundary for one Turn.
    pub(super) fn new(
        runtime: &'a TurnRuntime,
        context: &'a SurfaceContext,
        chat_id: i64,
        turn_id: &str,
        agent_id: &str,
    ) -> Self {
        Self {
            runtime,
            context,
            chat_id,
            turn_id: turn_id.to_string(),
            agent_id: agent_id.to_string(),
        }
    }

    /// Persists the user input and the resulting session snapshot.
    pub(super) async fn persist_user_input(
        &self,
        input_message_id: &str,
        user_message: &Message,
        user_input: &str,
        llm: &Arc<dyn LlmProvider>,
        prompt_ctx: &PromptContext<'_>,
        config_snapshot: &crate::config::manager::ConfigSnapshot,
    ) -> Result<(Arc<Vec<Message>>, Option<i64>), EgoPulseError> {
        persist_user_turn_with_compaction(
            self.runtime,
            self.context,
            self.chat_id,
            &self.turn_id,
            input_message_id,
            user_message,
            user_input,
            llm,
            prompt_ctx,
            config_snapshot,
        )
        .await
    }

    /// Persists the final assistant message and emits its final-response event.
    pub(super) async fn persist_final(
        &self,
        final_message_id: &str,
        messages: &mut Arc<Vec<Message>>,
        session_revision: Option<i64>,
        on_event: &EventEmitter,
        response: (String, Option<String>),
    ) -> Result<String, EgoPulseError> {
        persist_and_finalize(
            self.runtime,
            self.context.scope,
            self.chat_id,
            &self.agent_id,
            final_message_id,
            &self.turn_id,
            messages,
            session_revision,
            on_event,
            response,
        )
        .await
    }

    /// Persists an assistant Tool Call message before execution begins.
    pub(super) async fn persist_tool_call(
        &self,
        assistant_message_id: &str,
        assistant_phase: &AssistantToolPhase,
        messages: Vec<Message>,
        session_revision: Option<i64>,
    ) -> Result<PersistedTurn, EgoPulseError> {
        persist_tool_call_assistant_message(
            self.runtime,
            self.context.scope,
            self.chat_id,
            &self.agent_id,
            assistant_message_id,
            &self.turn_id,
            assistant_phase,
            messages,
            session_revision,
        )
        .await
    }

    /// Persists Tool Result messages after execution completes.
    pub(super) async fn persist_tool_results(
        &self,
        assistant_message_id: &str,
        messages: Vec<Message>,
        tool_result_phase: ToolResultPhase,
        session_revision: Option<i64>,
    ) -> Result<PersistedTurn, EgoPulseError> {
        persist_tool_result_messages(
            self.runtime,
            self.context.scope,
            self.chat_id,
            &self.agent_id,
            assistant_message_id,
            &self.turn_id,
            messages,
            tool_result_phase,
            session_revision,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_and_finalize(
    state: &TurnRuntime,
    scope: ConversationScope,
    chat_id: i64,
    agent_id: &str,
    final_message_id: &str,
    turn_id: &str,
    messages: &mut Arc<Vec<Message>>,
    session_revision: Option<i64>,
    on_event: &EventEmitter,
    response: (String, Option<String>),
) -> Result<String, EgoPulseError> {
    let (final_content, reasoning_content) = response;
    let mut assistant_message = Message::text("assistant", final_content.clone());
    assistant_message.reasoning_content = reasoning_content;
    let mut updated = Arc::try_unwrap(std::mem::replace(messages, Arc::new(Vec::new())))
        .unwrap_or_else(|arc| (*arc).clone());
    updated.push(assistant_message.clone());

    let mut stored = StoredMessage::assistant(chat_id, agent_id.to_string(), final_content.clone());
    stored.id = final_message_id.to_string();
    stored.turn_id = Some(turn_id.to_string());
    let _persisted = persist_phase(
        state,
        scope,
        stored,
        assistant_message,
        &updated,
        session_revision,
    )
    .await?;

    *messages = Arc::new(updated);

    on_event.emit(AgentEvent::FinalResponse {
        text: final_content.clone(),
    });
    Ok(final_content)
}
#[allow(clippy::too_many_arguments)]
async fn persist_tool_call_assistant_message(
    state: &TurnRuntime,
    scope: ConversationScope,
    chat_id: i64,
    agent_id: &str,
    assistant_message_id: &str,
    turn_id: &str,
    assistant_phase: &AssistantToolPhase,
    mut messages: Vec<Message>,
    session_revision: Option<i64>,
) -> Result<PersistedTurn, EgoPulseError> {
    let assistant_message = assistant_phase.assistant_message.clone();
    messages.push(assistant_message.clone());

    persist_phase(
        state,
        scope,
        StoredMessage {
            id: assistant_message_id.to_string(),
            turn_id: Some(turn_id.to_string()),
            ..StoredMessage::assistant(
                chat_id,
                agent_id.to_string(),
                assistant_phase.assistant_preview.clone(),
            )
        },
        assistant_message,
        &messages,
        session_revision,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn persist_tool_result_messages(
    state: &TurnRuntime,
    scope: ConversationScope,
    chat_id: i64,
    agent_id: &str,
    assistant_message_id: &str,
    turn_id: &str,
    messages: Vec<Message>,
    tool_result_phase: ToolResultPhase,
    session_revision: Option<i64>,
) -> Result<PersistedTurn, EgoPulseError> {
    let ToolResultPhase {
        tool_messages,
        tool_result_preview,
    } = tool_result_phase;
    if tool_messages.is_empty() {
        return Ok(PersistedTurn {
            revision: session_revision.unwrap_or(0),
            messages,
        });
    }

    let mut messages_with_tools = messages;
    messages_with_tools.extend(tool_messages.iter().cloned());
    let mut tool_summary =
        StoredMessage::assistant(chat_id, agent_id.to_string(), tool_result_preview);
    tool_summary.turn_id = Some(turn_id.to_string());
    tool_summary.parent_message_id = Some(assistant_message_id.to_string());
    persist_phase_messages(
        state,
        scope,
        tool_summary,
        tool_messages,
        &messages_with_tools,
        session_revision,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn persist_user_turn_with_compaction(
    state: &TurnRuntime,
    context: &SurfaceContext,
    chat_id: i64,
    turn_id: &str,
    input_message_id: &str,
    user_message: &Message,
    user_input: &str,
    llm: &std::sync::Arc<dyn crate::llm::LlmProvider>,
    prompt_ctx: &PromptContext<'_>,
    config_snapshot: &crate::config::manager::ConfigSnapshot,
) -> Result<(Arc<Vec<Message>>, Option<i64>), EgoPulseError> {
    let config = &config_snapshot.config;
    let mut loaded = crate::agent_loop::session::load_messages_for_turn_with_limit(
        state,
        context.scope,
        chat_id,
        config.max_history_messages,
    )
    .await?;
    let mut stored_message = StoredMessage::user(
        chat_id,
        context.surface_user.clone(),
        user_input.to_string(),
    );
    stored_message.id = input_message_id.to_string();
    stored_message.turn_id = Some(turn_id.to_string());

    for attempt in 0..2 {
        let current_messages = std::mem::replace(&mut loaded.messages, Arc::new(Vec::new()));
        let mut candidate_messages =
            Arc::try_unwrap(current_messages).unwrap_or_else(|arc| (*arc).clone());
        candidate_messages.push(user_message.clone());
        let candidate_messages = maybe_compact_messages(
            state,
            context,
            chat_id,
            &candidate_messages,
            llm,
            prompt_ctx,
            config,
        )
        .await?;

        let persist_result = crate::agent_loop::session::commit_user_turn_input(
            state,
            context.scope,
            stored_message.clone(),
            &candidate_messages,
            loaded.session_revision,
            turn_id,
            config_snapshot,
        )
        .await;
        let persisted = match persist_result {
            Ok(persisted) => persisted,
            Err(error) => {
                loaded = handle_user_turn_persist_error(
                    state,
                    context.scope,
                    chat_id,
                    attempt,
                    error,
                    config.max_history_messages,
                )
                .await?;
                continue;
            }
        };

        return Ok((Arc::new(persisted.messages), Some(persisted.revision)));
    }

    Err(EgoPulseError::Storage(
        StorageError::SessionSnapshotConflict,
    ))
}

async fn handle_user_turn_persist_error(
    state: &TurnRuntime,
    scope: ConversationScope,
    chat_id: i64,
    attempt: usize,
    error: EgoPulseError,
    max_history_messages: usize,
) -> Result<crate::agent_loop::session::LoadedSession, EgoPulseError> {
    match persist_phase_conflict_outcome(attempt, error) {
        PersistConflictOutcome::Reload => {
            crate::agent_loop::session::load_messages_for_turn_with_limit(
                state,
                scope,
                chat_id,
                max_history_messages,
            )
            .await
        }
        PersistConflictOutcome::Return(error) => Err(error),
    }
}
enum PersistConflictOutcome {
    Reload,
    Return(EgoPulseError),
}

fn persist_phase_conflict_outcome(attempt: usize, error: EgoPulseError) -> PersistConflictOutcome {
    match error {
        EgoPulseError::Storage(StorageError::SessionSnapshotConflict) if attempt == 0 => {
            PersistConflictOutcome::Reload
        }
        other => PersistConflictOutcome::Return(other),
    }
}

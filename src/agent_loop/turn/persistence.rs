//! Turn-scoped Message and Session persistence.

use std::sync::Arc;

use crate::agent_loop::TurnDependencies;
use crate::agent_loop::compaction::{PromptContext, maybe_compact_messages};
use crate::agent_loop::event::{AgentEvent, EventEmitter};
use crate::agent_loop::message_format::format_direct_input;
use crate::agent_loop::model_step::AssistantToolPhase;
use crate::agent_loop::session::{
    PersistedTurn, persist_phase, persist_phase_messages, serialize_snapshot,
};
use crate::agent_loop::tool_execution::ToolResultPhase;
use crate::conversation::SurfaceContext;
use crate::error::{EgoPulseError, StorageError};
use crate::llm::{LlmProvider, Message};
use crate::storage::StoredMessage;

/// Owns Message and Session persistence for one Turn.
pub(crate) struct TurnPersistence<'a> {
    runtime: &'a TurnDependencies,
    context: &'a SurfaceContext,
    chat_id: i64,
    turn_id: String,
}

pub(super) struct UserInput<'a> {
    pub(super) message_id: &'a str,
    pub(super) message: &'a Message,
    pub(super) input: &'a str,
    pub(super) received_at: &'a str,
}

impl<'a> TurnPersistence<'a> {
    /// Creates a persistence boundary for one Turn.
    pub(crate) fn new(
        runtime: &'a TurnDependencies,
        context: &'a SurfaceContext,
        chat_id: i64,
        turn_id: &str,
    ) -> Self {
        Self {
            runtime,
            context,
            chat_id,
            turn_id: turn_id.to_string(),
        }
    }

    /// Persists the user input and the resulting session snapshot.
    pub(super) async fn persist_user_input(
        &self,
        input: UserInput<'_>,
        llm: &Arc<dyn LlmProvider>,
        prompt_ctx: &PromptContext<'_>,
        config_snapshot: &crate::config::manager::ConfigSnapshot,
    ) -> Result<(Arc<Vec<Message>>, Option<i64>), EgoPulseError> {
        let config = &config_snapshot.config;
        let mut loaded = crate::agent_loop::session::load_messages_for_turn_with_limit(
            self.runtime,
            self.context.scope,
            self.chat_id,
            config.max_history_messages,
        )
        .await?;
        let mut stored_message = StoredMessage::user(
            self.chat_id,
            self.context.surface_user.clone(),
            input.input.to_string(),
        );
        stored_message.id = input.message_id.to_string();
        stored_message.turn_id = Some(self.turn_id.clone());
        stored_message.timestamp = input.received_at.to_string();

        for attempt in 0..2 {
            let current_messages = std::mem::replace(&mut loaded.messages, Arc::new(Vec::new()));
            let mut candidate_messages =
                Arc::try_unwrap(current_messages).unwrap_or_else(|arc| (*arc).clone());
            candidate_messages.push(input.message.clone());
            let candidate_messages = maybe_compact_messages(
                self.runtime,
                self.context,
                self.chat_id,
                &candidate_messages,
                llm,
                prompt_ctx,
                config,
            )
            .await?;

            let persist_result = crate::agent_loop::session::commit_user_turn_input(
                self.runtime,
                self.context.scope,
                stored_message.clone(),
                &candidate_messages,
                loaded.session_revision,
                &self.turn_id,
                config_snapshot,
            )
            .await;
            let persisted = match persist_result {
                Ok(persisted) => persisted,
                Err(error) => {
                    loaded = match error {
                        EgoPulseError::Storage(StorageError::SessionSnapshotConflict)
                            if attempt == 0 =>
                        {
                            crate::agent_loop::session::load_messages_for_turn_with_limit(
                                self.runtime,
                                self.context.scope,
                                self.chat_id,
                                config.max_history_messages,
                            )
                            .await?
                        }
                        other => return Err(other),
                    };
                    continue;
                }
            };

            return Ok((Arc::new(persisted.messages), Some(persisted.revision)));
        }

        Err(EgoPulseError::Storage(
            StorageError::SessionSnapshotConflict,
        ))
    }

    /// Persists the final assistant message and emits its final-response event.
    pub(crate) async fn persist_final(
        &self,
        final_message_id: &str,
        messages: Arc<Vec<Message>>,
        session_revision: Option<i64>,
        on_event: &EventEmitter,
        response: (String, Option<String>),
    ) -> Result<String, EgoPulseError> {
        let (final_content, reasoning_content) = response;
        let mut assistant_message = Message::text("assistant", final_content.clone());
        assistant_message.reasoning_content = reasoning_content;
        let mut updated = Arc::try_unwrap(messages).unwrap_or_else(|arc| (*arc).clone());
        updated.push(assistant_message.clone());

        let mut stored = StoredMessage::assistant(
            self.chat_id,
            self.context.agent_id.clone(),
            final_content.clone(),
        );
        stored.id = final_message_id.to_string();
        stored.turn_id = Some(self.turn_id.clone());
        let _persisted = persist_phase(
            self.runtime,
            self.context.scope,
            stored,
            assistant_message,
            &updated,
            session_revision,
        )
        .await?;

        on_event.emit(AgentEvent::FinalResponse {
            turn_id: self.turn_id.clone(),
            // The final row was just persisted above under
            // `final_message_id`, which is also the id the response
            // streamed under — delivery adopts nothing.
            assistant_message_id: final_message_id.to_string(),
            text: final_content.clone(),
            terminal: false,
        });
        Ok(final_content)
    }

    /// Persists an assistant Tool Call message before execution begins.
    pub(crate) async fn persist_tool_call(
        &self,
        assistant_message_id: &str,
        assistant_phase: &AssistantToolPhase,
        mut messages: Vec<Message>,
        session_revision: Option<i64>,
    ) -> Result<PersistedTurn, EgoPulseError> {
        let assistant_message = assistant_phase.assistant_message.clone();
        messages.push(assistant_message.clone());

        persist_phase(
            self.runtime,
            self.context.scope,
            StoredMessage {
                id: assistant_message_id.to_string(),
                turn_id: Some(self.turn_id.clone()),
                ..StoredMessage::assistant(
                    self.chat_id,
                    self.context.agent_id.clone(),
                    assistant_phase.assistant_preview.clone(),
                )
            },
            assistant_message,
            &messages,
            session_revision,
        )
        .await
    }

    /// Persists Tool Result messages after execution completes.
    pub(crate) async fn persist_tool_results(
        &self,
        assistant_message_id: &str,
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
        let mut tool_summary = StoredMessage::assistant(
            self.chat_id,
            self.context.agent_id.clone(),
            tool_result_preview,
        );
        tool_summary.turn_id = Some(self.turn_id.clone());
        tool_summary.parent_message_id = Some(assistant_message_id.to_string());
        persist_phase_messages(
            self.runtime,
            self.context.scope,
            tool_summary,
            tool_messages,
            &messages_with_tools,
            session_revision,
        )
        .await
    }

    /// Promotes staged human follow-ups after the Tool phase has completed.
    ///
    /// The candidate LLM snapshot is built from the current loop messages and
    /// the staged rows in FIFO order. Storage then assigns causal sequence
    /// numbers and writes the snapshot atomically. A concurrent snapshot
    /// update is handled by reloading the latest committed snapshot once.
    pub(crate) async fn commit_staged_user_messages(
        &self,
        messages: Arc<Vec<Message>>,
        session_revision: Option<i64>,
        config_snapshot: &crate::config::manager::ConfigSnapshot,
        on_event: &EventEmitter,
    ) -> Result<PersistedTurn, EgoPulseError> {
        let Some(mut revision) = session_revision else {
            let turn_id = self.turn_id.clone();
            let staged =
                crate::storage::call_blocking(self.runtime.db_for(self.context.scope), move |db| {
                    db.list_staged_user_messages(&turn_id)
                })
                .await?;
            if staged.is_empty() {
                return Ok(PersistedTurn {
                    revision: 0,
                    messages: (*messages).clone(),
                });
            }
            return Err(EgoPulseError::Storage(StorageError::Conflict(
                "staged follow-up requires an existing session snapshot".to_string(),
            )));
        };
        let mut current_messages = messages;

        for attempt in 0..2 {
            let turn_id = self.turn_id.clone();
            let staged =
                crate::storage::call_blocking(self.runtime.db_for(self.context.scope), move |db| {
                    db.list_staged_user_messages(&turn_id)
                })
                .await?;
            if staged.is_empty() {
                return Ok(PersistedTurn {
                    revision,
                    messages: (*current_messages).clone(),
                });
            }

            let mut candidate = (*current_messages).clone();
            for message in &staged {
                candidate.push(Message::text(
                    "user",
                    format_direct_input(
                        &message.content,
                        &message.timestamp,
                        &config_snapshot.config.timezone,
                    ),
                ));
            }
            let session_json =
                serialize_snapshot(Arc::clone(&self.runtime.assets), candidate.clone())
                    .await
                    .map_err(|error| match error {
                        EgoPulseError::Storage(storage) => storage,
                        other => StorageError::TaskJoin(other.to_string()),
                    })?;
            let turn_id = self.turn_id.clone();
            let committed =
                crate::storage::call_blocking(self.runtime.db_for(self.context.scope), move |db| {
                    db.commit_staged_user_messages(&turn_id, &session_json, revision)
                })
                .await;
            match committed {
                Ok(committed) => {
                    for message in &committed.messages {
                        on_event.emit(AgentEvent::UserInputInjected {
                            message_id: message.id.clone(),
                            sender_id: message.sender_id.clone(),
                            text: message.content.clone(),
                            timestamp: message.timestamp.clone(),
                        });
                    }
                    return Ok(PersistedTurn {
                        revision: committed.revision,
                        messages: candidate,
                    });
                }
                Err(StorageError::SessionSnapshotConflict) if attempt == 0 => {
                    let loaded = crate::agent_loop::session::load_messages_for_turn_with_limit(
                        self.runtime,
                        self.context.scope,
                        self.chat_id,
                        config_snapshot.config.max_history_messages,
                    )
                    .await?;
                    current_messages = loaded.messages;
                    revision = loaded.session_revision.ok_or_else(|| {
                        EgoPulseError::Storage(StorageError::SessionSnapshotConflict)
                    })?;
                }
                Err(error) => return Err(EgoPulseError::Storage(error)),
            }
        }

        Err(EgoPulseError::Storage(
            StorageError::SessionSnapshotConflict,
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::agent_loop::event::{AgentEvent, EventEmitter};
    use crate::agent_loop::process_turn;
    use crate::agent_loop::test_support::{
        RecordingProvider, build_state_with_provider, cli_context,
    };
    use crate::agent_loop::turn::lifecycle::TurnLifecycle;
    use crate::conversation::{ConversationScope, SurfaceContext};
    use crate::llm::{Message, MessagesResponse, ToolCall};
    use crate::runtime::AppState;
    use crate::storage::{AcceptTurnParams, StoredMessage, TurnRunState, call_blocking};
    use serial_test::serial;
    use std::sync::Arc;

    fn context_with_request_key(session: &str, request_key: &str) -> SurfaceContext {
        let mut context = cli_context(session);
        context.request_key = request_key.to_string();
        context
    }

    /// A persisted message row's Turn-linkage fields, in `seq` order.
    struct MessageLink {
        id: String,
        turn_id: Option<String>,
        parent_message_id: Option<String>,
    }

    fn message_turn_links(state: &AppState, chat_id: i64) -> Vec<MessageLink> {
        let conn = state.db.get_conn().expect("conn");
        let mut stmt = conn
            .prepare(
                "SELECT seq, id, turn_id, parent_message_id FROM messages
                 WHERE chat_id = ?1 ORDER BY seq",
            )
            .expect("prepare");
        stmt.query_map(rusqlite::params![chat_id], |row| {
            Ok(MessageLink {
                id: row.get(1)?,
                turn_id: row.get(2)?,
                parent_message_id: row.get(3)?,
            })
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect")
    }

    #[tokio::test]
    #[serial]
    async fn turn_stamps_turn_id_on_all_persisted_messages() {
        // Arrange: a Turn that runs one tool call, then finalizes.
        let dir = tempfile::tempdir().expect("tempdir");
        let relative_path = format!("tests/{}/turnid.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "checking".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "call-turnid".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": relative_path}),
                    }],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "done".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        let note_path = workspace.join(&relative_path);
        std::fs::create_dir_all(note_path.parent().expect("parent")).expect("dir");
        std::fs::write(&note_path, "content").expect("file");
        let context = context_with_request_key("turn-id-links", "cli:turnid:1");

        // Act
        let reply = process_turn(&state.turn_dependencies(), &context, "read the note")
            .await
            .expect("turn");
        assert_eq!(reply, "done");

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:turn-id-links:agent:default",
                Some("turn-id-links"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");

        // Assert: every persisted message carries the owning Turn id. The Tool
        // Result message also references the issuing assistant message as its
        // parent.
        let turn_id: String = state
            .db
            .get_conn()
            .expect("conn")
            .query_row(
                "SELECT turn_id FROM turn_runs WHERE chat_id = ?1",
                rusqlite::params![chat_id],
                |row| row.get(0),
            )
            .expect("turn_id");
        let rows = message_turn_links(&state, chat_id);
        assert!(!rows.is_empty(), "turn must persist messages");
        for link in &rows {
            assert_eq!(
                link.turn_id.as_deref(),
                Some(turn_id.as_str()),
                "every message must carry the owning turn_id"
            );
        }
        // The Tool Result message is the one with a parent_message_id, and it
        // points at the Tool Call assistant message.
        let parents: Vec<String> = rows
            .iter()
            .filter_map(|link| link.parent_message_id.clone())
            .collect();
        assert_eq!(parents.len(), 1, "exactly the Tool Result carries a parent");
        assert!(
            rows.iter().any(|link| link.id == parents[0]),
            "parent_message_id must reference a persisted assistant message"
        );
    }

    #[tokio::test]
    #[serial]
    async fn final_persist_then_completion_failure_keeps_one_stable_id() {
        // Arrange: a turn whose input is committed under the canonical
        // request key, then whose final row is persisted under the streamed
        // iteration id — exactly what the loop reports live.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(vec![], vec![]);
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let context = context_with_request_key("final-failure", "cli:final-failure:1");
        let runtime = state.turn_dependencies();
        let chat_id = crate::agent_loop::resolve_chat_id(&runtime, &context)
            .await
            .expect("chat id");
        let turn_id = call_blocking(Arc::clone(&state.db), move |db| {
            match db.accept_or_get_turn(AcceptTurnParams {
                chat_id,
                request_key: "cli:final-failure:1",
                config_revision: 1,
                config_fingerprint: Some("fingerprint"),
                request_payload_hash: "payload",
                origin_id: None,
                scheduled_request_json: None,
            })? {
                crate::storage::AcceptOutcome::Created(run) => {
                    Ok::<String, crate::error::StorageError>(run.turn_id)
                }
                crate::storage::AcceptOutcome::Existing(_) => Err(
                    crate::error::StorageError::Conflict("expected a fresh turn".to_string()),
                ),
            }
        })
        .await
        .expect("accept turn");
        let revision = call_blocking(Arc::clone(&state.db), {
            let turn_id = turn_id.clone();
            move |db| {
                let mut input =
                    StoredMessage::user(chat_id, "local_user".to_string(), "do it".to_string());
                input.id = "cli:final-failure:1".to_string();
                input.turn_id = Some(turn_id.clone());
                input.timestamp = "2026-09-12T00:00:00Z".to_string();
                db.commit_turn_input_with_conversation(&input, "[]", None, &turn_id, 1, None)
            }
        })
        .await
        .expect("commit input");
        let final_id = format!("turn:{turn_id}:assistant:1");
        let events = Arc::new(std::sync::Mutex::new(Vec::<AgentEvent>::new()));
        let event_log = Arc::clone(&events);
        let persistence = super::TurnPersistence::new(&runtime, &context, chat_id, &turn_id);
        persistence
            .persist_final(
                &final_id,
                Arc::new(vec![
                    Message::text("user", "do it"),
                    Message::text("assistant", "done"),
                ]),
                Some(revision),
                &EventEmitter::new(move |event| {
                    event_log.lock().expect("events").push(event);
                }),
                ("done".to_string(), None),
            )
            .await
            .expect("persist final");

        // Act: the turn leaves the completable state before complete() runs,
        // so completion conflicts and the failure is recorded — while the
        // final row already exists under the streamed id.
        call_blocking(Arc::clone(&state.db), {
            let turn_id = turn_id.clone();
            move |db| db.fail_turn(&turn_id, TurnRunState::Failed, "test", "boom")
        })
        .await
        .expect("fail turn");
        let lifecycle = TurnLifecycle::new(&runtime, ConversationScope::Normal, &turn_id, "");
        lifecycle
            .complete(&final_id)
            .await
            .expect_err("complete must conflict after failure");

        // Assert: the FinalResponse named the streamed id before completion
        // failed — the live transcript and the durable row agree already.
        {
            let events = events.lock().expect("events");
            assert!(
                matches!(
                    &events[..],
                    [AgentEvent::FinalResponse {
                        assistant_message_id,
                        text,
                        ..
                    }] if assistant_message_id == &final_id && text == "done"
                ),
                "final event must name the streamed id: {events:?}"
            );
        }

        // Assert: the failed turn keeps exactly one final row under the
        // streamed id. A history refetch returns it under the same id the
        // live transcript already shows, so the merge converges instead of
        // duplicating — and the failed completion stamped nothing else.
        let history = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_all_messages(chat_id)
        })
        .await
        .expect("history");
        let finals: Vec<_> = history
            .iter()
            .filter(|message| message.id == final_id)
            .collect();
        assert_eq!(finals.len(), 1, "one final row: {history:?}");
        assert_eq!(finals[0].content, "done");
        assert!(
            history
                .iter()
                .any(|message| message.id == "cli:final-failure:1"),
            "input keeps the request key: {history:?}"
        );
        let run = call_blocking(Arc::clone(&state.db), {
            let turn_id = turn_id.clone();
            move |db| db.get_turn_run(&turn_id)
        })
        .await
        .expect("turn row");
        assert_eq!(run.state, TurnRunState::Failed);
        assert_eq!(run.final_message_id, None);
    }
}

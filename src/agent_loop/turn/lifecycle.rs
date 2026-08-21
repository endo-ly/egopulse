//! Durable Turn acceptance, state transitions, resume validation, and failure.

use crate::agent_loop::TurnDependencies;
use crate::channels::utils::text::truncate_by_chars;
use crate::conversation::{ConversationScope, SurfaceContext};
use crate::error::EgoPulseError;
use crate::runtime::turn::StopReason;
use crate::runtime::turn::{ScheduledTurn, deserialize_scheduled_turn};
use crate::storage::{AcceptOutcome, TurnRun, TurnRunState, call_blocking};
use tracing::warn;

/// Outcome of idempotent Turn acceptance.
pub(crate) enum TurnAcceptance {
    /// A fresh `accepted` Turn created by this call; the caller owns execution.
    Proceed(Box<TurnRun>),
    /// The Turn was already `completed`; replay its saved final response.
    Completed(String),
    /// The Turn already exists and is non-terminal; another executor owns it.
    InProgress(String),
    /// The Turn already terminated in a non-success state.
    Terminated(String),
}

/// Resolves the idempotency key used to accept a user request.
pub(crate) fn resolve_request_key(context: &SurfaceContext) -> String {
    if context.request_key.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        context.request_key.clone()
    }
}

/// Owns durable state transitions for one Turn.
pub(crate) struct TurnLifecycle<'a> {
    runtime: &'a TurnDependencies,
    scope: ConversationScope,
    turn_id: String,
    origin_id: String,
}

impl<'a> TurnLifecycle<'a> {
    /// Creates a lifecycle boundary for one durable Turn.
    pub(crate) fn new(
        runtime: &'a TurnDependencies,
        scope: ConversationScope,
        turn_id: &str,
        origin_id: &str,
    ) -> Self {
        Self {
            runtime,
            scope,
            turn_id: turn_id.to_string(),
            origin_id: origin_id.to_string(),
        }
    }

    /// Accepts a request idempotently or returns the existing Turn outcome.
    pub(crate) async fn accept(
        runtime: &TurnDependencies,
        scope: ConversationScope,
        chat_id: i64,
        request_key: &str,
        payload_hash: &str,
        origin_id: &str,
        snapshot: &crate::config::manager::ConfigSnapshot,
    ) -> Result<TurnAcceptance, EgoPulseError> {
        let request_key = request_key.to_string();
        let payload_hash = payload_hash.to_string();
        let config_revision = snapshot.revision as i64;
        let config_fingerprint = snapshot.fingerprint.clone();
        let origin_id = origin_id.to_string();
        let run = call_blocking(runtime.db_for(scope), move |db| {
            db.accept_or_get_turn(crate::storage::AcceptTurnParams {
                chat_id,
                request_key: &request_key,
                config_revision,
                config_fingerprint: Some(&config_fingerprint),
                request_payload_hash: &payload_hash,
                origin_id: Some(&origin_id),
                scheduled_request_json: None,
            })
        })
        .await?;

        match run {
            AcceptOutcome::Created(run) => Ok(TurnAcceptance::Proceed(Box::new(run))),
            AcceptOutcome::Existing(run) => match run.state {
                TurnRunState::Accepted => Ok(TurnAcceptance::Proceed(Box::new(run))),
                TurnRunState::Completed => {
                    let final_message_id = run.final_message_id.clone().ok_or_else(|| {
                        EgoPulseError::Internal(
                            "completed turn_run has no final_message_id".to_string(),
                        )
                    })?;
                    let content = call_blocking(runtime.db_for(scope), move |db| {
                        db.get_message_content(&final_message_id)
                    })
                    .await?
                    .ok_or_else(|| {
                        EgoPulseError::Internal(
                            "completed turn_run final message is missing".to_string(),
                        )
                    })?;
                    Ok(TurnAcceptance::Completed(content))
                }
                other if other.is_terminal() => Ok(TurnAcceptance::Terminated(format!(
                    "このリクエストは以前に処理されましたが、状態が {other} になりました。再度お試しください。"
                ))),
                _ => Ok(TurnAcceptance::InProgress(
                    "このリクエストはすでに処理中です。".to_string(),
                )),
            },
        }
    }

    async fn transition<F>(&self, apply: F) -> Result<(), EgoPulseError>
    where
        F: FnOnce(&crate::storage::Database, &str) -> Result<(), crate::error::StorageError>
            + Send
            + 'static,
    {
        let turn_id = self.turn_id.clone();
        call_blocking(self.runtime.db_for(self.scope), move |db| {
            apply(db, &turn_id)
        })
        .await?;
        Ok(())
    }

    /// Marks model output as externally published.
    pub(crate) async fn mark_output_published(&self) {
        let turn_id = self.turn_id.clone();
        if let Err(error) = call_blocking(self.runtime.db_for(self.scope), move |db| {
            db.mark_turn_output_published(&turn_id)
        })
        .await
        {
            warn!(error = %error, turn_id = self.turn_id, "failed to mark turn_run output_published");
        }
    }

    /// Marks the current model iteration complete.
    pub(crate) async fn complete_model(&self) -> Result<(), EgoPulseError> {
        self.transition(|db, turn_id| db.complete_turn_model(turn_id))
            .await
    }

    /// Marks the tool phase as started.
    pub(crate) async fn begin_tools(&self) -> Result<(), EgoPulseError> {
        self.transition(|db, turn_id| db.begin_turn_tools(turn_id))
            .await
    }

    /// Marks the tool phase as complete.
    pub(crate) async fn complete_tools(&self) -> Result<(), EgoPulseError> {
        self.transition(|db, turn_id| db.complete_turn_tools(turn_id))
            .await
    }

    /// Completes the Turn with its persisted final message.
    pub(crate) async fn complete(&self, final_message_id: &str) -> Result<(), EgoPulseError> {
        let turn_id = self.turn_id.clone();
        let final_message_id = final_message_id.to_string();
        call_blocking(self.runtime.db_for(self.scope), move |db| {
            db.complete_turn(&turn_id, &final_message_id)
        })
        .await?;
        Ok(())
    }

    /// Records failure or uncertainty according to the durable publication state.
    pub(crate) async fn fail(&self, error: &EgoPulseError) {
        let turn_id = self.turn_id.clone();
        let run = match call_blocking(self.runtime.db_for(self.scope), {
            let turn_id = turn_id.clone();
            move |db| db.get_turn_run(&turn_id)
        })
        .await
        {
            Ok(run) => run,
            Err(load_error) => {
                warn!(error = %load_error, turn_id = self.turn_id, "failed to load turn_run for failure recording");
                return;
            }
        };
        if run.state.is_terminal() {
            return;
        }
        let target = if run.output_published {
            TurnRunState::Uncertain
        } else {
            TurnRunState::Failed
        };
        let error_kind = error.error_kind();
        let error_message = sanitize_error_message(error);
        let turn_id_for_fail = turn_id;
        let result = if self.origin_id.is_empty() {
            call_blocking(self.runtime.db_for(self.scope), move |db| {
                db.fail_turn(&turn_id_for_fail, target, error_kind, &error_message)
            })
            .await
        } else {
            let origin_id = self.origin_id.clone();
            let terminal_reason = StopReason::LlmFailure.to_string();
            call_blocking(self.runtime.db_for(self.scope), move |db| {
                db.fail_turn_and_terminate_origin(
                    &turn_id_for_fail,
                    target,
                    error_kind,
                    &error_message,
                    &origin_id,
                    &terminal_reason,
                )
            })
            .await
        };
        if let Err(record_error) = result {
            warn!(error = %record_error, turn_id = self.turn_id, "failed to record turn_run failure");
        }
    }

    /// Records a failure unless this executor lost a concurrency CAS race.
    pub(crate) async fn record_failure_excluding_conflict(&self, error: &EgoPulseError) {
        if matches!(error, EgoPulseError::TurnConcurrencyConflict) {
            return;
        }
        self.fail(error).await;
    }
}

/// Marks an unrecoverable resume target as failed.
pub(crate) async fn fail_resume_permanently(
    runtime: &TurnDependencies,
    scope: ConversationScope,
    turn_id: &str,
    reason: &str,
) {
    let turn_id_owned = turn_id.to_string();
    let reason = reason.to_string();
    let turn_id_for_db = turn_id_owned.clone();
    let reason_for_db = reason.clone();
    if let Err(error) = call_blocking(runtime.db_for(scope), move |db| {
        db.fail_turn(
            &turn_id_for_db,
            TurnRunState::Failed,
            "validation",
            &reason_for_db,
        )
    })
    .await
    {
        warn!(error = %error, %turn_id_owned, "failed to mark unrecoverable resume turn as failed");
    }
}

/// Validates and decodes a durable `input_committed` resume target.
pub(crate) async fn validate_resume(
    runtime: &TurnDependencies,
    scope: ConversationScope,
    turn_id: &str,
    run: &TurnRun,
    snapshot: &crate::config::manager::ConfigSnapshot,
) -> Result<ScheduledTurn, EgoPulseError> {
    if run.state != TurnRunState::InputCommitted {
        return Err(EgoPulseError::TurnConcurrencyConflict);
    }

    let scheduled_json = match run.scheduled_request_json.clone() {
        Some(json) => json,
        None => {
            fail_resume_permanently(
                runtime,
                scope,
                turn_id,
                "resume target has no scheduled request",
            )
            .await;
            return Err(EgoPulseError::Internal(
                "resume target turn has no scheduled request".to_string(),
            ));
        }
    };
    if run.output_published {
        fail_resume_permanently(
            runtime,
            scope,
            turn_id,
            "resume target already published output",
        )
        .await;
        return Err(EgoPulseError::Internal(
            "resume target turn already published output".to_string(),
        ));
    }

    let persisted = match deserialize_scheduled_turn(&scheduled_json) {
        Ok(persisted) => persisted,
        Err(error) => {
            fail_resume_permanently(
                runtime,
                scope,
                turn_id,
                "failed to decode scheduled request",
            )
            .await;
            return Err(EgoPulseError::Internal(format!(
                "failed to decode scheduled request for resume: {error}"
            )));
        }
    };

    if let Some(fingerprint) = &run.config_fingerprint {
        if !fingerprint.is_empty() && fingerprint != &snapshot.fingerprint {
            fail_resume_permanently(
                runtime,
                scope,
                turn_id,
                "config fingerprint mismatch on resume",
            )
            .await;
            return Err(EgoPulseError::Internal(
                "config fingerprint mismatch on resume".to_string(),
            ));
        }
    }

    let input_message_id = format!("turn:{turn_id}:input");
    let input_exists = call_blocking(runtime.db_for(scope), {
        let id = input_message_id;
        move |db| db.get_message_content(&id)
    })
    .await
    .map_err(EgoPulseError::from)?
    .is_some();
    if !input_exists {
        fail_resume_permanently(
            runtime,
            scope,
            turn_id,
            "resume target input message missing",
        )
        .await;
        return Err(EgoPulseError::Internal(
            "resume target input message is missing".to_string(),
        ));
    }

    Ok(persisted)
}

fn sanitize_error_message(error: &EgoPulseError) -> String {
    truncate_by_chars(&error.user_facing_summary(), 200)
}

#[cfg(test)]
mod tests {
    use super::validate_resume;
    use crate::agent_loop::event::AgentEvent;
    use crate::agent_loop::test_support::{
        RecordingProvider, build_state_with_provider, cli_context,
    };
    use crate::agent_loop::{process_turn, process_turn_with_events, resolve_chat_id};
    use crate::conversation::SurfaceContext;
    use crate::llm::MessagesResponse;
    use crate::runtime::AppState;
    use crate::runtime::turn::{ScheduledTurn, serialize_scheduled_turn};
    use crate::storage::{AcceptOutcome, StoredMessage, TurnRun, TurnRunState, call_blocking};
    use serial_test::serial;
    use std::sync::{Arc, Mutex};

    fn context_with_request_key(session: &str, request_key: &str) -> SurfaceContext {
        let mut context = cli_context(session);
        context.request_key = request_key.to_string();
        context
    }

    fn turn_run_count(state: &AppState, chat_id: i64) -> i64 {
        let conn = state.db.get_conn().expect("conn");
        conn.query_row(
            "SELECT COUNT(*) FROM turn_runs WHERE chat_id = ?1",
            rusqlite::params![chat_id],
            |row| row.get(0),
        )
        .expect("count turn_runs")
    }

    fn user_message_count(state: &AppState, chat_id: i64) -> i64 {
        let conn = state.db.get_conn().expect("conn");
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE chat_id = ?1 AND sender_kind = 'user'",
            rusqlite::params![chat_id],
            |row| row.get(0),
        )
        .expect("count user messages")
    }

    fn resume_payload(context: &SurfaceContext, input: &str) -> String {
        serialize_scheduled_turn(&ScheduledTurn {
            turn_id: String::new(),
            context: context.clone(),
            input: input.to_string(),
            origin_id: context.origin_id.clone(),
            config_snapshot: None,
        })
        .expect("serialize scheduled turn")
    }

    async fn seed_input_committed_turn(
        state: &AppState,
        context: &SurfaceContext,
        scheduled_request_json: Option<&str>,
        config_fingerprint: Option<&str>,
        output_published: bool,
        keep_input: bool,
    ) -> TurnRun {
        let runtime = state.turn_dependencies();
        let chat_id = resolve_chat_id(&runtime, context).await.expect("chat id");
        let snapshot = state.config_manager.current_blocking();
        let config_revision = snapshot.revision as i64;
        let config_fingerprint = config_fingerprint.map(str::to_string);
        let accept_fingerprint = config_fingerprint.clone();
        let request_key = context.request_key.clone();
        let scheduled_request_json = scheduled_request_json.map(str::to_string);
        let accepted = call_blocking(state.db_for(context.scope), move |db| {
            db.accept_or_get_turn(crate::storage::AcceptTurnParams {
                chat_id,
                request_key: &request_key,
                config_revision,
                config_fingerprint: accept_fingerprint.as_deref(),
                request_payload_hash: "resume-test-hash",
                origin_id: None,
                scheduled_request_json: scheduled_request_json.as_deref(),
            })
        })
        .await
        .expect("accept turn");
        let turn_id = match accepted {
            AcceptOutcome::Created(run) => run.turn_id,
            AcceptOutcome::Existing(_) => panic!("resume test must create a new turn"),
        };

        let mut message =
            StoredMessage::user(chat_id, "sender".to_string(), "resume input".to_string());
        message.id = format!("turn:{turn_id}:input");
        message.turn_id = Some(turn_id.clone());
        let fingerprint = config_fingerprint.clone();
        call_blocking(state.db_for(context.scope), {
            let message = message.clone();
            let turn_id = turn_id.clone();
            move |db| {
                db.commit_turn_input_with_conversation(
                    &message,
                    "[]",
                    None,
                    &turn_id,
                    config_revision,
                    fingerprint.as_deref(),
                )
            }
        })
        .await
        .expect("commit turn input");

        if !keep_input {
            let input_id = message.id.clone();
            call_blocking(state.db_for(context.scope), move |db| {
                db.get_conn()?.execute(
                    "DELETE FROM messages WHERE id = ?1",
                    rusqlite::params![input_id],
                )?;
                Ok(())
            })
            .await
            .expect("delete input message");
        }
        if output_published {
            let turn_id = turn_id.clone();
            call_blocking(state.db_for(context.scope), move |db| {
                db.mark_turn_output_published(&turn_id)
            })
            .await
            .expect("mark output published");
        }

        call_blocking(state.db_for(context.scope), move |db| {
            db.get_turn_run(&turn_id)
        })
        .await
        .expect("load seeded turn")
    }

    fn assert_failed(run: &TurnRun) {
        assert_eq!(run.state, TurnRunState::Failed);
        assert_eq!(run.error_kind.as_deref(), Some("validation"));
    }

    #[tokio::test]
    #[serial]
    async fn same_request_key_accepts_one_turn_and_one_user_message() {
        // Arrange: a provider whose first response is final so the turn completes.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "hello back".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        let context = context_with_request_key("dup-accept", "cli:duplicate:1");

        // Act: accept the same request_key twice.
        let first = process_turn(&state.turn_dependencies(), &context, "hi")
            .await
            .expect("first turn");
        let second = process_turn(&state.turn_dependencies(), &context, "hi")
            .await
            .expect("second turn");

        // Assert: the completed result is reused, so the response matches and
        // only one Turn / one user message exists.
        assert_eq!(first, "hello back");
        assert_eq!(second, "hello back");
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:dup-accept:agent:default",
                Some("dup-accept"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        assert_eq!(turn_run_count(&state, chat_id), 1, "exactly one turn_run");
        assert_eq!(
            user_message_count(&state, chat_id),
            1,
            "exactly one user message"
        );
        assert_eq!(
            provider.seen_messages().len(),
            1,
            "completed turn re-acceptance must not call the LLM"
        );
    }

    #[tokio::test]
    #[serial]
    async fn duplicate_in_progress_request_returns_terminal_event_and_response() {
        // Arrange: seed a non-terminal turn_runs row so the re-accept resolves
        // to InProgress (another executor owns it).
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(vec![], vec![]);
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        let session = "dup-inprogress";
        let request_key = "cli:dup-ip:1";
        let context = context_with_request_key(session, request_key);
        let chat_id = call_blocking(Arc::clone(&state.db), {
            let session = session.to_string();
            move |db| {
                db.resolve_or_create_chat_id(
                    "cli",
                    &format!("cli:{session}:agent:default"),
                    Some(&session),
                    "cli",
                    "default",
                )
            }
        })
        .await
        .expect("chat id");
        let payload_hash = crate::runtime::turn::canonical_request_hash(&context, "hello");
        {
            let conn = state.db.get_conn().expect("conn");
            conn.execute(
                "INSERT INTO turn_runs
                     (turn_id, chat_id, request_key, state, config_revision,
                      config_fingerprint, request_payload_hash, accepted_at, updated_at)
                 VALUES (?1, ?2, ?3, 'model_pending', 1, 'fp', ?4, 't', 't')",
                rusqlite::params![
                    &format!("seed-{session}"),
                    chat_id,
                    request_key,
                    &payload_hash
                ],
            )
            .expect("seed turn_runs");
        }

        // Act: a duplicate request for the in-progress Turn must terminate.
        let collected: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let collector = Arc::clone(&collected);
        let reply =
            process_turn_with_events(&state.turn_dependencies(), &context, "hello", move |ev| {
                collector.lock().expect("collector").push(ev);
            })
            .await
            .expect("turn");

        // Assert: a non-empty terminal message is returned and a FinalResponse
        // event is emitted, and the owning executor's LLM is never invoked.
        assert!(
            !reply.is_empty(),
            "in-progress duplicate must return a terminal message"
        );
        let events = collected.lock().expect("events");
        assert!(
            events.iter().any(|ev| matches!(
                ev,
                AgentEvent::FinalResponse { text } if text == &reply
            )),
            "in-progress duplicate must emit a matching FinalResponse event"
        );
        assert!(
            provider.seen_messages().is_empty(),
            "duplicate request must not invoke the LLM"
        );
    }

    #[tokio::test]
    #[serial]
    async fn terminated_turn_re_acceptance_returns_terminal_event_and_response() {
        // Arrange: seed a terminal (uncertain) turn_runs row so the re-accept
        // resolves to Terminated.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(vec![], vec![]);
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        let session = "dup-terminated";
        let request_key = "cli:dup-term:1";
        let context = context_with_request_key(session, request_key);
        let chat_id = call_blocking(Arc::clone(&state.db), {
            let session = session.to_string();
            move |db| {
                db.resolve_or_create_chat_id(
                    "cli",
                    &format!("cli:{session}:agent:default"),
                    Some(&session),
                    "cli",
                    "default",
                )
            }
        })
        .await
        .expect("chat id");
        let payload_hash = crate::runtime::turn::canonical_request_hash(&context, "hello");
        {
            let conn = state.db.get_conn().expect("conn");
            conn.execute(
                "INSERT INTO turn_runs
                     (turn_id, chat_id, request_key, state, config_revision,
                      config_fingerprint, request_payload_hash, accepted_at, updated_at)
                 VALUES (?1, ?2, ?3, 'uncertain', 1, 'fp', ?4, 't', 't')",
                rusqlite::params![
                    &format!("seed-{session}"),
                    chat_id,
                    request_key,
                    &payload_hash
                ],
            )
            .expect("seed turn_runs");
        }

        // Act
        let collected: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let collector = Arc::clone(&collected);
        let reply =
            process_turn_with_events(&state.turn_dependencies(), &context, "hello", move |ev| {
                collector.lock().expect("collector").push(ev);
            })
            .await
            .expect("turn");

        // Assert: a non-empty terminal message is returned and a FinalResponse
        // event is emitted, with no LLM call.
        assert!(
            !reply.is_empty(),
            "terminated re-acceptance must return a terminal message"
        );
        let events = collected.lock().expect("events");
        assert!(
            events.iter().any(|ev| matches!(
                ev,
                AgentEvent::FinalResponse { text } if text == &reply
            )),
            "terminated re-acceptance must emit a matching FinalResponse event"
        );
        assert!(provider.seen_messages().is_empty());
    }

    #[tokio::test]
    #[serial]
    async fn validate_resume_rejects_missing_payload_and_marks_turn_failed() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(RecordingProvider::new(Vec::new(), Vec::new())),
        );
        let context = context_with_request_key("resume-missing-payload", "resume:missing-payload");
        let run = seed_input_committed_turn(
            &state,
            &context,
            None,
            Some(&state.config_manager.current_blocking().fingerprint),
            false,
            true,
        )
        .await;

        // Act
        let error = validate_resume(
            &state.turn_dependencies(),
            context.scope,
            &run.turn_id,
            &run,
            &state.config_manager.current_blocking(),
        )
        .await
        .expect_err("missing payload must be rejected");

        // Assert
        assert!(error.to_string().contains("no scheduled request"));
        assert_failed(
            &state
                .db
                .get_turn_run(&run.turn_id)
                .expect("failed resume turn"),
        );
    }

    #[tokio::test]
    #[serial]
    async fn validate_resume_rejects_published_output_and_marks_turn_failed() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(RecordingProvider::new(Vec::new(), Vec::new())),
        );
        let context = context_with_request_key("resume-published", "resume:published");
        let snapshot = state.config_manager.current_blocking();
        let payload = resume_payload(&context, "resume input");
        let run = seed_input_committed_turn(
            &state,
            &context,
            Some(&payload),
            Some(&snapshot.fingerprint),
            true,
            true,
        )
        .await;

        // Act
        let error = validate_resume(
            &state.turn_dependencies(),
            context.scope,
            &run.turn_id,
            &run,
            &snapshot,
        )
        .await
        .expect_err("published output must be rejected");

        // Assert
        assert!(error.to_string().contains("already published output"));
        assert_failed(
            &state
                .db
                .get_turn_run(&run.turn_id)
                .expect("failed resume turn"),
        );
    }

    #[tokio::test]
    #[serial]
    async fn validate_resume_rejects_invalid_payload_and_marks_turn_failed() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(RecordingProvider::new(Vec::new(), Vec::new())),
        );
        let context = context_with_request_key("resume-invalid-payload", "resume:invalid-payload");
        let snapshot = state.config_manager.current_blocking();
        let run = seed_input_committed_turn(
            &state,
            &context,
            Some("not-json"),
            Some(&snapshot.fingerprint),
            false,
            true,
        )
        .await;

        // Act
        let error = validate_resume(
            &state.turn_dependencies(),
            context.scope,
            &run.turn_id,
            &run,
            &snapshot,
        )
        .await
        .expect_err("invalid payload must be rejected");

        // Assert
        assert!(
            error
                .to_string()
                .contains("failed to decode scheduled request")
        );
        assert_failed(
            &state
                .db
                .get_turn_run(&run.turn_id)
                .expect("failed resume turn"),
        );
    }

    #[tokio::test]
    #[serial]
    async fn validate_resume_rejects_fingerprint_mismatch_and_marks_turn_failed() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(RecordingProvider::new(Vec::new(), Vec::new())),
        );
        let context = context_with_request_key("resume-fingerprint", "resume:fingerprint");
        let snapshot = state.config_manager.current_blocking();
        let payload = resume_payload(&context, "resume input");
        let run = seed_input_committed_turn(
            &state,
            &context,
            Some(&payload),
            Some("stale-fingerprint"),
            false,
            true,
        )
        .await;

        // Act
        let error = validate_resume(
            &state.turn_dependencies(),
            context.scope,
            &run.turn_id,
            &run,
            &snapshot,
        )
        .await
        .expect_err("fingerprint mismatch must be rejected");

        // Assert
        assert!(error.to_string().contains("config fingerprint mismatch"));
        assert_failed(
            &state
                .db
                .get_turn_run(&run.turn_id)
                .expect("failed resume turn"),
        );
    }

    #[tokio::test]
    #[serial]
    async fn validate_resume_rejects_missing_input_and_marks_turn_failed() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(RecordingProvider::new(Vec::new(), Vec::new())),
        );
        let context = context_with_request_key("resume-missing-input", "resume:missing-input");
        let snapshot = state.config_manager.current_blocking();
        let payload = resume_payload(&context, "resume input");
        let run = seed_input_committed_turn(
            &state,
            &context,
            Some(&payload),
            Some(&snapshot.fingerprint),
            false,
            false,
        )
        .await;

        // Act
        let error = validate_resume(
            &state.turn_dependencies(),
            context.scope,
            &run.turn_id,
            &run,
            &snapshot,
        )
        .await
        .expect_err("missing input must be rejected");

        // Assert
        assert!(error.to_string().contains("input message is missing"));
        assert_failed(
            &state
                .db
                .get_turn_run(&run.turn_id)
                .expect("failed resume turn"),
        );
    }

    #[tokio::test]
    #[serial]
    async fn validate_resume_accepts_valid_input_committed_turn() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(RecordingProvider::new(Vec::new(), Vec::new())),
        );
        let context = context_with_request_key("resume-valid", "resume:valid");
        let snapshot = state.config_manager.current_blocking();
        let payload = resume_payload(&context, "resume input");
        let run = seed_input_committed_turn(
            &state,
            &context,
            Some(&payload),
            Some(&snapshot.fingerprint),
            false,
            true,
        )
        .await;

        // Act
        let persisted = validate_resume(
            &state.turn_dependencies(),
            context.scope,
            &run.turn_id,
            &run,
            &snapshot,
        )
        .await
        .expect("valid resume target");

        // Assert
        assert_eq!(persisted.input, "resume input");
        assert_eq!(persisted.context.session_key(), context.session_key());
        assert_eq!(
            state.db.get_turn_run(&run.turn_id).expect("turn run").state,
            TurnRunState::InputCommitted
        );
    }
}

//! Durable Turn acceptance, state transitions, resume validation, and failure.

use crate::agent_loop::TurnRuntime;
use crate::channels::utils::text::truncate_by_chars;
use crate::conversation::ConversationScope;
use crate::error::EgoPulseError;
use crate::runtime::turn_scheduler::StopReason;
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

/// Owns durable state transitions for one Turn.
pub(crate) struct TurnLifecycle<'a> {
    runtime: &'a TurnRuntime,
    scope: ConversationScope,
    turn_id: String,
    origin_id: String,
}

impl<'a> TurnLifecycle<'a> {
    /// Creates a lifecycle boundary for one durable Turn.
    pub(crate) fn new(
        runtime: &'a TurnRuntime,
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
        runtime: &TurnRuntime,
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

    /// Marks model output as externally published.
    pub(crate) async fn mark_output_published(&self) {
        let turn_id = self.turn_id.to_string();
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
        let turn_id = self.turn_id.to_string();
        call_blocking(self.runtime.db_for(self.scope), move |db| {
            db.complete_turn_model(&turn_id)
        })
        .await?;
        Ok(())
    }

    /// Marks the tool phase as started.
    pub(crate) async fn begin_tools(&self) -> Result<(), EgoPulseError> {
        let turn_id = self.turn_id.to_string();
        call_blocking(self.runtime.db_for(self.scope), move |db| {
            db.begin_turn_tools(&turn_id)
        })
        .await?;
        Ok(())
    }

    /// Marks the tool phase as complete.
    pub(crate) async fn complete_tools(&self) -> Result<(), EgoPulseError> {
        let turn_id = self.turn_id.to_string();
        call_blocking(self.runtime.db_for(self.scope), move |db| {
            db.complete_turn_tools(&turn_id)
        })
        .await?;
        Ok(())
    }

    /// Completes the Turn with its persisted final message.
    pub(crate) async fn complete(&self, final_message_id: &str) -> Result<(), EgoPulseError> {
        let turn_id = self.turn_id.to_string();
        let final_message_id = final_message_id.to_string();
        call_blocking(self.runtime.db_for(self.scope), move |db| {
            db.complete_turn(&turn_id, &final_message_id)
        })
        .await?;
        Ok(())
    }

    /// Records failure or uncertainty according to the durable publication state.
    pub(crate) async fn fail(&self, error: &EgoPulseError) {
        let turn_id = self.turn_id.to_string();
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
            let origin_id = self.origin_id.to_string();
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
    runtime: &TurnRuntime,
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

fn sanitize_error_message(error: &EgoPulseError) -> String {
    truncate_by_chars(&error.user_facing_summary(), 200)
}

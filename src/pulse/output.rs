//! Pulse output handling — routes activation results to the appropriate destination.
//!
//! After a Pulse Activation completes, this module decides what happens:
//! - **PULSE_OK** (silent): updates the pulse run, does not send or persist anything.
//! - **Notification**: persists a synthetic conversation turn to the normal session
//!   and updates the pulse run. The actual channel send is deferred to the scheduler
//!   (Step 8) which has access to channel adapters.

use tracing::warn;

use crate::error::EgoPulseError;
use crate::pulse::home_surface::HomeSurface;
use crate::pulse::runner::ActivationResult;
use crate::storage::{Database, MessageKind, PulseOutputKind, StoredMessage};

/// Result of output handling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OutputResult {
    /// Whether a notification was sent.
    pub notified: bool,
    /// The chat_id where the notification was sent (if any).
    pub chat_id: Option<i64>,
    /// The message_id of the sent notification (if any).
    pub message_id: Option<String>,
    /// The output text.
    pub output_text: String,
    /// The output kind (silent or notify).
    pub output_kind: PulseOutputKind,
}

/// Handle the output of a Pulse Activation.
///
/// - **PULSE_OK**: updates the pulse run as silent success. No session persistence.
/// - **Notification**: persists a synthetic conversation turn to the normal session
///   and updates the pulse run with the message reference.
///
/// The actual channel send (Discord/Telegram) is NOT performed here — that is
/// the scheduler's responsibility (Step 8), which has access to channel adapters.
///
/// # Errors
/// Returns `EgoPulseError` when database persistence or pulse run updates fail.
pub(crate) async fn handle_output(
    db: &std::sync::Arc<Database>,
    agent_id: &str,
    intention_id: &str,
    home_surface: &HomeSurface,
    activation_result: &ActivationResult,
    pulse_run_id: &str,
) -> Result<OutputResult, EgoPulseError> {
    match activation_result.output_kind {
        PulseOutputKind::Silent => {
            handle_silent(db, &activation_result.output_text, pulse_run_id).await
        }
        PulseOutputKind::Notify => {
            handle_notify(
                db,
                agent_id,
                intention_id,
                home_surface,
                &activation_result.output_text,
                pulse_run_id,
            )
            .await
        }
    }
}

/// Silent path: update pulse run, return without persisting anything.
async fn handle_silent(
    db: &std::sync::Arc<Database>,
    output_text: &str,
    pulse_run_id: &str,
) -> Result<OutputResult, EgoPulseError> {
    let output_text_owned = output_text.to_string();
    let pulse_run_id_owned = pulse_run_id.to_string();
    crate::storage::call_blocking(db.clone(), move |db| {
        db.update_pulse_run_success(
            &pulse_run_id_owned,
            None,
            None,
            PulseOutputKind::Silent,
            &output_text_owned,
        )
    })
    .await?;

    Ok(OutputResult {
        notified: false,
        chat_id: None,
        message_id: None,
        output_text: output_text.to_string(),
        output_kind: PulseOutputKind::Silent,
    })
}

/// Notification path: persist synthetic turn to the normal session, update pulse run.
async fn handle_notify(
    db: &std::sync::Arc<Database>,
    agent_id: &str,
    intention_id: &str,
    home_surface: &HomeSurface,
    output_text: &str,
    pulse_run_id: &str,
) -> Result<OutputResult, EgoPulseError> {
    let chat_id = home_surface.chat_id;
    let agent_id_owned = agent_id.to_string();
    let intention_id_owned = intention_id.to_string();
    let output_text_owned = output_text.to_string();
    let pulse_run_id_owned = pulse_run_id.to_string();

    let result = crate::storage::call_blocking(db.clone(), move |db| {
        persist_notification(
            db,
            &agent_id_owned,
            &intention_id_owned,
            chat_id,
            &output_text_owned,
        )
    })
    .await;

    match result {
        Ok(message_id) => {
            let msg_id_for_update = message_id.clone();
            let output_for_update = output_text.to_string();
            crate::storage::call_blocking(db.clone(), move |db| {
                db.update_pulse_run_success(
                    &pulse_run_id_owned,
                    Some(chat_id),
                    Some(&msg_id_for_update),
                    PulseOutputKind::Notify,
                    &output_for_update,
                )
            })
            .await?;

            Ok(OutputResult {
                notified: true,
                chat_id: Some(chat_id),
                message_id: Some(message_id),
                output_text: output_text.to_string(),
                output_kind: PulseOutputKind::Notify,
            })
        }
        Err(e) => {
            warn!(
                error = %e,
                agent_id,
                intention_id,
                "pulse notification persistence failed"
            );

            let error_msg = e.to_string();
            crate::storage::call_blocking(db.clone(), move |db| {
                db.update_pulse_run_failed(&pulse_run_id_owned, &error_msg)
            })
            .await
            .ok();

            Err(EgoPulseError::Storage(e))
        }
    }
}

/// Persist a notification as a synthetic conversation turn to the normal session.
///
/// Creates two messages:
/// 1. Synthetic user input: `[Pulse: {intention_id}]`
/// 2. Assistant message with the notification text
///
/// Returns the message ID of the assistant message (used as reference in pulse_run).
fn persist_notification(
    db: &Database,
    agent_id: &str,
    intention_id: &str,
    chat_id: i64,
    output_text: &str,
) -> Result<String, crate::error::StorageError> {
    let now = chrono::Utc::now().to_rfc3339();

    let synthetic_input = StoredMessage {
        id: format!("pulse-in-{}", uuid::Uuid::new_v4()),
        chat_id,
        sender_name: "Pulse".to_string(),
        content: format!("[Pulse: {intention_id}]"),
        is_from_bot: false,
        timestamp: now.clone(),
        message_kind: MessageKind::SystemEvent,
        sender_agent_id: None,
        recipient_agent_id: None,
    };

    let assistant_id = format!("pulse-out-{}", uuid::Uuid::new_v4());
    let assistant_msg = StoredMessage {
        id: assistant_id.clone(),
        chat_id,
        sender_name: agent_id.to_string(),
        content: output_text.to_string(),
        is_from_bot: true,
        timestamp: now,
        message_kind: MessageKind::Message,
        sender_agent_id: Some(agent_id.to_string()),
        recipient_agent_id: None,
    };

    db.store_message_only(&synthetic_input)?;
    db.store_message_only(&assistant_msg)?;

    Ok(assistant_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (Arc::new(db), dir)
    }

    fn test_home_surface(chat_id: i64) -> HomeSurface {
        HomeSurface {
            chat_id,
            channel: "discord".to_string(),
            external_chat_id: "discord:123".to_string(),
        }
    }

    fn create_pulse_run(db: &Database, id: &str, agent_id: &str, intention_id: &str) {
        db.try_create_pulse_run(id, agent_id, intention_id, "2026-05-11T09:00")
            .expect("create pulse run");
    }

    fn insert_chat(db: &Database, agent_id: &str) -> i64 {
        db.resolve_or_create_chat_id("discord", "discord:123", None, "dm", agent_id)
            .expect("create chat")
    }

    #[tokio::test]
    async fn pulse_ok_sends_nothing_and_persists_no_session() {
        // Arrange
        let (db, _dir) = test_db();
        let agent_id = "lyre";
        let intention_id = "morning_review";
        let pulse_run_id = "run-001";

        let chat_id = insert_chat(&db, agent_id);
        create_pulse_run(&db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: "PULSE_OK".to_string(),
            output_kind: PulseOutputKind::Silent,
        };

        // Act
        let result = handle_output(
            &db,
            agent_id,
            intention_id,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert
        assert!(!result.notified);
        assert!(result.chat_id.is_none());
        assert!(result.message_id.is_none());
        assert_eq!(result.output_text, "PULSE_OK");
        assert_eq!(result.output_kind, PulseOutputKind::Silent);

        let run = db
            .get_pulse_run(pulse_run_id)
            .expect("get run")
            .expect("exists");
        assert_eq!(run.status, crate::storage::PulseRunStatus::Success);
        assert_eq!(run.output_kind, Some(PulseOutputKind::Silent));
        assert!(run.chat_id.is_none());
        assert!(run.message_id.is_none());

        let messages = db.get_all_messages(chat_id).expect("messages");
        assert!(
            messages.is_empty(),
            "PULSE_OK should not persist any messages"
        );
    }

    #[tokio::test]
    async fn notify_sends_text_to_home_surface() {
        // Arrange
        let (db, _dir) = test_db();
        let agent_id = "lyre";
        let intention_id = "evening_digest";
        let pulse_run_id = "run-002";
        let notification_text = "You have 3 unread messages from today.";

        let chat_id = insert_chat(&db, agent_id);
        create_pulse_run(&db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        // Act
        let result = handle_output(
            &db,
            agent_id,
            intention_id,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert
        assert!(result.notified);
        assert_eq!(result.chat_id, Some(chat_id));
        assert!(result.message_id.is_some());
        assert_eq!(result.output_text, notification_text);
        assert_eq!(result.output_kind, PulseOutputKind::Notify);
    }

    #[tokio::test]
    async fn notify_persists_synthetic_input_and_turn_like_normal_conversation() {
        // Arrange
        let (db, _dir) = test_db();
        let agent_id = "lyre";
        let intention_id = "morning_review";
        let pulse_run_id = "run-003";
        let notification_text = "Good morning! You have 2 tasks today.";

        let chat_id = insert_chat(&db, agent_id);
        create_pulse_run(&db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        // Act
        handle_output(
            &db,
            agent_id,
            intention_id,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert
        let messages = db.get_all_messages(chat_id).expect("messages");
        assert_eq!(messages.len(), 2);

        let synthetic = &messages[0];
        assert_eq!(synthetic.content, "[Pulse: morning_review]");
        assert!(!synthetic.is_from_bot);
        assert_eq!(synthetic.message_kind, MessageKind::SystemEvent);
        assert_eq!(synthetic.sender_name, "Pulse");

        let assistant = &messages[1];
        assert_eq!(assistant.content, notification_text);
        assert!(assistant.is_from_bot);
        assert_eq!(assistant.message_kind, MessageKind::Message);
        assert_eq!(assistant.sender_name, agent_id);
    }

    #[tokio::test]
    async fn notify_does_not_store_pulse_capsule_body() {
        // Arrange
        let (db, _dir) = test_db();
        let agent_id = "lyre";
        let intention_id = "check_in";
        let pulse_run_id = "run-004";
        let capsule_prompt_text = "# Pulse Activation\n## Core Contract\nYou are an agent.";
        let notification_text = "All quiet. Nothing to report.";

        let chat_id = insert_chat(&db, agent_id);
        create_pulse_run(&db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        // Act
        handle_output(
            &db,
            agent_id,
            intention_id,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert
        let messages = db.get_all_messages(chat_id).expect("messages");
        for msg in &messages {
            assert!(
                !msg.content.contains("# Pulse Activation"),
                "capsule prompt body should not be persisted: found in message {}",
                msg.id
            );
            assert!(
                !msg.content.contains("## Core Contract"),
                "capsule prompt body should not be persisted: found in message {}",
                msg.id
            );
            assert!(
                !msg.content.contains(capsule_prompt_text),
                "capsule prompt body should not be persisted: found in message {}",
                msg.id
            );
        }
    }

    #[tokio::test]
    async fn notify_updates_pulse_run_with_message_id() {
        // Arrange
        let (db, _dir) = test_db();
        let agent_id = "lyre";
        let intention_id = "weekly_report";
        let pulse_run_id = "run-005";
        let notification_text = "Weekly summary: 42 conversations processed.";

        let chat_id = insert_chat(&db, agent_id);
        create_pulse_run(&db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        // Act
        let result = handle_output(
            &db,
            agent_id,
            intention_id,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert
        let run = db
            .get_pulse_run(pulse_run_id)
            .expect("get run")
            .expect("exists");
        assert_eq!(run.status, crate::storage::PulseRunStatus::Success);
        assert_eq!(run.chat_id, Some(chat_id));
        assert!(run.message_id.is_some());
        assert_eq!(run.message_id.as_deref(), result.message_id.as_deref());
        assert_eq!(run.output_kind, Some(PulseOutputKind::Notify));
        assert_eq!(run.output_text.as_deref(), Some(notification_text));
    }

    #[tokio::test]
    async fn notify_marks_failed_when_send_fails() {
        // Arrange: drop the messages table to force a DB error during persist_notification
        let (db, _dir) = test_db();
        let agent_id = "lyre";
        let intention_id = "broken_intention";
        let pulse_run_id = "run-006";

        let chat_id = insert_chat(&db, agent_id);
        create_pulse_run(&db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: "This should fail.".to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        {
            let conn = db.conn.lock().expect("lock");
            conn.execute("DROP TABLE messages", rusqlite::params![])
                .expect("drop messages");
        }

        // Act
        let result = handle_output(
            &db,
            agent_id,
            intention_id,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await;

        // Assert
        assert!(
            result.is_err(),
            "handle_output should fail when persistence fails"
        );
    }
}

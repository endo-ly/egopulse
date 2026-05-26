//! Pulse output handling — routes activation results to the appropriate destination.
//!
//! After a Pulse Activation completes, this module decides what happens:
//! - **PULSE_OK** (silent): updates the pulse run, does not send or persist anything.
//! - **Notification**: persists a synthetic conversation turn to the normal session
//!   (updating both messages and session snapshot), sends to the channel adapter,
//!   and updates the pulse run.

use std::sync::Arc;

use tracing::warn;

use crate::agent_loop::formatting::preview_text;
use crate::agent_loop::session::{
    PersistedTurn, persist_phase, persist_phase_messages, persist_phase_once,
};
use crate::error::EgoPulseError;
use crate::llm::Message;
use crate::pulse::capsule::HomeSurface;
use crate::pulse::definition::{TemporalIntention, format_schedule};
use crate::pulse::runner::ActivationResult;
use crate::runtime::AppState;
use crate::storage::{MessageKind, PulseOutputKind, SenderKind, StoredMessage};

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
///   (updating both messages table AND session snapshot), sends via channel adapter,
///   and updates the pulse run.
///
/// # Errors
/// Returns `EgoPulseError` when database persistence, channel delivery, or pulse run updates fail.
pub(crate) async fn handle_output(
    state: &AppState,
    agent_id: &str,
    intention: &TemporalIntention,
    home_surface: &HomeSurface,
    activation_result: &ActivationResult,
    pulse_run_id: &str,
) -> Result<OutputResult, EgoPulseError> {
    match activation_result.output_kind {
        PulseOutputKind::Silent => {
            handle_silent(&state.db, &activation_result.output_text, pulse_run_id).await
        }
        PulseOutputKind::Notify => {
            handle_notify(
                state,
                agent_id,
                intention,
                home_surface,
                activation_result,
                pulse_run_id,
            )
            .await
        }
    }
}

/// Silent path: update pulse run, return without persisting anything.
async fn handle_silent(
    db: &Arc<crate::storage::Database>,
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

/// Notification path: send to channel, persist synthetic turn with session snapshot, update pulse run.
async fn handle_notify(
    state: &AppState,
    agent_id: &str,
    intention: &TemporalIntention,
    home_surface: &HomeSurface,
    activation_result: &ActivationResult,
    pulse_run_id: &str,
) -> Result<OutputResult, EgoPulseError> {
    let chat_id = home_surface.chat_id;
    let output_text = &activation_result.output_text;

    let adapter = match state.channels.get(&home_surface.channel) {
        Some(a) => a,
        None => {
            warn!(
                channel = %home_surface.channel,
                "pulse channel adapter not found, marking run as failed"
            );
            let db = Arc::clone(&state.db);
            let run_id = pulse_run_id.to_string();
            let error_msg = format!("channel adapter not found: {}", home_surface.channel);
            let error_for_return = error_msg.clone();
            crate::storage::call_blocking(db, move |db| {
                db.update_pulse_run_failed(&run_id, &error_msg)
            })
            .await
            .ok();
            return Err(EgoPulseError::Internal(error_for_return));
        }
    };

    // Send first — if delivery fails, nothing is persisted to the session.
    if let Err(e) = adapter
        .send_text(&home_surface.external_chat_id, output_text)
        .await
    {
        warn!(
            error = %e,
            channel = %home_surface.channel,
            "pulse send failed"
        );
        let db = Arc::clone(&state.db);
        let run_id = pulse_run_id.to_string();
        let error_msg = format!("channel send failed: {e}");
        crate::storage::call_blocking(db, move |db| {
            db.update_pulse_run_failed(&run_id, &error_msg)
        })
        .await
        .ok();
        return Err(EgoPulseError::Internal(format!(
            "pulse channel send failed: {e}"
        )));
    }

    // Delivery succeeded — persist the notification to session.
    let message_id = match persist_notification_with_session(
        state,
        agent_id,
        intention,
        chat_id,
        activation_result,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            warn!(
                error = %e,
                agent_id,
                intention_id = %intention.id,
                "pulse notification persistence failed (message was delivered)"
            );
            let db = Arc::clone(&state.db);
            let run_id = pulse_run_id.to_string();
            let error_msg = e.to_string();
            crate::storage::call_blocking(db, move |db| {
                db.update_pulse_run_failed(&run_id, &error_msg)
            })
            .await
            .ok();
            return Err(e);
        }
    };

    let msg_id_for_update = message_id.clone();
    let output_for_update = output_text.to_string();
    let pulse_run_id_owned = pulse_run_id.to_string();
    crate::storage::call_blocking(Arc::clone(&state.db), move |db| {
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

/// Build the synthetic user-visible content for a Pulse intention injection.
///
/// Format:
/// ```text
/// [Pulse: <intention_id>]
/// Schedule: <schedule>
/// Attention:
/// <attention>
/// ```
fn format_synthetic_content(intention: &TemporalIntention) -> String {
    let schedule_text = format_schedule(&intention.schedule);
    format!(
        "[Pulse: {}]\nSchedule: {}\nAttention:\n{}",
        intention.id,
        schedule_text,
        intention.attention.trim()
    )
}

async fn persist_notification_with_session(
    state: &AppState,
    agent_id: &str,
    intention: &TemporalIntention,
    chat_id: i64,
    activation_result: &ActivationResult,
) -> Result<String, EgoPulseError> {
    let now = chrono::Utc::now().to_rfc3339();
    let output_text = &activation_result.output_text;

    let synthetic_content = format_synthetic_content(intention);
    let synthetic_input = StoredMessage {
        id: format!("pulse-in-{}", uuid::Uuid::new_v4()),
        chat_id,
        sender_id: "pulse".to_string(),
        content: synthetic_content.clone(),
        sender_kind: SenderKind::User,
        timestamp: now.clone(),
        message_kind: MessageKind::SystemEvent,
        recipient_agent_id: None,
    };

    let loaded = crate::agent_loop::session::load_messages_for_turn(state, chat_id).await?;

    let mut session_messages = (*loaded.messages).clone();
    session_messages.push(Message::text("user", &synthetic_input.content));

    let PersistedTurn {
        mut updated_at,
        messages: mut session_messages,
    } = persist_phase_once(
        state,
        synthetic_input.clone(),
        &session_messages,
        loaded.session_updated_at,
    )
    .await?;

    for phase in &activation_result.tool_phases {
        session_messages.push(phase.assistant_message.clone());
        let PersistedTurn {
            updated_at: next_updated_at,
            messages: persisted_messages,
        } = persist_phase(
            state,
            StoredMessage {
                id: phase.assistant_message_id.clone(),
                chat_id,
                sender_id: agent_id.to_string(),
                content: phase.assistant_preview.clone(),
                sender_kind: SenderKind::Assistant,
                timestamp: chrono::Utc::now().to_rfc3339(),
                message_kind: MessageKind::Message,
                recipient_agent_id: None,
            },
            phase.assistant_message.clone(),
            &session_messages,
            Some(updated_at),
        )
        .await?;
        updated_at = next_updated_at;
        session_messages = persisted_messages;

        persist_tool_call_records(state, phase.stored_tool_calls.clone()).await?;

        if !phase.tool_messages.is_empty() {
            session_messages.extend(phase.tool_messages.iter().cloned());
            let PersistedTurn {
                updated_at: next_updated_at,
                messages: persisted_messages,
            } = persist_phase_messages(
                state,
                StoredMessage {
                    id: format!("pulse-tools-{}", uuid::Uuid::new_v4()),
                    chat_id,
                    sender_id: agent_id.to_string(),
                    content: preview_text(&phase.tool_result_preview, 160),
                    sender_kind: SenderKind::Assistant,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    message_kind: MessageKind::Message,
                    recipient_agent_id: None,
                },
                phase.tool_messages.clone(),
                &session_messages,
                Some(updated_at),
            )
            .await?;
            updated_at = next_updated_at;
            session_messages = persisted_messages;
        }
    }

    let assistant_id = format!("pulse-out-{}", uuid::Uuid::new_v4());
    let assistant_msg = StoredMessage {
        id: assistant_id.clone(),
        chat_id,
        sender_id: agent_id.to_string(),
        content: output_text.to_string(),
        sender_kind: SenderKind::Assistant,
        timestamp: now,
        message_kind: MessageKind::Message,
        recipient_agent_id: None,
    };

    session_messages.push(Message::text("assistant", output_text));

    persist_phase_once(state, assistant_msg, &session_messages, Some(updated_at)).await?;

    Ok(assistant_id)
}

async fn persist_tool_call_records(
    state: &AppState,
    tool_calls: Vec<crate::storage::ToolCall>,
) -> Result<(), EgoPulseError> {
    for record in tool_calls {
        crate::storage::call_blocking(Arc::clone(&state.db), move |db| db.store_tool_call(&record))
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::adapter::{ChannelAdapter, ChannelRegistry, ConversationKind};
    use crate::pulse::runner::ToolPhase;
    use crate::storage::{Database, SenderKind};
    use std::sync::Arc;

    #[test]
    fn format_synthetic_content_daily() {
        let intention = TemporalIntention {
            id: "morning_review".to_string(),
            enabled: true,
            schedule: crate::pulse::definition::TemporalSchedule::Daily {
                at: "08:00".to_string(),
            },
            attention: "Check today's schedule.\n".to_string(),
        };
        let content = format_synthetic_content(&intention);
        assert_eq!(
            content,
            "[Pulse: morning_review]\nSchedule: daily 08:00\nAttention:\nCheck today's schedule."
        );
    }

    #[test]
    fn format_synthetic_content_weekly() {
        let intention = TemporalIntention {
            id: "weekly_reflection".to_string(),
            enabled: true,
            schedule: crate::pulse::definition::TemporalSchedule::Weekly {
                day: "sun".to_string(),
                at: "21:00".to_string(),
            },
            attention: "Reflect on the week.".to_string(),
        };
        let content = format_synthetic_content(&intention);
        assert!(content.starts_with("[Pulse: weekly_reflection]"));
        assert!(content.contains("Schedule: weekly sun 21:00"));
        assert!(content.contains("Attention:\nReflect on the week."));
    }

    #[test]
    fn format_synthetic_content_trims_attention_whitespace() {
        let intention = TemporalIntention {
            id: "test".to_string(),
            enabled: true,
            schedule: crate::pulse::definition::TemporalSchedule::Daily {
                at: "09:00".to_string(),
            },
            attention: "  hello world  \n\n".to_string(),
        };
        let content = format_synthetic_content(&intention);
        assert!(content.contains("Attention:\nhello world"));
    }

    /// A no-op channel adapter for testing that records nothing but succeeds.
    struct MockChannelAdapter;

    #[async_trait::async_trait]
    impl ChannelAdapter for MockChannelAdapter {
        fn name(&self) -> &str {
            "discord"
        }
        fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
            vec![("discord", ConversationKind::Private)]
        }
        async fn send_text(&self, _external_chat_id: &str, _text: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn build_test_state(dir: &tempfile::TempDir) -> AppState {
        let mut channels = ChannelRegistry::new();
        channels.register(Arc::new(MockChannelAdapter));
        build_test_state_with_channels(dir, Arc::new(channels))
    }

    fn build_test_state_with_channels(
        dir: &tempfile::TempDir,
        channels: Arc<ChannelRegistry>,
    ) -> AppState {
        let state_root = dir.path().to_str().expect("utf8").to_string();
        let config = crate::test_util::test_config(&state_root);
        let db = Arc::new(Database::new(&config.db_path()).expect("db"));
        crate::test_util::build_state_with_config(config, None, None, Some(db), Some(channels))
    }

    fn test_intention(id: &str) -> TemporalIntention {
        TemporalIntention {
            id: id.to_string(),
            enabled: true,
            schedule: crate::pulse::definition::TemporalSchedule::Daily {
                at: "09:00".to_string(),
            },
            attention: "Check today's schedule and unresolved items.".to_string(),
        }
    }

    fn test_home_surface(chat_id: i64) -> HomeSurface {
        HomeSurface {
            chat_id,
            channel: "discord".to_string(),
            external_chat_id: "discord:123".to_string(),
            chat_type: "dm".to_string(),
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
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);
        let agent_id = "lyre";
        let intention = test_intention("morning_review");
        let pulse_run_id = "run-001";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, &intention.id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: "PULSE_OK".to_string(),
            output_kind: PulseOutputKind::Silent,
            tool_phases: Vec::new(),
        };

        // Act
        let result = handle_output(
            &state,
            agent_id,
            &intention,
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

        let run = state
            .db
            .get_pulse_run(pulse_run_id)
            .expect("get run")
            .expect("exists");
        assert_eq!(run.status, crate::storage::PulseRunStatus::Success);
        assert_eq!(run.output_kind, Some(PulseOutputKind::Silent));
        assert!(run.chat_id.is_none());
        assert!(run.message_id.is_none());

        let messages = state.db.get_all_messages(chat_id).expect("messages");
        assert!(
            messages.is_empty(),
            "PULSE_OK should not persist any messages"
        );
    }

    #[tokio::test]
    async fn notify_persists_synthetic_input_and_turn_like_normal_conversation() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);
        let agent_id = "lyre";
        let intention = test_intention("morning_review");
        let pulse_run_id = "run-003";
        let notification_text = "Good morning! You have 2 tasks today.";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, &intention.id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
            tool_phases: Vec::new(),
        };

        // Act
        handle_output(
            &state,
            agent_id,
            &intention,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert
        let messages = state.db.get_all_messages(chat_id).expect("messages");
        assert_eq!(messages.len(), 2);

        let synthetic = &messages[0];
        assert!(synthetic.content.starts_with("[Pulse: morning_review]"));
        assert!(synthetic.content.contains("Schedule: daily 09:00"));
        assert!(synthetic.content.contains("Attention:"));
        assert!(
            synthetic
                .content
                .contains("Check today's schedule and unresolved items.")
        );
        assert_eq!(synthetic.sender_kind, SenderKind::User);
        assert_eq!(synthetic.message_kind, MessageKind::SystemEvent);
        assert_eq!(synthetic.sender_id, "pulse");

        let assistant = &messages[1];
        assert_eq!(assistant.content, notification_text);
        assert_eq!(assistant.sender_kind, SenderKind::Assistant);
        assert_eq!(assistant.message_kind, MessageKind::Message);
        assert_eq!(assistant.sender_id, agent_id);
    }

    #[tokio::test]
    async fn notify_updates_session_snapshot() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);
        let agent_id = "lyre";
        let intention = test_intention("snapshot_test");
        let pulse_run_id = "run-snapshot-001";
        let notification_text = "Session snapshot should be updated.";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, &intention.id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
            tool_phases: Vec::new(),
        };

        // Act
        handle_output(
            &state,
            agent_id,
            &intention,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert: session snapshot should exist and contain the messages
        let snapshot = state
            .db
            .load_session_snapshot(chat_id, 10)
            .expect("load snapshot");
        let session_json = snapshot
            .messages_json
            .as_deref()
            .expect("session json should exist");
        assert!(
            session_json.contains("[Pulse: snapshot_test]"),
            "session snapshot should contain synthetic input"
        );
        assert!(
            session_json.contains(notification_text),
            "session snapshot should contain assistant response"
        );
    }

    #[tokio::test]
    async fn notify_persists_tool_phases_like_normal_conversation() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);
        let agent_id = "lyre";
        let intention = test_intention("tool_phase_test");
        let pulse_run_id = "run-tool-phase-001";
        let notification_text = "I checked the file.";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, &intention.id);

        let home_surface = test_home_surface(chat_id);
        let assistant_message = Message {
            role: "assistant".to_string(),
            content: crate::llm::MessageContent::text("I'll inspect it."),
            reasoning_content: None,
            tool_calls: vec![crate::llm::ToolCall {
                id: "call-read".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "notes.md"}),
            }],
            tool_call_id: None,
        };
        let tool_message = Message {
            role: "tool".to_string(),
            content: crate::llm::MessageContent::text("{\"status\":\"success\",\"result\":\"ok\"}"),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some("call-read".to_string()),
        };
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
            tool_phases: vec![ToolPhase {
                assistant_message_id: "assistant-tool-1".to_string(),
                assistant_message,
                assistant_preview: "I'll inspect it. [tool_call] read".to_string(),
                tool_messages: vec![tool_message],
                tool_result_preview: "ok".to_string(),
                stored_tool_calls: vec![crate::storage::ToolCall {
                    id: "call-read".to_string(),
                    chat_id,
                    message_id: "assistant-tool-1".to_string(),
                    tool_name: "read".to_string(),
                    tool_input: "{\"path\":\"notes.md\"}".to_string(),
                    tool_output: Some("{\"status\":\"success\",\"result\":\"ok\"}".to_string()),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                }],
            }],
        };

        // Act
        handle_output(
            &state,
            agent_id,
            &intention,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert
        let messages = state.db.get_all_messages(chat_id).expect("messages");
        assert_eq!(messages.len(), 4);
        assert!(
            messages
                .iter()
                .any(|message| message.content.starts_with("[Pulse: tool_phase_test]"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.content.contains("[tool_call] read"))
        );
        assert!(messages.iter().any(|message| message.content == "ok"));
        assert!(
            messages
                .iter()
                .any(|message| message.content == notification_text)
        );

        let snapshot = state
            .db
            .load_session_snapshot(chat_id, 10)
            .expect("load snapshot");
        let session_json = snapshot.messages_json.expect("session json");
        assert!(session_json.contains("call-read"));
        assert!(session_json.contains(notification_text));

        let tool_calls = state
            .db
            .get_tool_calls_for_chat(chat_id)
            .expect("tool calls");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].message_id, "assistant-tool-1");
        assert!(tool_calls[0].tool_output.is_some());
    }

    #[tokio::test]
    async fn notify_does_not_store_pulse_capsule_body() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);
        let agent_id = "lyre";
        let intention = test_intention("check_in");
        let pulse_run_id = "run-004";
        let capsule_prompt_text = "# Pulse Activation\n## Core Contract\nYou are an agent.";
        let notification_text = "All quiet. Nothing to report.";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, &intention.id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
            tool_phases: Vec::new(),
        };

        // Act
        handle_output(
            &state,
            agent_id,
            &intention,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert
        let messages = state.db.get_all_messages(chat_id).expect("messages");
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
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);
        let agent_id = "lyre";
        let intention = test_intention("weekly_report");
        let pulse_run_id = "run-005";
        let notification_text = "Weekly summary: 42 conversations processed.";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, &intention.id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
            tool_phases: Vec::new(),
        };

        // Act
        let result = handle_output(
            &state,
            agent_id,
            &intention,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await
        .expect("handle_output");

        // Assert
        let run = state
            .db
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
    async fn notify_marks_failed_when_persist_fails() {
        // Arrange: drop the messages table to force a DB error
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);
        let agent_id = "lyre";
        let intention = test_intention("broken_intention");
        let pulse_run_id = "run-006";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, &intention.id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: "This should fail.".to_string(),
            output_kind: PulseOutputKind::Notify,
            tool_phases: Vec::new(),
        };

        {
            let conn = state.db.get_conn().expect("pool");
            conn.execute("DROP TABLE messages", rusqlite::params![])
                .expect("drop messages");
        }

        // Act
        let result = handle_output(
            &state,
            agent_id,
            &intention,
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

    #[tokio::test]
    async fn notify_missing_adapter_fails_without_persisting_session_messages() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state_with_channels(&dir, Arc::new(ChannelRegistry::new()));
        let agent_id = "lyre";
        let intention = test_intention("missing_adapter");
        let pulse_run_id = "run-missing-adapter-001";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, &intention.id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: "This should not be persisted.".to_string(),
            output_kind: PulseOutputKind::Notify,
            tool_phases: Vec::new(),
        };

        // Act
        let result = handle_output(
            &state,
            agent_id,
            &intention,
            &home_surface,
            &activation,
            pulse_run_id,
        )
        .await;

        // Assert
        assert!(result.is_err());
        let messages = state.db.get_all_messages(chat_id).expect("messages");
        assert!(
            messages.is_empty(),
            "missing adapter should fail before session persistence"
        );
    }

    #[test]
    fn pulse_synthetic_sets_user_kind() {
        let msg = StoredMessage {
            id: "test-pulse".to_string(),
            chat_id: 1,
            sender_id: "pulse".to_string(),
            content: "[Pulse: test] content".to_string(),
            sender_kind: SenderKind::User,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message_kind: MessageKind::SystemEvent,
            recipient_agent_id: None,
        };
        assert_eq!(msg.sender_kind, SenderKind::User);
        assert_eq!(msg.sender_id, "pulse");
        assert_eq!(msg.message_kind, MessageKind::SystemEvent);
    }
}

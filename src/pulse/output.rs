//! Pulse output handling — routes activation results to the appropriate destination.
//!
//! After a Pulse Activation completes, this module decides what happens:
//! - **PULSE_OK** (silent): updates the pulse run, does not send or persist anything.
//! - **Notification**: persists a synthetic conversation turn to the normal session
//!   (updating both messages and session snapshot), sends to the channel adapter,
//!   and updates the pulse run.

use std::sync::Arc;

use tracing::warn;

use crate::agent_loop::session::{PersistedTurn, persist_phase_once};
use crate::error::EgoPulseError;
use crate::llm::Message;
use crate::pulse::home_surface::HomeSurface;
use crate::pulse::runner::ActivationResult;
use crate::runtime::AppState;
use crate::storage::{MessageKind, PulseOutputKind, StoredMessage};

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
    intention_id: &str,
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

/// Notification path: persist synthetic turn with session snapshot, send to channel, update pulse run.
async fn handle_notify(
    state: &AppState,
    agent_id: &str,
    intention_id: &str,
    home_surface: &HomeSurface,
    output_text: &str,
    pulse_run_id: &str,
) -> Result<OutputResult, EgoPulseError> {
    let chat_id = home_surface.chat_id;

    let persist_result =
        persist_notification_with_session(state, agent_id, intention_id, chat_id, output_text)
            .await;

    let message_id = match persist_result {
        Ok(id) => id,
        Err(e) => {
            warn!(
                error = %e,
                agent_id,
                intention_id,
                "pulse notification persistence failed"
            );
            let error_msg = e.to_string();
            let db = Arc::clone(&state.db);
            let run_id = pulse_run_id.to_string();
            tokio::spawn(async move {
                let _ = crate::storage::call_blocking(db, move |db| {
                    db.update_pulse_run_failed(&run_id, &error_msg)
                })
                .await;
            });
            return Err(e);
        }
    };

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

async fn persist_notification_with_session(
    state: &AppState,
    agent_id: &str,
    intention_id: &str,
    chat_id: i64,
    output_text: &str,
) -> Result<String, EgoPulseError> {
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

    let loaded = crate::agent_loop::session::load_messages_for_turn(state, chat_id).await?;

    let mut session_messages = loaded.messages;
    session_messages.push(Message::text("user", &synthetic_input.content));

    let PersistedTurn {
        updated_at,
        messages: mut session_messages,
    } = persist_phase_once(
        state,
        synthetic_input.clone(),
        &session_messages,
        loaded.session_updated_at,
    )
    .await?;

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

    session_messages.push(Message::text("assistant", output_text));

    persist_phase_once(state, assistant_msg, &session_messages, Some(updated_at)).await?;

    Ok(assistant_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::adapter::{ChannelAdapter, ChannelRegistry, ConversationKind};
    use crate::skills::SkillManager;
    use crate::storage::Database;
    use crate::tools::ToolRegistry;
    use std::sync::Arc;

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
        let state_root = dir.path().to_str().expect("utf8").to_string();
        let config = crate::test_util::test_config(&state_root);
        let db = Arc::new(Database::new(&config.db_path()).expect("db"));
        let skills = Arc::new(SkillManager::from_dirs(
            config.user_skills_dir().expect("user_skills_dir"),
            config.skills_dir().expect("skills_dir"),
        ));
        let mut channels = ChannelRegistry::new();
        channels.register(Arc::new(MockChannelAdapter));
        AppState {
            db,
            config: config.clone(),
            config_path: None,
            llm_override: None,
            channels: Arc::new(channels),
            skills: Arc::clone(&skills),
            tools: Arc::new(ToolRegistry::new(&config, skills)),
            mcp_manager: None,
            assets: Arc::new(crate::assets::AssetStore::new(&config.assets_dir()).expect("assets")),
            soul_agents: Arc::new(crate::soul_agents::SoulAgentsLoader::new(&config)),
            memory_loader: Arc::new(crate::memory::MemoryLoader::new(
                std::path::PathBuf::from(&state_root).join("agents"),
            )),
            llm_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            active_turns: Arc::new(crate::runtime::ActiveTurnTracker::new()),
            turn_sender: tokio::sync::mpsc::channel(16).0,
            turn_scheduler: Arc::new(crate::runtime::turn_scheduler::TurnScheduler::new()),
            turn_tracker: Arc::new(crate::runtime::turn_scheduler::TurnTracker::new()),
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
        let intention_id = "morning_review";
        let pulse_run_id = "run-001";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: "PULSE_OK".to_string(),
            output_kind: PulseOutputKind::Silent,
        };

        // Act
        let result = handle_output(
            &state,
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
        let intention_id = "morning_review";
        let pulse_run_id = "run-003";
        let notification_text = "Good morning! You have 2 tasks today.";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        // Act
        handle_output(
            &state,
            agent_id,
            intention_id,
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
    async fn notify_updates_session_snapshot() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);
        let agent_id = "lyre";
        let intention_id = "snapshot_test";
        let pulse_run_id = "run-snapshot-001";
        let notification_text = "Session snapshot should be updated.";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        // Act
        handle_output(
            &state,
            agent_id,
            intention_id,
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
    async fn notify_does_not_store_pulse_capsule_body() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);
        let agent_id = "lyre";
        let intention_id = "check_in";
        let pulse_run_id = "run-004";
        let capsule_prompt_text = "# Pulse Activation\n## Core Contract\nYou are an agent.";
        let notification_text = "All quiet. Nothing to report.";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        // Act
        handle_output(
            &state,
            agent_id,
            intention_id,
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
        let intention_id = "weekly_report";
        let pulse_run_id = "run-005";
        let notification_text = "Weekly summary: 42 conversations processed.";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: notification_text.to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        // Act
        let result = handle_output(
            &state,
            agent_id,
            intention_id,
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
        let intention_id = "broken_intention";
        let pulse_run_id = "run-006";

        let chat_id = insert_chat(&state.db, agent_id);
        create_pulse_run(&state.db, pulse_run_id, agent_id, intention_id);

        let home_surface = test_home_surface(chat_id);
        let activation = ActivationResult {
            output_text: "This should fail.".to_string(),
            output_kind: PulseOutputKind::Notify,
        };

        {
            let conn = state.db.conn.lock().expect("lock");
            conn.execute("DROP TABLE messages", rusqlite::params![])
                .expect("drop messages");
        }

        // Act
        let result = handle_output(
            &state,
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

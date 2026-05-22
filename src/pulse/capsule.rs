//! Pulse Capsule — gate evaluation, home surface resolution, and capsule construction.
//!
//! This module covers the preparation phase before a Pulse Activation:
//! 1. **Gate**: duplicate / active-turn check
//! 2. **Home Surface**: find the best chat for pulse delivery
//! 3. **Capsule**: build the LLM input from all resolved components

use std::sync::Arc;

use crate::storage::Database;

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// Result of gate evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GateDecision {
    /// The intention should proceed to activation.
    Allow,
    /// The intention was already processed (duplicate due_key).
    Duplicate,
    /// The agent currently has an active turn; defer until next tick.
    DeferActive,
}

/// Evaluate whether a due intention should pass through the gate.
///
/// Gate v1 checks:
/// 1. Has this due_key already been processed? (check `pulse_runs`)
/// 2. Is the agent currently in an active turn?
///
/// # Errors
/// Returns `StorageError` when the database query fails.
pub(crate) async fn evaluate_gate(
    db: &Arc<Database>,
    agent_id: &str,
    intention_id: &str,
    due_key: &str,
    is_active: bool,
) -> Result<GateDecision, crate::error::StorageError> {
    let agent_id = agent_id.to_string();
    let intention_id = intention_id.to_string();
    let due_key = due_key.to_string();

    let has_run = crate::storage::call_blocking(db.clone(), move |db| {
        db.has_pulse_due_run(&agent_id, &intention_id, &due_key)
    })
    .await?;

    if has_run {
        return Ok(GateDecision::Duplicate);
    }

    if is_active {
        return Ok(GateDecision::DeferActive);
    }

    Ok(GateDecision::Allow)
}

// ---------------------------------------------------------------------------
// Home Surface
// ---------------------------------------------------------------------------

/// Resolved home surface for pulse delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HomeSurface {
    pub chat_id: i64,
    pub channel: String,
    pub external_chat_id: String,
    pub chat_type: String,
}

const SENDABLE_CHANNELS: &[&str] = &["discord", "telegram"];

/// Resolve the home surface for an agent.
///
/// Queries the database for the agent's chats ordered by recency,
/// then returns the first chat on a sendable channel (Discord or Telegram).
///
/// # Errors
/// Returns `StorageError` when the database query fails.
pub(crate) async fn resolve_home_surface(
    db: &Arc<Database>,
    agent_id: &str,
    available_channels: &[&str],
) -> Result<Option<HomeSurface>, crate::error::StorageError> {
    let sendable_channels = SENDABLE_CHANNELS
        .iter()
        .copied()
        .filter(|channel| available_channels.contains(channel))
        .collect::<Vec<_>>();
    if sendable_channels.is_empty() {
        return Ok(None);
    }

    let agent_id = agent_id.to_string();

    let chats = crate::storage::call_blocking(db.clone(), move |db| {
        db.get_agent_chats_by_recent(&agent_id, &sendable_channels)
    })
    .await?;

    let Some(first) = chats.into_iter().next() else {
        return Ok(None);
    };

    Ok(Some(HomeSurface {
        chat_id: first.chat_id,
        channel: first.channel,
        external_chat_id: first.external_chat_id,
        chat_type: first.chat_type,
    }))
}

// ---------------------------------------------------------------------------
// Capsule
// ---------------------------------------------------------------------------

use crate::pulse::definition::TemporalIntention;

const CORE_CONTRACT: &str = include_str!("pulse_core_contract.md");

/// Constructed Pulse Capsule ready to send to LLM.
#[derive(Clone, Debug)]
pub(crate) struct PulseCapsule {
    /// The complete prompt text to send as the user message.
    pub prompt: String,
}

/// Build a Pulse Capsule from the resolved components.
///
/// # Arguments
/// * `agent_id` - Agent identifier
/// * `intention` - The due intention
/// * `pulse_body` - The PULSE.md body (notes section)
/// * `recent_messages` - Recent user-visible messages from Home Surface (max 10)
/// * `home_surface` - The resolved home surface
/// * `now_rfc3339` - Current timestamp in RFC3339
pub(crate) fn build_capsule(
    agent_id: &str,
    intention: &TemporalIntention,
    pulse_body: &str,
    recent_messages: &[String],
    home_surface: &HomeSurface,
    now_rfc3339: &str,
) -> PulseCapsule {
    let mut sections = String::new();

    // Header
    sections.push_str("# Pulse Activation\n\n");
    sections.push_str(&format!("agent_id: {agent_id}\n"));
    sections.push_str(&format!("intention_id: {}\n", intention.id));
    sections.push_str("trigger: temporal_due\n");
    sections.push_str("home_surface:\n");
    sections.push_str(&format!("  channel: {}\n", home_surface.channel));
    sections.push_str(&format!(
        "  external_chat_id: {}\n",
        home_surface.external_chat_id
    ));
    sections.push_str(&format!("now: {now_rfc3339}\n\n"));

    // Core Contract
    sections.push_str("## Core Contract\n\n");
    sections.push_str(CORE_CONTRACT);
    sections.push_str("\n\n");

    // Temporal Intention
    sections.push_str("## Temporal Intention\n\n");
    sections.push_str(&intention.attention);
    sections.push_str("\n\n");

    // Pulse Notes
    sections.push_str("## Pulse Notes\n\n");
    sections.push_str(pulse_body);
    sections.push_str("\n\n");

    // Recent Visible Context
    sections.push_str("## Recent Visible Context\n\n");
    if recent_messages.is_empty() {
        sections.push_str("No recent context.\n\n");
    } else {
        for msg in recent_messages {
            sections.push_str(msg);
            sections.push('\n');
        }
        sections.push('\n');
    }

    PulseCapsule { prompt: sections }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulse::definition::TemporalSchedule;

    // --- Gate tests ---

    fn test_db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (Arc::new(db), dir)
    }

    #[tokio::test]
    async fn gate_blocks_duplicate_due_key() {
        let (db, _dir) = test_db();
        db.try_create_pulse_run("run-1", "agent-a", "int-1", "2025-01-01")
            .expect("create pulse run");

        let decision = evaluate_gate(&db, "agent-a", "int-1", "2025-01-01", false)
            .await
            .expect("gate");

        assert_eq!(decision, GateDecision::Duplicate);
    }

    #[tokio::test]
    async fn gate_defers_active_agent_without_run_record() {
        let (db, _dir) = test_db();

        let decision = evaluate_gate(&db, "agent-a", "int-1", "2025-01-01", true)
            .await
            .expect("gate");

        assert_eq!(decision, GateDecision::DeferActive);

        let has_run = db
            .has_pulse_due_run("agent-a", "int-1", "2025-01-01")
            .expect("has run");
        assert!(!has_run, "defer should not consume the due_key");
    }

    #[tokio::test]
    async fn gate_allows_deferred_due_key_on_next_tick() {
        let (db, _dir) = test_db();

        let first = evaluate_gate(&db, "agent-a", "int-1", "2025-01-01", true)
            .await
            .expect("gate first");
        assert_eq!(first, GateDecision::DeferActive);

        let second = evaluate_gate(&db, "agent-a", "int-1", "2025-01-01", false)
            .await
            .expect("gate second");
        assert_eq!(second, GateDecision::Allow);
    }

    // --- Home Surface tests ---

    fn insert_chat(
        db: &Database,
        channel: &str,
        external_chat_id: &str,
        chat_type: &str,
        agent_id: &str,
        last_message_time: &str,
    ) -> i64 {
        let chat_id = db
            .resolve_or_create_chat_id(channel, external_chat_id, None, chat_type, agent_id)
            .expect("create chat");
        let conn = db.conn.lock().expect("lock");
        conn.execute(
            "UPDATE chats SET last_message_time = ?1 WHERE chat_id = ?2",
            rusqlite::params![last_message_time, chat_id],
        )
        .expect("set last_message_time");
        chat_id
    }

    #[tokio::test]
    async fn home_surface_uses_latest_sendable_agent_chat() {
        let (db, _dir) = test_db();

        let _discord_id = insert_chat(
            &db,
            "discord",
            "discord:111",
            "dm",
            "agent-a",
            "2024-01-01T00:00:00Z",
        );

        let telegram_id = insert_chat(
            &db,
            "telegram",
            "telegram:222",
            "dm",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );

        let surface = resolve_home_surface(&db, "agent-a", &["discord", "telegram"])
            .await
            .expect("resolve")
            .expect("should find surface");

        assert_eq!(surface.chat_id, telegram_id);
        assert_eq!(surface.channel, "telegram");
    }

    #[tokio::test]
    async fn home_surface_skips_web_cli_tui_and_uses_previous_sendable_chat() {
        let (db, _dir) = test_db();

        let discord_id = insert_chat(
            &db,
            "discord",
            "discord:333",
            "dm",
            "agent-a",
            "2024-01-01T00:00:00Z",
        );

        let _web_id = insert_chat(
            &db,
            "web",
            "web:444",
            "dm",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );

        let surface = resolve_home_surface(&db, "agent-a", &["discord"])
            .await
            .expect("resolve")
            .expect("should find surface");

        assert_eq!(surface.chat_id, discord_id);
        assert_eq!(surface.channel, "discord");
    }

    #[tokio::test]
    async fn home_surface_skips_when_no_sendable_chat() {
        let (db, _dir) = test_db();

        insert_chat(
            &db,
            "web",
            "web:555",
            "dm",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );

        let surface = resolve_home_surface(&db, "agent-a", &["discord", "telegram"])
            .await
            .expect("resolve");

        assert!(surface.is_none());
    }

    #[tokio::test]
    async fn home_surface_does_not_use_default_delivery() {
        let (db, _dir) = test_db();

        insert_chat(
            &db,
            "cli",
            "cli:666",
            "cli",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );
        insert_chat(
            &db,
            "tui",
            "tui:777",
            "tui",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );

        let surface = resolve_home_surface(&db, "agent-a", &["discord", "telegram"])
            .await
            .expect("resolve");

        assert!(surface.is_none());
    }

    #[tokio::test]
    async fn home_surface_skips_sendable_db_chat_when_adapter_unavailable() {
        let (db, _dir) = test_db();
        insert_chat(
            &db,
            "discord",
            "discord:888",
            "dm",
            "agent-a",
            "2024-06-01T00:00:00Z",
        );

        let surface = resolve_home_surface(&db, "agent-a", &["telegram"])
            .await
            .expect("resolve");

        assert!(surface.is_none());
    }

    // --- Capsule tests ---

    fn test_intention() -> TemporalIntention {
        TemporalIntention {
            id: "morning_review".to_string(),
            enabled: true,
            schedule: TemporalSchedule::Daily {
                at: "09:00".to_string(),
            },
            attention: "Check today's schedule and unresolved items.".to_string(),
        }
    }

    fn test_home_surface() -> HomeSurface {
        HomeSurface {
            chat_id: 42,
            channel: "discord".to_string(),
            external_chat_id: "1234567890123456789".to_string(),
            chat_type: "dm".to_string(),
        }
    }

    #[test]
    fn capsule_includes_contract_intention_notes_and_recent_context() {
        let intention = test_intention();
        let surface = test_home_surface();
        let pulse_body = "Don't notify for trivial changes.";
        let recent = vec!["User said hello".to_string(), "Bot replied hi".to_string()];

        let capsule = build_capsule(
            "lyre",
            &intention,
            pulse_body,
            &recent,
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        let prompt = &capsule.prompt;
        assert!(prompt.contains("# Pulse Activation"));
        assert!(prompt.contains("agent_id: lyre"));
        assert!(prompt.contains("intention_id: morning_review"));
        assert!(prompt.contains("trigger: temporal_due"));
        assert!(prompt.contains("channel: discord"));
        assert!(prompt.contains("external_chat_id: 1234567890123456789"));
        assert!(prompt.contains("now: 2026-05-10T09:00:00+09:00"));
        assert!(prompt.contains("## Core Contract"));
        assert!(prompt.contains("PULSE_OK"));
        assert!(prompt.contains("## Temporal Intention"));
        assert!(prompt.contains("Check today's schedule and unresolved items."));
        assert!(prompt.contains("## Pulse Notes"));
        assert!(prompt.contains("Don't notify for trivial changes."));
        assert!(prompt.contains("## Recent Visible Context"));
        assert!(prompt.contains("User said hello"));
        assert!(prompt.contains("Bot replied hi"));
    }

    #[test]
    fn capsule_uses_recent_visible_messages_from_messages_table() {
        let intention = test_intention();
        let surface = test_home_surface();
        let mut recent = Vec::new();
        for i in 0..10 {
            recent.push(format!("Message number {i}"));
        }

        let capsule = build_capsule(
            "lyre",
            &intention,
            "body",
            &recent,
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        let prompt = &capsule.prompt;
        for i in 0..10 {
            assert!(
                prompt.contains(&format!("Message number {i}")),
                "prompt should contain message {i}"
            );
        }
    }

    #[test]
    fn capsule_excludes_full_session_and_internal_history() {
        let intention = test_intention();
        let surface = test_home_surface();

        let capsule = build_capsule(
            "lyre",
            &intention,
            "notes",
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        let prompt = &capsule.prompt;
        assert!(
            !prompt.contains("## Full Session"),
            "capsule should not contain full session section"
        );
        assert!(
            !prompt.contains("## Internal History"),
            "capsule should not contain internal history section"
        );
        assert!(
            !prompt.contains("session_history"),
            "capsule should not contain session_history"
        );

        assert!(prompt.contains("# Pulse Activation"));
        assert!(prompt.contains("## Core Contract"));
    }

    #[test]
    fn capsule_shows_no_recent_context_when_empty() {
        let intention = test_intention();
        let surface = test_home_surface();

        let capsule = build_capsule(
            "lyre",
            &intention,
            "body",
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        assert!(
            capsule.prompt.contains("No recent context."),
            "capsule should show 'No recent context.' when no messages"
        );
    }
}

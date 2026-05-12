//! Pulse Capsule — builds the LLM input for a Pulse Activation.

use crate::pulse::definition::TemporalIntention;
use crate::pulse::home_surface::HomeSurface;

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
/// * `prospective_memory` - Optional prospective memory content
/// * `recent_messages` - Recent user-visible messages from Home Surface (max 10)
/// * `home_surface` - The resolved home surface
/// * `now_rfc3339` - Current timestamp in RFC3339
pub(crate) fn build_capsule(
    agent_id: &str,
    intention: &TemporalIntention,
    pulse_body: &str,
    prospective_memory: Option<&str>,
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

    // Prospective Memory (optional)
    if let Some(memory) = prospective_memory {
        sections.push_str("## Prospective Memory\n\n");
        sections.push_str(memory);
        sections.push_str("\n\n");
    }

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

    // Output Contract
    sections.push_str("## Output Contract\n\n");
    sections.push_str("- 何も通知すべきでなければ PULSE_OK\n");
    sections.push_str("- 通知すべき場合だけ、短いユーザー向け文を書く\n");
    sections.push_str("- 大きな作業は開始しない\n");
    sections.push_str("- 破壊的操作はしない\n");

    PulseCapsule { prompt: sections }
}

/// Returns the embedded Core Contract text for use as system prompt.
pub(crate) fn core_contract_text() -> &'static str {
    CORE_CONTRACT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulse::definition::TemporalSchedule;

    fn test_intention() -> TemporalIntention {
        TemporalIntention {
            id: "morning_review".to_string(),
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
    fn capsule_includes_contract_intention_notes_memory_and_recent_context() {
        // Arrange
        let intention = test_intention();
        let surface = test_home_surface();
        let pulse_body = "Don't notify for trivial changes.";
        let memory = "Check the deployment status.";
        let recent = vec!["User said hello".to_string(), "Bot replied hi".to_string()];

        // Act
        let capsule = build_capsule(
            "lyre",
            &intention,
            pulse_body,
            Some(memory),
            &recent,
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        // Assert
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
        assert!(prompt.contains("## Prospective Memory"));
        assert!(prompt.contains("Check the deployment status."));
        assert!(prompt.contains("## Recent Visible Context"));
        assert!(prompt.contains("User said hello"));
        assert!(prompt.contains("Bot replied hi"));
        assert!(prompt.contains("## Output Contract"));
    }

    #[test]
    fn capsule_uses_recent_visible_messages_from_messages_table() {
        // Arrange
        let intention = test_intention();
        let surface = test_home_surface();
        let mut recent = Vec::new();
        for i in 0..10 {
            recent.push(format!("Message number {i}"));
        }

        // Act
        let capsule = build_capsule(
            "lyre",
            &intention,
            "body",
            None,
            &recent,
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        // Assert: all 10 messages are present
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
        // Arrange
        let intention = test_intention();
        let surface = test_home_surface();

        // Act
        let capsule = build_capsule(
            "lyre",
            &intention,
            "notes",
            None,
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        // Assert: capsule does not contain session/full-history markers
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

        // Verify it uses the proper capsule structure instead
        assert!(prompt.contains("# Pulse Activation"));
        assert!(prompt.contains("## Core Contract"));
    }

    #[test]
    fn capsule_omits_prospective_memory_section_when_none() {
        // Arrange
        let intention = test_intention();
        let surface = test_home_surface();

        // Act
        let capsule = build_capsule(
            "lyre",
            &intention,
            "body",
            None,
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        // Assert
        assert!(
            !capsule.prompt.contains("## Prospective Memory"),
            "capsule should omit Prospective Memory section when no memory provided"
        );
    }

    #[test]
    fn capsule_shows_no_recent_context_when_empty() {
        // Arrange
        let intention = test_intention();
        let surface = test_home_surface();

        // Act
        let capsule = build_capsule(
            "lyre",
            &intention,
            "body",
            None,
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        // Assert
        assert!(
            capsule.prompt.contains("No recent context."),
            "capsule should show 'No recent context.' when no messages"
        );
    }
}

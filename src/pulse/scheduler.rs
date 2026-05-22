//! Pulse scheduler — tick loop and single scan execution.

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use tracing::{info, warn};

use crate::config::AgentId;
use crate::runtime::AppState;
use crate::storage::Database;

/// Run a single pulse scan across all configured agents.
///
/// For each agent:
/// 1. Load PULSE.md → parse intentions
/// 2. For each intention → check due → evaluate gate → resolve home surface →
///    build capsule → run activation → handle output
/// 3. If parse error for one agent → warn and continue to next agent
/// 4. If execution error for one intention → warn and continue to next intention
pub(crate) async fn run_pulse_scan(state: &AppState) {
    let pulse_cfg = state.config.pulse();
    if !pulse_cfg.scheduler_enabled() {
        return;
    }

    let timezone = pulse_cfg.timezone.as_deref().unwrap_or("UTC");

    let state_root = Path::new(&state.config.state_root);
    let now = Utc::now();

    for agent_id in state.config.agents.keys() {
        scan_agent(state, state_root, agent_id, timezone, now).await;
    }
}

/// Scan a single agent's PULSE.md and process all due intentions.
async fn scan_agent(
    state: &AppState,
    state_root: &Path,
    agent_id: &AgentId,
    timezone: &str,
    now: chrono::DateTime<Utc>,
) {
    let definition = match super::definition::load_pulse_definition(state_root, agent_id.as_str()) {
        Ok(d) => d,
        Err(e) => {
            warn!(
                agent_id = %agent_id,
                error = %e,
                "pulse scan: failed to load PULSE.md, skipping agent"
            );
            return;
        }
    };

    if definition.intentions.is_empty() {
        return;
    }

    for intention in &definition.intentions {
        process_intention(state, agent_id, intention, &definition.body, timezone, now).await;
    }
}

/// Process a single intention: check due → gate → create run → resolve surface →
/// build capsule → activate → handle output.
async fn process_intention(
    state: &AppState,
    agent_id: &AgentId,
    intention: &super::definition::TemporalIntention,
    pulse_body: &str,
    timezone: &str,
    now: chrono::DateTime<Utc>,
) {
    let agent_id_str = agent_id.as_str();

    // 1. Skip disabled intentions
    if !intention.enabled {
        return;
    }

    // 2. Check due
    let due_check = super::definition::check_due(agent_id_str, intention, now, timezone);
    if !due_check.due {
        return;
    }

    // 3. Evaluate gate
    let is_active = state.active_turns.is_active(agent_id_str);
    let decision = match super::capsule::evaluate_gate(
        &state.db,
        agent_id_str,
        &intention.id,
        &due_check.due_key,
        is_active,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            warn!(
                agent_id = agent_id_str,
                intention_id = %intention.id,
                error = %e,
                "pulse scan: gate evaluation failed"
            );
            return;
        }
    };

    match decision {
        super::capsule::GateDecision::Duplicate | super::capsule::GateDecision::DeferActive => {
            return;
        }
        super::capsule::GateDecision::Allow => {}
    }

    // 4. Create pulse_run
    let pulse_run_id = uuid::Uuid::new_v4().to_string();
    if let Err(e) = create_pulse_run(
        &state.db,
        &pulse_run_id,
        agent_id_str,
        &intention.id,
        &due_check.due_key,
    )
    .await
    {
        warn!(
            agent_id = agent_id_str,
            intention_id = %intention.id,
            error = %e,
            "pulse scan: failed to create pulse_run, skipping"
        );
        return;
    }

    // 5. Resolve home surface
    let available_channels = state.channels.names();
    let home_surface =
        match super::capsule::resolve_home_surface(&state.db, agent_id_str, &available_channels)
            .await
        {
            Ok(Some(surface)) => surface,
            Ok(None) => {
                if let Err(e) =
                    update_run_skipped(&state.db, &pulse_run_id, "no sendable home surface").await
                {
                    warn!(
                        pulse_run_id = %pulse_run_id,
                        error = %e,
                        "pulse scan: failed to mark pulse_run as skipped"
                    );
                }
                return;
            }
            Err(e) => {
                warn!(
                    agent_id = agent_id_str,
                    error = %e,
                    "pulse scan: failed to resolve home surface"
                );
                if let Err(e) =
                    update_run_skipped(&state.db, &pulse_run_id, "home surface resolution failed")
                        .await
                {
                    warn!(
                        pulse_run_id = %pulse_run_id,
                        error = %e,
                        "pulse scan: failed to mark pulse_run as skipped"
                    );
                }
                return;
            }
        };

    // 6. Build capsule
    // Prospective memory is already injected via build_system_prompt() in the
    // system prompt; omitting it here avoids duplication.
    let recent_messages = load_recent_messages(&state.db, home_surface.chat_id).await;
    let now_rfc3339 = now.to_rfc3339();

    let capsule = super::capsule::build_capsule(
        agent_id_str,
        intention,
        pulse_body,
        &recent_messages,
        &home_surface,
        &now_rfc3339,
    );

    // 7. Run activation
    let activation_result =
        match super::runner::run_activation(state, agent_id_str, &capsule, &home_surface).await {
            Ok(result) => result,
            Err(e) => {
                warn!(
                    agent_id = agent_id_str,
                    intention_id = %intention.id,
                    error = %e,
                    "pulse scan: activation failed"
                );
                if let Err(e) = update_run_failed(&state.db, &pulse_run_id, &e.to_string()).await {
                    warn!(
                        pulse_run_id = %pulse_run_id,
                        error = %e,
                        "pulse scan: failed to mark pulse_run as failed"
                    );
                }
                return;
            }
        };

    // 8. Handle output
    if let Err(e) = super::output::handle_output(
        state,
        agent_id_str,
        intention,
        &home_surface,
        &activation_result,
        &pulse_run_id,
    )
    .await
    {
        warn!(
            agent_id = agent_id_str,
            intention_id = %intention.id,
            error = %e,
            "pulse scan: output handling failed"
        );
    }
}

/// Create a pulse_run record via the blocking pool.
async fn create_pulse_run(
    db: &Arc<Database>,
    id: &str,
    agent_id: &str,
    intention_id: &str,
    due_key: &str,
) -> Result<(), crate::error::StorageError> {
    let id = id.to_string();
    let agent_id = agent_id.to_string();
    let intention_id = intention_id.to_string();
    let due_key = due_key.to_string();
    crate::storage::call_blocking(Arc::clone(db), move |db| {
        db.try_create_pulse_run(&id, &agent_id, &intention_id, &due_key)
    })
    .await
}

/// Mark a pulse_run as skipped.
async fn update_run_skipped(
    db: &Arc<Database>,
    pulse_run_id: &str,
    reason: &str,
) -> Result<(), crate::error::StorageError> {
    let pulse_run_id = pulse_run_id.to_string();
    let reason = reason.to_string();
    crate::storage::call_blocking(Arc::clone(db), move |db| {
        db.update_pulse_run_skipped(&pulse_run_id, &reason)
    })
    .await
}

/// Mark a pulse_run as failed.
async fn update_run_failed(
    db: &Arc<Database>,
    pulse_run_id: &str,
    error_message: &str,
) -> Result<(), crate::error::StorageError> {
    let pulse_run_id = pulse_run_id.to_string();
    let error_message = error_message.to_string();
    crate::storage::call_blocking(Arc::clone(db), move |db| {
        db.update_pulse_run_failed(&pulse_run_id, &error_message)
    })
    .await
}

/// Load up to 10 recent messages for the given chat (for capsule context).
async fn load_recent_messages(db: &Arc<Database>, chat_id: i64) -> Vec<String> {
    crate::storage::call_blocking(Arc::clone(db), move |db| {
        let messages = db.get_recent_messages(chat_id, 10)?;
        Ok(messages.into_iter().map(|m| m.content).collect())
    })
    .await
    .unwrap_or_default()
}

/// Run the pulse scheduler tick loop.
///
/// Sleeps for the configured tick interval between scans.
/// Runs indefinitely until the task is cancelled (via tokio cancellation).
pub(crate) async fn run_pulse_scheduler(state: AppState) {
    let tick_interval = state.config.pulse().tick_interval_secs;
    info!("pulse scheduler starting with {tick_interval}s tick interval");

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(tick_interval)).await;
        run_pulse_scan(&state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, AgentId, PulseConfig};
    use crate::storage::PulseRunStatus;
    use std::fs;

    fn build_pulse_state(
        dir: &tempfile::TempDir,
        pulse_config: PulseConfig,
        agents: Vec<(&str, &str)>,
    ) -> AppState {
        let state_root = dir.path().to_str().expect("utf8");
        let mut config = crate::test_util::test_config(state_root);
        config.pulse = pulse_config;

        config.agents.clear();
        for (agent_id, pulse_md_content) in &agents {
            config.agents.insert(
                AgentId::new(agent_id),
                AgentConfig {
                    label: agent_id.to_string(),
                    ..Default::default()
                },
            );
            if !pulse_md_content.is_empty() {
                let agents_dir = dir.path().join("agents").join(agent_id);
                fs::create_dir_all(&agents_dir).expect("create agent dir");
                fs::write(agents_dir.join("PULSE.md"), pulse_md_content).expect("write PULSE.md");
            }
        }

        let mut channels = crate::channels::adapter::ChannelRegistry::new();
        channels.register(Arc::new(MockChannelAdapter("discord")));
        channels.register(Arc::new(MockChannelAdapter("telegram")));

        crate::test_util::build_state_with_config(
            config.clone(),
            Some(Arc::new(MockPulseLlm::new())),
            None,
            Some(Arc::new(Database::new(&config.db_path()).expect("db"))),
            Some(Arc::new(channels)),
        )
    }

    fn enabled_pulse_config() -> PulseConfig {
        PulseConfig {
            enabled: true,
            tick_interval_secs: 60,
            timezone: Some("UTC".to_string()),
        }
    }

    fn valid_daily_pulse_md() -> String {
        "\
---
version: 1
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: \"00:00\"
    attention: Check today.
---

# Notes
Some notes.
"
        .to_string()
    }

    fn count_agent_runs(db: &Arc<Database>, agent_id: &str) -> usize {
        let conn = db.conn.lock().expect("lock");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pulse_runs WHERE agent_id = ?1",
                rusqlite::params![agent_id],
                |row| row.get(0),
            )
            .expect("count");
        count as usize
    }

    fn latest_run_status(db: &Arc<Database>, agent_id: &str) -> PulseRunStatus {
        let conn = db.conn.lock().expect("lock");
        let status_str: String = conn
            .query_row(
                "SELECT status FROM pulse_runs WHERE agent_id = ?1 ORDER BY started_at DESC LIMIT 1",
                rusqlite::params![agent_id],
                |row| row.get(0),
            )
            .expect("status");
        status_str.parse().expect("parse status")
    }

    #[tokio::test]
    async fn scheduler_disabled_exits_without_scan() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let pulse_config = PulseConfig {
            enabled: false,
            ..Default::default()
        };
        let state = build_pulse_state(&dir, pulse_config, vec![("default", "")]);

        // Act
        run_pulse_scan(&state).await;

        // Assert
        assert_eq!(
            count_agent_runs(&state.db, "default"),
            0,
            "disabled scheduler should not create any runs"
        );
    }

    #[tokio::test]
    async fn scheduler_scan_loads_all_configured_agents() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_pulse_state(
            &dir,
            enabled_pulse_config(),
            vec![
                ("agent-a", &valid_daily_pulse_md()),
                ("agent-b", &valid_daily_pulse_md()),
            ],
        );

        let _chat_a = state
            .db
            .resolve_or_create_chat_id("discord", "discord:a", None, "dm", "agent-a")
            .expect("chat a");
        let _chat_b = state
            .db
            .resolve_or_create_chat_id("telegram", "telegram:b", None, "dm", "agent-b")
            .expect("chat b");

        // Act
        run_pulse_scan(&state).await;

        // Assert
        assert_eq!(
            count_agent_runs(&state.db, "agent-a"),
            1,
            "agent-a should have 1 pulse_run"
        );
        assert_eq!(
            count_agent_runs(&state.db, "agent-b"),
            1,
            "agent-b should have 1 pulse_run"
        );
    }

    #[tokio::test]
    async fn scheduler_runs_due_intention_once() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_pulse_state(
            &dir,
            enabled_pulse_config(),
            vec![("default", &valid_daily_pulse_md())],
        );

        let _chat = state
            .db
            .resolve_or_create_chat_id("discord", "discord:123", None, "dm", "default")
            .expect("chat");

        // Act
        run_pulse_scan(&state).await;
        run_pulse_scan(&state).await;

        // Assert
        assert_eq!(
            count_agent_runs(&state.db, "default"),
            1,
            "should have exactly 1 pulse_run after two scans"
        );
    }

    #[tokio::test]
    async fn scheduler_continues_after_agent_parse_error() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let invalid_yaml = "\
---
version: not_a_number
intentions:
  - bad
---
body
";
        let state = build_pulse_state(
            &dir,
            enabled_pulse_config(),
            vec![
                ("agent-a", invalid_yaml),
                ("agent-b", &valid_daily_pulse_md()),
            ],
        );

        let _chat_b = state
            .db
            .resolve_or_create_chat_id("telegram", "telegram:b", None, "dm", "agent-b")
            .expect("chat b");

        // Act
        run_pulse_scan(&state).await;

        // Assert
        assert_eq!(
            count_agent_runs(&state.db, "agent-a"),
            0,
            "agent-a with invalid PULSE.md should have no runs"
        );
        assert_eq!(
            count_agent_runs(&state.db, "agent-b"),
            1,
            "agent-b should have 1 run"
        );
    }

    #[test]
    fn runtime_starts_pulse_scheduler_when_enabled() {
        // Arrange
        let pulse_config = enabled_pulse_config();

        // Assert
        assert!(
            pulse_config.scheduler_enabled(),
            "pulse config should be enabled"
        );
    }

    #[tokio::test]
    async fn runtime_requires_channel_even_when_pulse_scheduler_enabled() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        config.pulse = enabled_pulse_config();
        config.channels.clear();

        let state = crate::runtime::build_app_state(config)
            .await
            .expect("build state");

        // Act
        let result = crate::runtime::start_channels(state).await;

        // Assert
        assert!(
            result.is_err(),
            "start_channels should fail with NoActiveChannels"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("no enabled channel") || msg.contains("no active channel"),
            "error should mention no active channels, got: {msg}"
        );
    }

    #[tokio::test]
    async fn scheduler_continues_after_agent_error() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_pulse_state(
            &dir,
            enabled_pulse_config(),
            vec![
                ("agent-a", &valid_daily_pulse_md()),
                ("agent-b", &valid_daily_pulse_md()),
            ],
        );

        // agent-b has a sendable chat, agent-a does not
        let _chat_b = state
            .db
            .resolve_or_create_chat_id("telegram", "telegram:b", None, "dm", "agent-b")
            .expect("chat b");

        // Act
        run_pulse_scan(&state).await;

        // Assert
        assert_eq!(
            count_agent_runs(&state.db, "agent-a"),
            1,
            "agent-a should have 1 run (skipped)"
        );
        assert_eq!(
            latest_run_status(&state.db, "agent-a"),
            PulseRunStatus::Skipped,
            "agent-a run should be skipped"
        );

        assert_eq!(
            count_agent_runs(&state.db, "agent-b"),
            1,
            "agent-b should have 1 run"
        );
        assert_eq!(
            latest_run_status(&state.db, "agent-b"),
            PulseRunStatus::Success,
            "agent-b run should succeed"
        );
    }

    struct MockPulseLlm;

    struct MockChannelAdapter(&'static str);

    #[async_trait::async_trait]
    impl crate::channels::adapter::ChannelAdapter for MockChannelAdapter {
        fn name(&self) -> &str {
            self.0
        }

        fn chat_type_routes(&self) -> Vec<(&str, crate::channels::adapter::ConversationKind)> {
            vec![("dm", crate::channels::adapter::ConversationKind::Private)]
        }

        async fn send_text(&self, _external_chat_id: &str, _text: &str) -> Result<(), String> {
            Ok(())
        }
    }

    impl MockPulseLlm {
        fn new() -> Self {
            Self
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::LlmProvider for MockPulseLlm {
        fn provider_name(&self) -> &str {
            "mock-pulse"
        }

        fn model_name(&self) -> &str {
            "mock-pulse-model"
        }

        async fn send_message(
            &self,
            _system: &str,
            _messages: Vec<crate::llm::Message>,
            _tools: Option<Vec<crate::llm::ToolDefinition>>,
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            Ok(crate::llm::MessagesResponse {
                content: "PULSE_OK".to_string(),
                reasoning_content: None,
                tool_calls: vec![],
                usage: Some(crate::llm::LlmUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                }),
            })
        }
    }

    #[tokio::test]
    async fn scheduler_skips_disabled_intention() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let disabled_pulse_md = "\
---
version: 1
intentions:
  - id: morning_review
    enabled: false
    schedule:
      kind: daily
      at: \"00:00\"
    attention: Check today.
---

# Notes
Some notes.
";
        let state = build_pulse_state(
            &dir,
            enabled_pulse_config(),
            vec![("default", disabled_pulse_md)],
        );

        let _chat = state
            .db
            .resolve_or_create_chat_id("discord", "discord:123", None, "dm", "default")
            .expect("chat");

        // Act
        run_pulse_scan(&state).await;

        // Assert
        assert_eq!(
            count_agent_runs(&state.db, "default"),
            0,
            "disabled intention should not create any pulse_run"
        );
    }

    #[tokio::test]
    async fn scheduler_runs_only_enabled_intentions_when_mixed() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let mixed_pulse_md = "\
---
version: 1
intentions:
  - id: active_check
    schedule:
      kind: daily
      at: \"00:00\"
    attention: Active one.
  - id: paused_check
    enabled: false
    schedule:
      kind: daily
      at: \"00:00\"
    attention: Paused one.
---

# Notes
";
        let state = build_pulse_state(
            &dir,
            enabled_pulse_config(),
            vec![("default", mixed_pulse_md)],
        );

        let _chat = state
            .db
            .resolve_or_create_chat_id("discord", "discord:123", None, "dm", "default")
            .expect("chat");

        // Act
        run_pulse_scan(&state).await;

        // Assert
        assert_eq!(
            count_agent_runs(&state.db, "default"),
            1,
            "only enabled intention should create a pulse_run"
        );

        // Verify it was the enabled one
        let conn = state.db.conn.lock().expect("lock");
        let intention_id: String = conn
            .query_row(
                "SELECT intention_id FROM pulse_runs WHERE agent_id = ?1",
                rusqlite::params!["default"],
                |row| row.get(0),
            )
            .expect("intention_id");
        drop(conn);
        assert_eq!(intention_id, "active_check");
    }
}

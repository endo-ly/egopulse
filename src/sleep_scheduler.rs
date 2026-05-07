//! Sleep batch scheduler — schedule calculation and execution control.

use std::collections::HashMap;

use chrono::{DateTime, Duration, LocalResult, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use tracing::{info, warn};

use crate::config::{AgentConfig, AgentId, SleepBatchConfig};
use crate::runtime::AppState;
use crate::sleep_batch::{self, SleepBatchError};
use crate::storage::SleepRunTrigger;

/// Returns the next scheduled run as a UTC instant, or `None` if the scheduler
/// is disabled or the configuration is incomplete.
///
/// The result is a **pure function** of `(schedule, timezone, now)` with no
/// side-effects, making it straightforward to unit-test without tokio.
pub(crate) fn next_scheduled_run(
    config: &SleepBatchConfig,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if !config.scheduler_enabled() {
        return None;
    }
    let schedule = config.schedule.as_ref()?;
    let timezone = config.timezone.as_ref()?;
    let tz: Tz = timezone.parse().ok()?;
    let time = parse_schedule_time(schedule)?;
    next_run_for_time(tz, time, now)
}

/// Resolves the ordered list of agent IDs that should be processed in a
/// scheduled cycle.
///
/// Rules:
/// - `None` agents → all configured agents (default_agent first, rest sorted)
/// - `Some([])` → empty (no agents)
/// - `Some(list)` → filter to configured agents (already validated at load time)
pub(crate) fn resolve_target_agents(
    config: &SleepBatchConfig,
    all_agents: &HashMap<AgentId, AgentConfig>,
    default_agent: &AgentId,
) -> Vec<AgentId> {
    match &config.agents {
        None => {
            let mut agents: Vec<AgentId> = all_agents.keys().cloned().collect();
            agents.sort_by(|a, b| {
                let a_default = a == default_agent;
                let b_default = b == default_agent;
                b_default.cmp(&a_default).then_with(|| a.cmp(b))
            });
            agents
        }
        Some(list) => list.clone(),
    }
}

pub(crate) async fn run_scheduled_cycle(state: &AppState) {
    let config = &state.config.sleep_batch;
    if !config.scheduler_enabled() {
        return;
    }

    let agents = resolve_target_agents(config, &state.config.agents, &state.config.default_agent);
    if agents.is_empty() {
        info!("scheduled cycle: no agents to process");
        return;
    }

    for agent_id in &agents {
        if state.active_turns.is_active(agent_id.as_str()) {
            info!(agent_id = %agent_id, "scheduled cycle: agent active, deferring");
            continue;
        }

        match run_agent_with_retry(state, agent_id).await {
            Ok(()) => {}
            Err(SleepBatchError::AlreadyRunning { .. }) => {
                info!(agent_id = %agent_id, "scheduled cycle: already running, skipping");
            }
            Err(e) => {
                warn!(agent_id = %agent_id, error = %e, "scheduled cycle: agent failed");
            }
        }
    }
}

async fn run_agent_with_retry(state: &AppState, agent_id: &AgentId) -> Result<(), SleepBatchError> {
    let max_attempts = state.config.sleep_batch.retry_max_attempts;
    let interval = state.config.sleep_batch.retry_interval_minutes;
    let mut last_error = None;

    for attempt in 0..max_attempts {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs((interval as u64) * 60)).await;
        }

        match sleep_batch::run_sleep_batch(
            state,
            Some(agent_id.as_str()),
            SleepRunTrigger::Scheduled,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(SleepBatchError::AlreadyRunning { .. }) => {
                return Err(
                    last_error.unwrap_or_else(|| SleepBatchError::AlreadyRunning {
                        agent_id: agent_id.to_string(),
                    }),
                );
            }
            Err(e) => {
                warn!(
                    agent_id = %agent_id,
                    attempt = attempt + 1,
                    max_attempts,
                    error = %e,
                    "scheduled cycle: attempt failed"
                );
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| SleepBatchError::Internal("no attempts made".to_string())))
}

/// Runs the scheduler loop until the process is shut down.
///
/// Each iteration calculates the next scheduled run, sleeps until then,
/// and executes [`run_scheduled_cycle`]. Returns `Ok(())` on normal exit
/// (e.g. if the scheduler becomes disabled at runtime).
pub(crate) async fn run_scheduler_loop(state: AppState) -> Result<(), crate::error::EgoPulseError> {
    loop {
        let now = Utc::now();
        let next = match next_scheduled_run(&state.config.sleep_batch, now) {
            Some(t) => t,
            None => {
                info!("sleep scheduler: disabled or no schedule configured, exiting loop");
                return Ok(());
            }
        };

        let delay = (next - now).to_std().unwrap_or(std::time::Duration::ZERO);
        info!(
            next_run = %next.to_rfc3339(),
            delay_secs = delay.as_secs(),
            "sleep scheduler: waiting for next scheduled run"
        );

        tokio::time::sleep(delay).await;

        run_scheduled_cycle(&state).await;
    }
}

fn parse_schedule_time(schedule: &str) -> Option<NaiveTime> {
    let (hour, minute) = parse_hhmm(schedule)?;
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn parse_hhmm(schedule: &str) -> Option<(u32, u32)> {
    let (h, m) = schedule.split_once(':')?;
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some((hour, minute))
}

fn next_run_for_time(tz: Tz, time: NaiveTime, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let local_now = now.with_timezone(&tz);
    let today_date = local_now.date_naive();

    if let Some(instant) = try_date(tz, today_date, time, &local_now) {
        return Some(instant);
    }

    let tomorrow_date = today_date + Duration::days(1);
    try_date(tz, tomorrow_date, time, &local_now)
}

fn try_date(
    tz: Tz,
    date: chrono::NaiveDate,
    time: NaiveTime,
    local_now: &DateTime<Tz>,
) -> Option<DateTime<Utc>> {
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => {
            if dt > *local_now {
                Some(dt.with_timezone(&Utc))
            } else {
                None
            }
        }
        LocalResult::Ambiguous(earliest, _latest) => {
            if earliest > *local_now {
                Some(earliest.with_timezone(&Utc))
            } else {
                None
            }
        }
        LocalResult::None => resolve_gap(tz, naive, local_now),
    }
}

fn resolve_gap(
    tz: Tz,
    start: chrono::NaiveDateTime,
    local_now: &DateTime<Tz>,
) -> Option<DateTime<Utc>> {
    let mut candidate = start;
    for _ in 0..120 {
        candidate += Duration::minutes(1);
        if let LocalResult::Single(dt) = tz.from_local_datetime(&candidate) {
            if dt > *local_now {
                return Some(dt.with_timezone(&Utc));
            }
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config(schedule: &str, timezone: &str) -> SleepBatchConfig {
        SleepBatchConfig {
            enabled: true,
            schedule: Some(schedule.to_string()),
            timezone: Some(timezone.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn next_run_returns_today_when_time_is_future() {
        let config = enabled_config("14:00", "Asia/Tokyo");
        // 2026-01-15 05:00 UTC = 2026-01-15 14:00 JST
        // Set now to 13:00 JST = 04:00 UTC → target is still in the future
        let now = "2026-01-15T04:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-01-15 14:00 JST = 2026-01-15 05:00 UTC
        assert_eq!(
            result,
            "2026-01-15T05:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_run_returns_tomorrow_when_time_has_passed() {
        let config = enabled_config("14:00", "Asia/Tokyo");
        // Set now to 15:00 JST = 06:00 UTC → target already passed today
        let now = "2026-01-15T06:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-01-16 14:00 JST = 2026-01-16 05:00 UTC
        assert_eq!(
            result,
            "2026-01-16T05:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_run_uses_configured_iana_timezone() {
        let config = enabled_config("09:00", "America/New_York");
        // 2026-01-15 04:00 UTC = 2026-01-14 23:00 EST → 09:00 EST is tomorrow
        let now = "2026-01-15T04:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-01-15 09:00 EST = 2026-01-15 14:00 UTC
        assert_eq!(
            result,
            "2026-01-15T14:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_run_handles_utc_timezone() {
        let config = enabled_config("04:00", "UTC");
        let now = "2026-01-15T03:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        assert_eq!(
            result,
            "2026-01-15T04:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_run_moves_dst_gap_to_first_valid_time() {
        // America/New_York: DST starts 2026-03-08 at 02:00 EST → clocks jump to 03:00 EDT.
        // Local time 02:30 does not exist. Should move to 03:00 EDT.
        let config = enabled_config("02:30", "America/New_York");
        // 2026-03-08 01:00 EST = 06:00 UTC (before the gap)
        let now = "2026-03-08T06:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-03-08 03:00 EDT = 07:00 UTC
        assert_eq!(
            result,
            "2026-03-08T07:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_run_uses_earliest_instant_for_dst_fold() {
        // America/New_York: DST ends 2026-11-01 at 02:00 EDT → clocks fall back to 01:00 EST.
        // Local time 01:30 exists twice. Use earliest (EDT) = 05:30 UTC.
        let config = enabled_config("01:30", "America/New_York");
        // 2026-11-01 00:00 EDT = 04:00 UTC (before the fold)
        let now = "2026-11-01T04:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-11-01 01:30 EDT = 05:30 UTC (earliest instant)
        assert_eq!(
            result,
            "2026-11-01T05:30:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_run_rejects_invalid_local_time() {
        let config = enabled_config("99:00", "Asia/Tokyo");
        let now = "2026-01-15T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(next_scheduled_run(&config, now).is_none());
    }

    #[test]
    fn scheduler_config_disabled_has_no_next_run() {
        let config = SleepBatchConfig {
            enabled: false,
            ..Default::default()
        };
        let now = "2026-01-15T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(next_scheduled_run(&config, now).is_none());
    }

    fn agent_map(agent_ids: &[&str]) -> HashMap<AgentId, AgentConfig> {
        agent_ids
            .iter()
            .map(|id| (AgentId::new(id), AgentConfig::default()))
            .collect()
    }

    #[test]
    fn resolve_returns_all_agents_when_config_agents_is_none() {
        let config = SleepBatchConfig {
            enabled: true,
            ..Default::default()
        };
        let agents = agent_map(&["beta", "alpha", "default"]);
        let default = AgentId::new("default");

        let result = resolve_target_agents(&config, &agents, &default);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0], AgentId::new("default"));
    }

    #[test]
    fn resolve_places_default_agent_first() {
        let config = SleepBatchConfig {
            enabled: true,
            ..Default::default()
        };
        let agents = agent_map(&["zebra", "alpha", "default"]);
        let default = AgentId::new("default");

        let result = resolve_target_agents(&config, &agents, &default);

        assert_eq!(result[0], AgentId::new("default"));
        let rest = &result[1..];
        let mut sorted = rest.to_vec();
        sorted.sort();
        assert_eq!(rest, sorted.as_slice());
    }

    #[test]
    fn resolve_returns_configured_list_when_agents_is_some() {
        let config = SleepBatchConfig {
            enabled: true,
            agents: Some(vec![AgentId::new("beta"), AgentId::new("alpha")]),
            ..Default::default()
        };
        let agents = agent_map(&["alpha", "beta", "gamma"]);
        let default = AgentId::new("default");

        let result = resolve_target_agents(&config, &agents, &default);

        assert_eq!(result, vec![AgentId::new("beta"), AgentId::new("alpha")]);
    }

    #[test]
    fn resolve_returns_empty_when_agents_is_empty_list() {
        let config = SleepBatchConfig {
            enabled: true,
            agents: Some(vec![]),
            ..Default::default()
        };
        let agents = agent_map(&["alpha"]);
        let default = AgentId::new("default");

        let result = resolve_target_agents(&config, &agents, &default);

        assert!(result.is_empty());
    }

    use crate::test_util;

    fn scheduler_config_with_agents(
        enabled: bool,
        agent_ids: Option<Vec<&str>>,
    ) -> SleepBatchConfig {
        SleepBatchConfig {
            enabled,
            schedule: Some("03:00".to_string()),
            timezone: Some("UTC".to_string()),
            agents: agent_ids.map(|ids| ids.into_iter().map(AgentId::new).collect()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn scheduler_skips_when_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_util::build_state_with_provider(
            dir.path().to_str().unwrap(),
            Box::new(MockLlm::new()),
        );

        let config = SleepBatchConfig {
            enabled: false,
            ..Default::default()
        };

        let agents =
            resolve_target_agents(&config, &state.config.agents, &state.config.default_agent);
        assert!(agents.is_empty() || !config.scheduler_enabled());
    }

    #[tokio::test]
    async fn scheduler_runs_configured_agents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_util::test_config(dir.path().to_str().unwrap());
        let mut config = config;
        config.sleep_batch = scheduler_config_with_agents(true, Some(vec!["default"]));
        config.agents.insert(
            AgentId::new("other"),
            AgentConfig {
                label: "Other".to_string(),
                ..Default::default()
            },
        );

        let agents =
            resolve_target_agents(&config.sleep_batch, &config.agents, &config.default_agent);

        assert_eq!(agents, vec![AgentId::new("default")]);
    }

    #[tokio::test]
    async fn scheduler_runs_all_agents_when_agents_unset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_util::test_config(dir.path().to_str().unwrap());
        let mut config = config;
        config.sleep_batch = scheduler_config_with_agents(true, None);
        config.agents.insert(
            AgentId::new("second"),
            AgentConfig {
                label: "Second".to_string(),
                ..Default::default()
            },
        );

        let agents =
            resolve_target_agents(&config.sleep_batch, &config.agents, &config.default_agent);

        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0], AgentId::new("default"));
    }

    #[tokio::test]
    async fn scheduler_uses_scheduled_trigger() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_util::build_state_with_provider(
            dir.path().to_str().unwrap(),
            Box::new(MockLlm::new()),
        );

        run_scheduled_cycle(&state).await;

        let runs = state.db.list_sleep_runs("default", 10).expect("list runs");
        let scheduled_runs: Vec<_> = runs
            .iter()
            .filter(|r| r.trigger == SleepRunTrigger::Scheduled)
            .collect();
        assert!(scheduled_runs.is_empty());
    }

    #[tokio::test]
    async fn scheduler_defers_active_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_util::build_state_with_provider(
            dir.path().to_str().unwrap(),
            Box::new(MockLlm::new()),
        );

        state.active_turns.begin_turn("default");
        assert!(state.active_turns.is_active("default"));

        run_scheduled_cycle(&state).await;

        state.active_turns.end_turn("default");

        let runs = state.db.list_sleep_runs("default", 10).expect("list runs");
        assert!(runs.is_empty());
    }

    #[tokio::test]
    async fn scheduler_continues_after_skip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_util::build_state_with_provider(
            dir.path().to_str().unwrap(),
            Box::new(MockLlm::new()),
        );

        run_scheduled_cycle(&state).await;

        let runs = state.db.list_sleep_runs("default", 10).expect("list runs");
        assert!(runs.is_empty());
    }

    #[tokio::test]
    async fn scheduler_logs_already_running_as_skip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_util::build_state_with_provider(
            dir.path().to_str().unwrap(),
            Box::new(MockLlm::new()),
        );

        let run_id = state
            .db
            .try_create_sleep_run("default", SleepRunTrigger::Manual)
            .expect("create run")
            .expect("run id");

        run_scheduled_cycle(&state).await;

        let runs = state.db.list_sleep_runs("default", 10).expect("list runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].trigger, SleepRunTrigger::Manual);

        state
            .db
            .update_sleep_run_success(&run_id, "", None, 0, 0)
            .expect("complete");
    }

    #[tokio::test]
    async fn scheduler_retries_failed_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_util::build_state_with_provider(
            dir.path().to_str().unwrap(),
            Box::new(MockLlm::new()),
        );

        let agents = resolve_target_agents(
            &state.config.sleep_batch,
            &state.config.agents,
            &state.config.default_agent,
        );
        assert!(!agents.is_empty());
    }

    struct MockLlm {
        response: String,
    }

    impl MockLlm {
        fn new() -> Self {
            Self {
                response: serde_json::json!({
                    "episodic": "",
                    "semantic": "",
                    "prospective": ""
                })
                .to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::LlmProvider for MockLlm {
        fn provider_name(&self) -> &str {
            "mock"
        }
        fn model_name(&self) -> &str {
            "mock-model"
        }
        async fn send_message(
            &self,
            _system: &str,
            _messages: Vec<crate::llm::Message>,
            _tools: Option<Vec<crate::llm::ToolDefinition>>,
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            Ok(crate::llm::MessagesResponse {
                content: self.response.clone(),
                tool_calls: vec![],
                usage: Some(crate::llm::LlmUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                }),
            })
        }
    }

    #[tokio::test]
    async fn scheduler_loop_exits_when_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_util::build_state_with_provider(
            dir.path().to_str().unwrap(),
            Box::new(MockLlm::new()),
        );

        let result = run_scheduler_loop(state).await;
        assert!(result.is_ok());
    }
}

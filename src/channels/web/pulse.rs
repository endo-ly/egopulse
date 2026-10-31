//! Pulse API endpoints: an agent's intentions with their next firing, and the
//! run history for auditing.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::Utc;
use serde::Deserialize;

use super::agents::resolved_agent_id;
use super::{
    ApiError, ApiResult, DEFAULT_RUN_LIMIT, MAX_RUN_LIMIT, WebState, internal_error, not_found,
    parse_statuses,
};
use crate::pulse::capsule::{HomeSurface, resolve_home_surface};
use crate::pulse::definition::{
    DeliverySpec, TemporalIntention, TemporalSchedule, load_pulse_definition,
};
use crate::pulse::next_due::{NextDue, resolve_next_due};
use crate::storage::{PulseRun, PulseRunStatus, call_blocking};

/// Runs per intention shown as its recent results.
const RECENT_RUNS_PER_INTENTION: usize = 10;

#[derive(Deserialize)]
pub(super) struct PulseRunsQuery {
    agent_id: String,
    /// Comma-separated statuses; every status when absent.
    status: Option<String>,
    limit: Option<usize>,
}

/// Returns the agent's Pulse definition: the agent-level switch, the default
/// delivery, and each intention with its next firing, the conversation it
/// would be delivered to now, and recent runs.
///
/// The delivery conversation (`target`) is resolved the same way the
/// scheduler resolves it at firing time; it is null when nothing resolves, in
/// which case a firing is skipped.
///
/// A `PULSE.yaml` that fails to parse is reported in `error` with no
/// intentions, so the screen can show what the scheduler is skipping.
///
/// # Errors
///
/// Returns `404` for an unknown agent and `500` on database errors.
pub(super) async fn get_agent_pulse(
    State(state): State<WebState>,
    Path(agent_id): Path<String>,
) -> ApiResult {
    let agent_id =
        resolved_agent_id(&state, &agent_id).ok_or_else(|| not_found("agent_not_found"))?;
    let timezone = state
        .app_state
        .config_manager
        .current_blocking()
        .config
        .timezone
        .clone();
    let state_root = PathBuf::from(&state.app_state.config.state_root);
    let agent = agent_id.as_str().to_string();
    let definition =
        tokio::task::spawn_blocking(move || load_pulse_definition(&state_root, &agent))
            .await
            .map_err(|error| {
                tracing::warn!(%error, "pulse definition task panicked");
                internal_error()
            })?;
    let definition = match definition {
        Ok(definition) => definition,
        Err(error) => {
            return Ok(Json(serde_json::json!({
                "ok": true,
                "pulse": {
                    "enabled": null,
                    "error": error.to_string(),
                    "timezone": timezone,
                    "default_delivery": null,
                    "intentions": [],
                }
            })));
        }
    };

    let now = Utc::now();
    let available_channels = state.app_state.channels.names();
    let mut intentions = Vec::with_capacity(definition.intentions.len());
    for intention in &definition.intentions {
        let target = resolve_home_surface(
            &state.app_state.db,
            agent_id.as_str(),
            &available_channels,
            intention
                .delivery
                .as_ref()
                .or(definition.default_delivery.as_ref()),
        )
        .await
        .map_err(storage_error)?;
        let next = if definition.enabled && intention.enabled {
            let next = resolve_next_due(
                &state.app_state.db,
                agent_id.as_str(),
                intention,
                &timezone,
                now,
            )
            .await
            .map_err(storage_error)?;
            Some(next_due_json(next))
        } else {
            None
        };
        let recent_runs = {
            let agent = agent_id.as_str().to_string();
            let intention_id = intention.id.clone();
            call_blocking(Arc::clone(&state.app_state.db), move |db| {
                db.list_pulse_runs(&agent, Some(&intention_id), &[], RECENT_RUNS_PER_INTENTION)
            })
            .await
            .map_err(storage_error)?
        };
        intentions.push(intention_json(
            intention,
            next,
            target.as_ref(),
            &recent_runs,
        ));
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "pulse": {
            "enabled": definition.enabled,
            "error": null,
            "timezone": timezone,
            "default_delivery": definition.default_delivery.as_ref().map(delivery_json),
            "intentions": intentions,
        }
    })))
}

/// Lists an agent's pulse runs, newest first.
///
/// Query parameters:
/// - `agent_id`: the agent whose runs to list
/// - `status` (optional): comma-separated statuses to keep
/// - `limit` (optional, default 20, at most 200): maximum number of runs
///
/// # Errors
///
/// Returns `400` for an unknown status and `500` on database errors.
pub(super) async fn list_pulse_runs(
    State(state): State<WebState>,
    Query(query): Query<PulseRunsQuery>,
) -> ApiResult {
    let statuses = parse_statuses::<PulseRunStatus>(query.status.as_deref())?;
    let limit = query.limit.unwrap_or(DEFAULT_RUN_LIMIT).min(MAX_RUN_LIMIT);
    let agent_id = query.agent_id;
    let runs = call_blocking(Arc::clone(&state.app_state.db), move |db| {
        db.list_pulse_runs(&agent_id, None, &statuses, limit)
    })
    .await
    .map_err(storage_error)?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "runs": runs.iter().map(pulse_run_json).collect::<Vec<_>>(),
    })))
}

/// Returns one pulse run with the attention it was activated with and, for
/// a delivered notification, the session it was delivered to.
///
/// # Errors
///
/// Returns `404` when the run does not exist and `500` on database errors.
pub(super) async fn get_pulse_run_detail(
    State(state): State<WebState>,
    Path(run_id): Path<String>,
) -> ApiResult {
    let run = call_blocking(Arc::clone(&state.app_state.db), move |db| {
        db.get_pulse_run(&run_id)
    })
    .await
    .map_err(storage_error)?
    .ok_or_else(|| not_found("not_found"))?;

    let chat = match run.chat_id {
        Some(chat_id) => call_blocking(Arc::clone(&state.app_state.db), move |db| {
            db.get_chat_by_id(chat_id)
        })
        .await
        .map_err(storage_error)?,
        None => None,
    };

    let mut run_json = pulse_run_json(&run);
    run_json["attention_snapshot"] = serde_json::json!(run.attention_snapshot);
    run_json["delivery"] = serde_json::json!(chat.map(|chat| serde_json::json!({
        "channel": chat.channel,
        "external_chat_id": chat.external_chat_id,
        "session_key": session_key(chat.chat_id),
    })));

    Ok(Json(serde_json::json!({"ok": true, "run": run_json})))
}

fn pulse_run_json(run: &PulseRun) -> serde_json::Value {
    serde_json::json!({
        "id": run.id,
        "agent_id": run.agent_id,
        "intention_id": run.intention_id,
        "status": run.status.to_string(),
        "started_at": run.started_at,
        "finished_at": run.finished_at,
        "output_kind": run.output_kind.map(|kind| kind.to_string()),
        "output_text": run.output_text,
        "error_message": run.error_message,
    })
}

/// The WebUI's key for the session of a chat.
fn session_key(chat_id: i64) -> String {
    format!("chat:{chat_id}")
}

fn intention_json(
    intention: &TemporalIntention,
    next: Option<serde_json::Value>,
    target: Option<&HomeSurface>,
    recent_runs: &[PulseRun],
) -> serde_json::Value {
    serde_json::json!({
        "id": intention.id,
        "enabled": intention.enabled,
        "schedule": schedule_json(&intention.schedule),
        "attention": intention.attention,
        "delivery": intention.delivery.as_ref().map(delivery_json),
        "target": target.map(|surface| serde_json::json!({
            "channel": surface.channel,
            "session_key": session_key(surface.chat_id),
        })),
        "next": next,
        "recent_runs": recent_runs
            .iter()
            .map(|run| serde_json::json!({
                "id": run.id,
                "status": run.status.to_string(),
                "output_kind": run.output_kind.map(|kind| kind.to_string()),
                "started_at": run.started_at,
            }))
            .collect::<Vec<_>>(),
    })
}

fn schedule_json(schedule: &TemporalSchedule) -> serde_json::Value {
    match schedule {
        TemporalSchedule::Daily { at } => serde_json::json!({"kind": "daily", "at": at}),
        TemporalSchedule::Weekly { day, at } => {
            serde_json::json!({"kind": "weekly", "day": day, "at": at})
        }
        TemporalSchedule::Interval { interval_days, at } => {
            serde_json::json!({"kind": "interval", "interval_days": interval_days, "at": at})
        }
        TemporalSchedule::Once { date, at } => {
            serde_json::json!({"kind": "once", "date": date, "at": at})
        }
    }
}

fn delivery_json(delivery: &DeliverySpec) -> serde_json::Value {
    serde_json::json!({
        "channel": delivery.channel,
        "external_chat_id": delivery.external_chat_id,
    })
}

fn next_due_json(next: NextDue) -> serde_json::Value {
    let (state, at) = match next {
        NextDue::DueNow => ("due_now", None),
        NextDue::At(instant) => ("scheduled", Some(instant.to_rfc3339())),
        NextDue::AttemptedThisPeriod => ("attempted_this_period", None),
        NextDue::AttemptedOnce => ("attempted_once", None),
        NextDue::NoUpcoming => ("no_upcoming", None),
    };
    serde_json::json!({"state": state, "at": at})
}

fn storage_error(error: crate::error::StorageError) -> ApiError {
    tracing::warn!(%error, "pulse api storage failure");
    internal_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State as AxumState;
    use axum::http::StatusCode;

    use async_trait::async_trait;

    use crate::channels::adapter::{ChannelAdapter, ChannelRegistry, ConversationKind};
    use crate::storage::PulseOutputKind;

    /// A sendable channel, so delivery targets on it can resolve.
    struct StubAdapter(&'static str);

    #[async_trait]
    impl ChannelAdapter for StubAdapter {
        fn name(&self) -> &str {
            self.0
        }
        fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
            Vec::new()
        }
        async fn send_text(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn web_state(dir: &tempfile::TempDir) -> WebState {
        let state_root = dir.path().to_string_lossy().to_string();
        let config = crate::test_util::test_config(&state_root);
        let mut channels = ChannelRegistry::new();
        channels.register(Arc::new(StubAdapter("discord")));
        let app_state = crate::test_util::build_state_with_config(
            config,
            None,
            None,
            None,
            Some(Arc::new(channels)),
        );
        WebState {
            app_state: Arc::new(app_state),
            config_path: None,
            run_hub: super::super::RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn write_pulse(dir: &tempfile::TempDir, yaml: &str) {
        let path = dir.path().join("agents/default/PULSE.yaml");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, yaml).expect("write PULSE.yaml");
    }

    fn runs_query(status: Option<&str>) -> Query<PulseRunsQuery> {
        Query(PulseRunsQuery {
            agent_id: "default".to_string(),
            status: status.map(str::to_string),
            limit: None,
        })
    }

    #[tokio::test]
    async fn agent_pulse_lists_intentions_with_next_firing_target_and_recent_runs() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        write_pulse(
            &dir,
            r#"
enabled: true
default_delivery:
  channel: discord
  external_chat_id: "123"
intentions:
  - id: morning
    schedule: { kind: weekly, day: mon, at: "09:00" }
    attention: Check the calendar.
  - id: paused
    enabled: false
    schedule: { kind: daily, at: "21:00" }
    attention: Reflect.
    delivery: { channel: telegram, external_chat_id: "456" }
"#,
        );
        let state = web_state(&dir);
        let chat_id = state
            .app_state
            .db
            .resolve_or_create_chat_id("discord", "discord:123:agent:default", "dm", "default")
            .expect("chat");
        state
            .app_state
            .db
            .try_create_pulse_run(
                "run-1",
                "default",
                "morning",
                "default:morning:2025-W01",
                "x",
            )
            .expect("run");

        // Act
        let body = get_agent_pulse(AxumState(state), Path("default".to_string()))
            .await
            .expect("ok")
            .0;

        // Assert
        let pulse = &body["pulse"];
        assert_eq!(pulse["enabled"], true);
        assert_eq!(pulse["error"], serde_json::Value::Null);
        assert_eq!(pulse["default_delivery"]["channel"], "discord");
        let morning = &pulse["intentions"][0];
        assert_eq!(
            morning["schedule"],
            serde_json::json!({"kind": "weekly", "day": "mon", "at": "09:00"})
        );
        assert_eq!(morning["delivery"], serde_json::Value::Null);
        assert_eq!(
            morning["target"],
            serde_json::json!({"channel": "discord", "session_key": format!("chat:{chat_id}")})
        );
        assert_eq!(morning["next"]["state"], "scheduled");
        assert!(morning["next"]["at"].is_string());
        assert_eq!(morning["recent_runs"][0]["id"], "run-1");
        assert_eq!(morning["recent_runs"][0]["status"], "running");
        let paused = &pulse["intentions"][1];
        assert_eq!(paused["enabled"], false);
        assert_eq!(paused["next"], serde_json::Value::Null);
        assert_eq!(paused["delivery"]["external_chat_id"], "456");
        assert_eq!(paused["target"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn agent_pulse_reports_a_definition_that_fails_to_parse() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        write_pulse(&dir, "intentions: [ {id: broken");
        let state = web_state(&dir);

        // Act
        let body = get_agent_pulse(AxumState(state), Path("default".to_string()))
            .await
            .expect("ok")
            .0;

        // Assert
        let pulse = &body["pulse"];
        assert_eq!(pulse["enabled"], serde_json::Value::Null);
        assert!(
            pulse["error"]
                .as_str()
                .is_some_and(|error| error.starts_with("pulse_parse_error"))
        );
        assert_eq!(pulse["intentions"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn pulse_runs_filter_by_status() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);
        let db = &state.app_state.db;
        db.try_create_pulse_run("run-ok", "default", "morning", "k1", "x")
            .expect("run");
        db.update_pulse_run_success("run-ok", None, None, PulseOutputKind::Silent, "PULSE_OK")
            .expect("success");
        db.try_create_pulse_run("run-skip", "default", "morning", "k2", "x")
            .expect("run");
        db.update_pulse_run_skipped("run-skip", "no sendable home surface")
            .expect("skip");

        // Act
        let body = list_pulse_runs(AxumState(state), runs_query(Some("skipped")))
            .await
            .expect("ok")
            .0;

        // Assert
        let runs = body["runs"].as_array().expect("runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["id"], "run-skip");
        assert_eq!(runs[0]["error_message"], "no sendable home surface");
    }

    #[tokio::test]
    async fn pulse_run_detail_includes_attention_and_delivered_session() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);
        let db = &state.app_state.db;
        let chat_id = db
            .resolve_or_create_chat_id("discord", "discord:123", "dm", "default")
            .expect("chat");
        db.try_create_pulse_run("run-1", "default", "morning", "k1", "Check the calendar.")
            .expect("run");
        db.update_pulse_run_success(
            "run-1",
            Some(chat_id),
            Some("msg-1"),
            PulseOutputKind::Notify,
            "Two meetings today.",
        )
        .expect("success");

        // Act
        let body = get_pulse_run_detail(AxumState(state), Path("run-1".to_string()))
            .await
            .expect("ok")
            .0;

        // Assert
        let run = &body["run"];
        assert_eq!(run["attention_snapshot"], "Check the calendar.");
        assert_eq!(run["output_kind"], "notify");
        assert_eq!(run["output_text"], "Two meetings today.");
        assert_eq!(run["delivery"]["channel"], "discord");
        assert_eq!(run["delivery"]["session_key"], format!("chat:{chat_id}"));
    }

    #[tokio::test]
    async fn pulse_run_detail_returns_404_for_a_missing_run() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = web_state(&dir);

        // Act
        let result = get_pulse_run_detail(AxumState(state), Path("nope".to_string())).await;

        // Assert
        let (status, _) = result.expect_err("not found");
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

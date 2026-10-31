//! Sleep batch audit API endpoints.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::Utc;
use serde::Deserialize;

use crate::memory::MemoryError;
use crate::sleep::SKIP_THRESHOLD;
use crate::sleep::scheduler::{next_scheduled_run, resolve_target_agents};
use crate::storage::{MemoryFile, SleepRun, SleepRunStatus, SleepStepName, call_blocking};

use super::agents::resolved_agent_id;
use super::{
    ApiResult, DEFAULT_RUN_LIMIT, MAX_RUN_LIMIT, WebState, bad_request, internal_error, not_found,
    parse_before, parse_statuses,
};

#[derive(Deserialize)]
pub(super) struct SleepRunsQuery {
    agent_id: String,
    /// Comma-separated statuses; every status when absent.
    status: Option<String>,
    /// Only runs started before this instant, for paging back through history.
    before: Option<String>,
    limit: Option<usize>,
}

/// Lists an agent's sleep runs, newest first.
///
/// Query parameters:
/// - `agent_id`: the agent whose runs to list
/// - `status` (optional): comma-separated statuses to keep
/// - `before` (optional): only runs started before this RFC 3339 instant, so
///   the caller can page back through the whole history
/// - `limit` (optional, default 20, at most 200): maximum number of runs
///
/// `has_more` says whether older runs exist beyond the returned page. Each run
/// carries `changed_memory`: the memory files it published changes to (see
/// [`crate::storage::Database::changed_memory_files`]).
///
/// # Errors
///
/// Returns `400` for an unknown status or `before` value and `500` on database
/// errors.
pub(super) async fn list_sleep_runs(
    State(state): State<WebState>,
    Query(query): Query<SleepRunsQuery>,
) -> ApiResult {
    let statuses = parse_statuses::<SleepRunStatus>(query.status.as_deref())?;
    let before = parse_before(query.before.as_deref())?;
    let limit = query.limit.unwrap_or(DEFAULT_RUN_LIMIT).min(MAX_RUN_LIMIT);
    let db = Arc::clone(&state.app_state.db);
    let agent_id = query.agent_id;

    let (runs, changed, has_more) = call_blocking(db, move |db| {
        // One row past the page tells us whether older runs exist.
        let mut runs = db.list_sleep_runs(&agent_id, &statuses, before.as_deref(), limit + 1)?;
        let has_more = runs.len() > limit;
        runs.truncate(limit);
        let run_ids: Vec<String> = runs.iter().map(|run| run.id.clone()).collect();
        let changed = db.changed_memory_files(&run_ids)?;
        Ok((runs, changed, has_more))
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "failed to list sleep runs");
        internal_error()
    })?;

    let runs_json: Vec<serde_json::Value> = runs
        .into_iter()
        .map(|run| {
            let changed_memory = changed.get(&run.id).map(Vec::as_slice).unwrap_or(&[]);
            sleep_run_json(&run, changed_memory)
        })
        .collect();

    Ok(Json(
        serde_json::json!({"ok": true, "runs": runs_json, "has_more": has_more}),
    ))
}

/// Describes the agent's next scheduled sleep check and the condition it
/// will decide on.
///
/// `next_check_at` is null when the scheduler is disabled or the agent is not
/// one of its targets. Pending messages above `threshold` make the check run.
///
/// # Errors
///
/// Returns `404` for an unknown agent and `500` on database errors.
pub(super) async fn get_agent_sleep_outlook(
    State(state): State<WebState>,
    Path(agent_id): Path<String>,
) -> ApiResult {
    let agent_id =
        resolved_agent_id(&state, &agent_id).ok_or_else(|| not_found("agent_not_found"))?;
    let snapshot = state.app_state.config_manager.current_blocking();
    let config = &snapshot.config;
    let next_check = next_scheduled_run(&config.sleep_batch, &config.timezone, Utc::now());
    let targeted =
        resolve_target_agents(&config.sleep_batch, &config.agents, &config.default_agent)
            .contains(&agent_id);

    let db = Arc::clone(&state.app_state.db);
    let pending = call_blocking(db, move |db| {
        db.count_agent_pending_sleep_messages(agent_id.as_str())
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "failed to count pending sleep messages");
        internal_error()
    })?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "outlook": {
            "scheduler_enabled": next_check.is_some(),
            "targeted": targeted,
            "next_check_at": next_check
                .filter(|_| targeted)
                .map(|instant| instant.to_rfc3339()),
            "pending_messages": pending,
            "threshold": SKIP_THRESHOLD,
        }
    })))
}

/// A sleep run as the API returns it, with the memory files it changed.
fn sleep_run_json(run: &SleepRun, changed_memory: &[MemoryFile]) -> serde_json::Value {
    serde_json::json!({
        "id": run.id,
        "agent_id": run.agent_id,
        "status": run.status.to_string(),
        "trigger": run.trigger.to_string(),
        "started_at": run.started_at,
        "finished_at": run.finished_at,
        "source_chats_json": run.source_chats_json,
        "source_digest_md": run.source_digest_md,
        "input_tokens": run.input_tokens,
        "output_tokens": run.output_tokens,
        "total_tokens": run.total_tokens,
        "error_message": run.error_message,
        "session_count": parse_session_count(&run.source_chats_json),
        "changed_memory": changed_memory
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    })
}

fn parse_session_count(source_chats_json: &str) -> usize {
    serde_json::from_str::<Vec<serde_json::Value>>(source_chats_json)
        .map(|v| v.len())
        .unwrap_or(0)
}

/// Gets a single sleep run with its step results and memory snapshots.
///
/// The `run` object has the same shape as a run in the listing, so the
/// screen can show the detail of a run it has not listed.
///
/// # Path parameters
/// - `run_id`: the sleep run identifier
///
/// # Errors
///
/// Returns `404` when the run does not exist.
/// Returns `500` on database errors.
pub(super) async fn get_sleep_run_detail(
    State(state): State<WebState>,
    Path(run_id): Path<String>,
) -> ApiResult {
    let db = Arc::clone(&state.app_state.db);

    // One blocking hop for the run and everything hanging off it: the screen
    // needs all of it, and the queries are independent.
    let detail = call_blocking(db, {
        let run_id = run_id.clone();
        move |db| {
            let Some(run) = db.get_sleep_run(&run_id)? else {
                return Ok(None);
            };
            let changed = db.changed_memory_files(std::slice::from_ref(&run_id))?;
            let snapshots = db.get_snapshots_for_run(&run_id)?;
            let steps = db.list_sleep_run_steps(&run_id)?;
            let changed_memory = changed.get(&run_id).cloned().unwrap_or_default();
            Ok(Some((run, changed_memory, snapshots, steps)))
        }
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, run_id = %run_id, "failed to get sleep run detail");
        internal_error()
    })?;
    let Some((run, changed_memory, snapshots, mut steps)) = detail else {
        return Err(not_found("not_found"));
    };

    // The storage layer orders by step_name (alphabetical); reorder to the
    // pipeline execution order so the UI can render steps top-to-bottom.
    steps.sort_by_key(|step| {
        SleepStepName::ALL
            .iter()
            .position(|name| *name == step.step_name)
            .unwrap_or(usize::MAX)
    });

    let snapshots_json: Vec<serde_json::Value> = snapshots
        .into_iter()
        .map(|snap| {
            serde_json::json!({
                "id": snap.id,
                "run_id": snap.run_id,
                "agent_id": snap.agent_id,
                "file": snap.file.to_string(),
                "content_before": snap.content_before,
                "content_after": snap.content_after,
                "created_at": snap.created_at,
            })
        })
        .collect();

    let steps_json: Vec<serde_json::Value> = steps
        .into_iter()
        .map(|step| {
            serde_json::json!({
                "step": step.step_name.to_string(),
                "status": step.status.to_string(),
                "started_at": step.started_at,
                "finished_at": step.finished_at,
                "input_tokens": step.input_tokens,
                "output_tokens": step.output_tokens,
                "error_message": step.error_message,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "ok": true,
        "run": sleep_run_json(&run, &changed_memory),
        "snapshots": snapshots_json,
        "steps": steps_json,
    })))
}

/// Gets the current published long-term memory bundle for an agent.
///
/// # Path parameters
/// - `agent_id`: the agent identifier
///
/// # Errors
///
/// Returns `400` when the agent id is unsafe (e.g. path traversal).
/// Returns `500` on filesystem errors.
pub(super) async fn get_agent_memory(
    State(state): State<WebState>,
    Path(agent_id): Path<String>,
) -> ApiResult {
    let loader = Arc::clone(&state.app_state.memory_loader);
    let lookup_agent_id = agent_id.clone();
    let bundle =
        match tokio::task::spawn_blocking(move || loader.load_bundle(&lookup_agent_id)).await {
            Ok(Ok(bundle)) => bundle,
            Ok(Err(MemoryError::UnsafeAgentId(id))) => {
                tracing::warn!(agent_id = %id, "rejected unsafe agent id for memory request");
                return Err(bad_request("invalid_agent_id"));
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, agent_id = %agent_id, "failed to load memory bundle");
                return Err(internal_error());
            }
            Err(error) => {
                tracing::warn!(%error, agent_id = %agent_id, "memory loader task panicked");
                return Err(internal_error());
            }
        };

    Ok(Json(serde_json::json!({
        "ok": true,
        "memory": {
            "episodic": bundle.episodic,
            "semantic": bundle.semantic,
            "prospective": bundle.prospective,
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::extract::State as AxumState;
    use axum::http::StatusCode;
    use std::sync::Arc;

    use crate::channels::web::{RunHub, WebState};
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::storage::{Database, SleepRunTrigger};

    struct DummyLlm;

    #[async_trait]
    impl LlmProvider for DummyLlm {
        fn provider_name(&self) -> &str {
            "dummy"
        }

        fn model_name(&self) -> &str {
            "dummy"
        }

        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<std::sync::Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<MessagesResponse, LlmError> {
            panic!("handler tests should not call LLM")
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<std::sync::Arc<Vec<crate::llm::ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    fn test_web_state(dir: &tempfile::TempDir) -> WebState {
        let state_root = dir.path().to_string_lossy().to_string();
        let app_state =
            crate::test_util::build_state_with_provider(&state_root, Box::new(DummyLlm));
        WebState {
            app_state: Arc::new(app_state),
            config_path: None,
            run_hub: RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn insert_sleep_run(db: &Database, id: &str, agent_id: &str, source_chats_json: &str) {
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT INTO sleep_runs (id, agent_id, status, trigger_type, started_at, source_chats_json)
             VALUES (?1, ?2, 'success', 'manual', '2024-01-01T00:00:00Z', ?3)",
            rusqlite::params![id, agent_id, source_chats_json],
        )
        .expect("insert sleep run");
    }

    fn insert_memory_snapshot(db: &Database, id: &str, run_id: &str, agent_id: &str) {
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT INTO memory_snapshots (id, run_id, agent_id, file, content_before, content_after, created_at)
             VALUES (?1, ?2, ?3, 'episodic', 'before', 'after', '2024-01-01T00:00:00Z')",
            rusqlite::params![id, run_id, agent_id],
        )
        .expect("insert memory snapshot");
    }

    fn insert_sleep_step(
        db: &Database,
        run_id: &str,
        step_name: &str,
        status: &str,
        error_message: Option<&str>,
    ) {
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT INTO sleep_run_steps (sleep_run_id, step_name, status, error_message)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![run_id, step_name, status, error_message],
        )
        .expect("insert sleep step");
    }

    fn write_memory_file(dir: &std::path::Path, agent_id: &str, file_name: &str, content: &str) {
        let path = dir
            .join("agents")
            .join(agent_id)
            .join("memory")
            .join(file_name);
        std::fs::create_dir_all(path.parent().expect("memory dir parent")).expect("mkdir");
        std::fs::write(path, content).expect("write memory file");
    }

    fn runs_query(
        agent_id: &str,
        status: Option<&str>,
        limit: Option<usize>,
    ) -> Query<SleepRunsQuery> {
        Query(SleepRunsQuery {
            agent_id: agent_id.to_string(),
            status: status.map(str::to_string),
            before: None,
            limit,
        })
    }

    fn paged_runs_query(agent_id: &str, before: &str) -> Query<SleepRunsQuery> {
        Query(SleepRunsQuery {
            agent_id: agent_id.to_string(),
            status: None,
            before: Some(before.to_string()),
            limit: None,
        })
    }

    #[tokio::test]
    async fn api_sleep_runs_returns_the_agents_runs_with_session_count_and_changed_memory() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        insert_sleep_run(
            &web_state.app_state.db,
            "run-1",
            "agent-a",
            r#"[{"chat_id": 1}, {"chat_id": 2}]"#,
        );
        insert_memory_snapshot(&web_state.app_state.db, "snap-1", "run-1", "agent-a");
        insert_sleep_run(&web_state.app_state.db, "run-b", "agent-b", "[]");

        // Act
        let body = list_sleep_runs(AxumState(web_state), runs_query("agent-a", None, None))
            .await
            .expect("ok")
            .0;

        // Assert
        let runs = body["runs"].as_array().expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["id"], "run-1");
        assert_eq!(runs[0]["agent_id"], "agent-a");
        assert_eq!(runs[0]["session_count"], 2);
        assert_eq!(runs[0]["changed_memory"], serde_json::json!(["episodic"]));
    }

    #[tokio::test]
    async fn api_sleep_runs_respects_limit() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        for id in ["run-1", "run-2", "run-3"] {
            insert_sleep_run(&web_state.app_state.db, id, "agent-a", "[]");
        }

        // Act
        let body = list_sleep_runs(AxumState(web_state), runs_query("agent-a", None, Some(1)))
            .await
            .expect("ok")
            .0;

        // Assert
        assert_eq!(body["runs"].as_array().expect("runs array").len(), 1);
    }

    #[tokio::test]
    async fn api_sleep_runs_reports_whether_older_runs_exist() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        insert_sleep_run(&web_state.app_state.db, "run-1", "agent-a", "[]");
        insert_sleep_run(&web_state.app_state.db, "run-2", "agent-a", "[]");

        // Act
        let page = list_sleep_runs(
            AxumState(web_state.clone()),
            runs_query("agent-a", None, Some(1)),
        )
        .await
        .expect("ok")
        .0;
        let all = list_sleep_runs(AxumState(web_state), runs_query("agent-a", None, None))
            .await
            .expect("ok")
            .0;

        // Assert
        assert_eq!(page["runs"].as_array().expect("runs").len(), 1);
        assert_eq!(page["has_more"], serde_json::json!(true));
        assert_eq!(all["has_more"], serde_json::json!(false));
    }

    #[tokio::test]
    async fn api_sleep_runs_pages_back_from_the_before_cursor() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        insert_sleep_run(&web_state.app_state.db, "run-1", "agent-a", "[]");
        let conn = web_state.app_state.db.get_conn().expect("pool");
        conn.execute(
            "UPDATE sleep_runs SET started_at = '2024-01-02T00:00:00+00:00' WHERE id = 'run-1'",
            [],
        )
        .expect("pin started_at");

        // Act
        let older = list_sleep_runs(
            AxumState(web_state.clone()),
            paged_runs_query("agent-a", "2024-01-03T00:00:00+00:00"),
        )
        .await
        .expect("ok")
        .0;
        let not_older = list_sleep_runs(
            AxumState(web_state),
            paged_runs_query("agent-a", "2024-01-02T00:00:00+00:00"),
        )
        .await
        .expect("ok")
        .0;

        // Assert
        assert_eq!(older["runs"][0]["id"], "run-1");
        assert!(not_older["runs"].as_array().expect("runs").is_empty());
    }

    #[tokio::test]
    async fn api_sleep_runs_rejects_an_unparseable_before_cursor() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        // Act
        let result = list_sleep_runs(
            AxumState(web_state),
            paged_runs_query("agent-a", "yesterday"),
        )
        .await;

        // Assert
        let (status, _) = result.expect_err("bad request");
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn api_sleep_runs_filters_by_status_list() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        insert_sleep_run(&web_state.app_state.db, "run-ok", "agent-a", "[]");
        let skipped = web_state
            .app_state
            .db
            .record_skipped_sleep_run("agent-a", SleepRunTrigger::Scheduled, "agent was active")
            .expect("record skip");

        // Act
        let body = list_sleep_runs(
            AxumState(web_state),
            runs_query("agent-a", Some("skipped,failed"), None),
        )
        .await
        .expect("ok")
        .0;

        // Assert
        let runs = body["runs"].as_array().expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["id"], skipped.as_str());
        assert_eq!(runs[0]["error_message"], "agent was active");
    }

    #[tokio::test]
    async fn api_sleep_runs_rejects_an_unknown_status() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        // Act
        let result = list_sleep_runs(
            AxumState(web_state),
            runs_query("agent-a", Some("finished"), None),
        )
        .await;

        // Assert
        let (status, _) = result.expect_err("bad request");
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn api_sleep_outlook_reports_pending_messages_against_the_threshold() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        // Act
        let body = get_agent_sleep_outlook(AxumState(web_state), Path("default".to_string()))
            .await
            .expect("ok")
            .0;

        // Assert
        let outlook = &body["outlook"];
        assert_eq!(outlook["scheduler_enabled"], false);
        assert_eq!(outlook["targeted"], true);
        assert_eq!(outlook["next_check_at"], serde_json::Value::Null);
        assert_eq!(outlook["pending_messages"], 0);
        assert_eq!(outlook["threshold"], SKIP_THRESHOLD);
    }

    #[tokio::test]
    async fn api_sleep_outlook_rejects_an_unknown_agent() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        // Act
        let result =
            get_agent_sleep_outlook(AxumState(web_state), Path("nobody".to_string())).await;

        // Assert
        let (status, _) = result.expect_err("not found");
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_sleep_run_detail_returns_run_and_snapshots() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(
            &web_state.app_state.db,
            "run-1",
            "agent-a",
            r#"[{"chat_id": 1}]"#,
        );
        insert_memory_snapshot(&web_state.app_state.db, "snap-1", "run-1", "agent-a");

        // Act
        let result = get_sleep_run_detail(AxumState(web_state), Path("run-1".to_string()))
            .await
            .expect("ok");

        // Assert
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        assert_eq!(body["run"]["id"], "run-1");
        assert_eq!(body["run"]["agent_id"], "agent-a");
        assert_eq!(body["run"]["status"], "success");
        assert_eq!(body["run"]["trigger"], "manual");
        // Same shape as a listed run, so the detail can stand alone.
        assert_eq!(body["run"]["session_count"], 1);
        assert_eq!(
            body["run"]["changed_memory"],
            serde_json::json!(["episodic"])
        );

        let snapshots = body["snapshots"].as_array().expect("snapshots array");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["id"], "snap-1");
        assert_eq!(snapshots[0]["run_id"], "run-1");
        assert_eq!(snapshots[0]["content_before"], "before");
        assert_eq!(snapshots[0]["content_after"], "after");
    }

    #[tokio::test]
    async fn api_sleep_run_detail_returns_404_for_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let state = AxumState(web_state);
        let path = Path("nonexistent-run".to_string());
        let result = get_sleep_run_detail(state, path).await;
        assert!(result.is_err());
        let (status, body) = result.unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["ok"], serde_json::json!(false));
        assert_eq!(body["error"], "not_found");
    }

    #[tokio::test]
    async fn api_sleep_run_detail_snapshots_file_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(&web_state.app_state.db, "run-file", "agent-a", "[]");
        insert_memory_snapshot(&web_state.app_state.db, "snap-file", "run-file", "agent-a");

        let state = AxumState(web_state);
        let path = Path("run-file".to_string());
        let result = get_sleep_run_detail(state, path).await.expect("ok");
        let body = result.0;
        let snapshots = body["snapshots"].as_array().expect("snapshots array");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["file"], "episodic");
        assert!(snapshots[0]["file"].is_string());
    }

    #[tokio::test]
    async fn api_sleep_run_detail_returns_steps_in_pipeline_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(&web_state.app_state.db, "run-steps", "agent-a", "[]");
        // Insert in a non-pipeline order to prove the handler reorders.
        insert_sleep_step(
            &web_state.app_state.db,
            "run-steps",
            "semantic_update",
            "failed",
            Some("rate limited"),
        );
        insert_sleep_step(
            &web_state.app_state.db,
            "run-steps",
            "event_extraction",
            "success",
            None,
        );
        insert_sleep_step(
            &web_state.app_state.db,
            "run-steps",
            "prospective_update",
            "skipped",
            None,
        );
        insert_sleep_step(
            &web_state.app_state.db,
            "run-steps",
            "episodic_update",
            "success",
            None,
        );

        let state = AxumState(web_state);
        let path = Path("run-steps".to_string());
        let result = get_sleep_run_detail(state, path).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));

        let steps = body["steps"].as_array().expect("steps array");
        assert_eq!(steps.len(), 4);
        let step_names: Vec<&str> = steps
            .iter()
            .map(|step| step["step"].as_str().expect("step name"))
            .collect();
        assert_eq!(
            step_names,
            vec![
                "event_extraction",
                "episodic_update",
                "semantic_update",
                "prospective_update",
            ]
        );
        assert_eq!(steps[0]["status"], "success");
        assert_eq!(steps[2]["status"], "failed");
        assert_eq!(steps[2]["error_message"], "rate limited");
        assert_eq!(steps[3]["status"], "skipped");
    }

    #[tokio::test]
    async fn api_agent_memory_returns_published_bundle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        write_memory_file(
            dir.path(),
            "agent-a",
            "episodic.md",
            "# Episodic\n- entry\n",
        );
        write_memory_file(dir.path(), "agent-a", "semantic.md", "# Semantic\n");

        let state = AxumState(web_state);
        let path = Path("agent-a".to_string());
        let result = get_agent_memory(state, path).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        assert_eq!(body["memory"]["episodic"], "# Episodic\n- entry\n");
        assert_eq!(body["memory"]["semantic"], "# Semantic\n");
        assert_eq!(body["memory"]["prospective"], "");
    }

    #[tokio::test]
    async fn api_agent_memory_returns_empty_bundle_without_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let state = AxumState(web_state);
        let path = Path("fresh-agent".to_string());
        let result = get_agent_memory(state, path).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        assert_eq!(body["memory"]["episodic"], "");
        assert_eq!(body["memory"]["semantic"], "");
        assert_eq!(body["memory"]["prospective"], "");
    }

    #[tokio::test]
    async fn api_agent_memory_rejects_unsafe_agent_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let state = AxumState(web_state);
        let path = Path("../escape".to_string());
        let result = get_agent_memory(state, path).await;
        assert!(result.is_err());
        let (status, body) = result.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["ok"], serde_json::json!(false));
        assert_eq!(body["error"], "invalid_agent_id");
    }

    #[tokio::test]
    async fn sleep_runs_api_returns_partial_failure_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let conn = web_state.app_state.db.get_conn().expect("pool");
        conn.execute(
            "INSERT INTO sleep_runs (id, agent_id, status, trigger_type, started_at, finished_at)
             VALUES ('run-pf', 'agent-a', 'partial_failure', 'manual', '2024-01-01T00:00:00Z', '2024-01-01T00:01:00Z')",
            [],
        )
        .expect("insert partial_failure run");

        let state = AxumState(web_state);
        let result = list_sleep_runs(state, runs_query("agent-a", None, None))
            .await
            .expect("ok");
        let body = result.0;
        let runs = body["runs"].as_array().expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["status"], "partial_failure");
    }
}

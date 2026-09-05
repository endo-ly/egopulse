//! Sleep batch audit API endpoints.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use std::sync::Arc;

use crate::memory::MemoryError;
use crate::storage::{SleepStepName, call_blocking};

use super::WebState;

const DEFAULT_LIMIT: i64 = 20;
const DEFAULT_OFFSET: i64 = 0;

/// Lists sleep runs, optionally filtered by agent_id.
///
/// Query parameters:
/// - `agent_id` (optional): filter runs to a specific agent
/// - `limit` (optional, default 20): maximum number of runs to return
/// - `offset` (optional, default 0): number of runs to skip (pagination)
pub(super) async fn list_sleep_runs(
    State(state): State<WebState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let db = Arc::clone(&state.app_state.db);
    let agent_id = params.get("agent_id").map(|s| s.to_string());
    let limit: i64 = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_LIMIT);
    let offset: i64 = params
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_OFFSET);

    let runs = match call_blocking(db, move |db| {
        if let Some(ref agent_id) = agent_id {
            db.list_sleep_runs(agent_id, limit, offset)
        } else {
            db.list_all_sleep_runs(limit, offset)
        }
    })
    .await
    {
        Ok(runs) => runs,
        Err(error) => {
            tracing::warn!(%error, "failed to list sleep runs");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": error.to_string()})),
            ));
        }
    };

    let runs_json: Vec<serde_json::Value> = runs
        .into_iter()
        .map(|run| {
            let session_count = parse_session_count(&run.source_chats_json);
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
                "session_count": session_count,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({"ok": true, "runs": runs_json})))
}

fn parse_session_count(source_chats_json: &str) -> usize {
    serde_json::from_str::<Vec<serde_json::Value>>(source_chats_json)
        .map(|v| v.len())
        .unwrap_or(0)
}

/// Gets a single sleep run with its step results and memory snapshots.
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
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let db = Arc::clone(&state.app_state.db);

    let run = match call_blocking(Arc::clone(&db), {
        let run_id = run_id.clone();
        move |db| db.get_sleep_run(&run_id)
    })
    .await
    {
        Ok(Some(run)) => run,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"ok": false, "error": "not_found"})),
            ));
        }
        Err(error) => {
            tracing::warn!(%error, run_id = %run_id, "failed to get sleep run");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": error.to_string()})),
            ));
        }
    };

    let snapshots = match call_blocking(Arc::clone(&db), {
        let run_id = run_id.clone();
        move |db| db.get_snapshots_for_run(&run_id)
    })
    .await
    {
        Ok(snapshots) => snapshots,
        Err(error) => {
            tracing::warn!(%error, run_id = %run_id, "failed to get snapshots for run");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": error.to_string()})),
            ));
        }
    };

    let mut steps = match call_blocking(db, {
        let run_id = run_id.clone();
        move |db| db.list_sleep_run_steps(&run_id)
    })
    .await
    {
        Ok(steps) => steps,
        Err(error) => {
            tracing::warn!(%error, run_id = %run_id, "failed to get steps for run");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": error.to_string()})),
            ));
        }
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

    let run_json = serde_json::json!({
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
    });

    Ok(Json(serde_json::json!({
        "ok": true,
        "run": run_json,
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
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let loader = Arc::clone(&state.app_state.memory_loader);
    let lookup_agent_id = agent_id.clone();
    let bundle =
        match tokio::task::spawn_blocking(move || loader.load_bundle(&lookup_agent_id)).await {
            Ok(Ok(bundle)) => bundle,
            Ok(Err(MemoryError::UnsafeAgentId(id))) => {
                tracing::warn!(agent_id = %id, "rejected unsafe agent id for memory request");
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"ok": false, "error": "invalid_agent_id"})),
                ));
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, agent_id = %agent_id, "failed to load memory bundle");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"ok": false, "error": error.to_string()})),
                ));
            }
            Err(error) => {
                tracing::warn!(%error, agent_id = %agent_id, "memory loader task panicked");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"ok": false, "error": error.to_string()})),
                ));
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
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::channels::web::{RunHub, WebState};
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::storage::Database;

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

    #[tokio::test]
    async fn api_sleep_runs_returns_runs_with_session_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(
            &web_state.app_state.db,
            "run-1",
            "agent-a",
            r#"[{"chat_id": 1}, {"chat_id": 2}]"#,
        );
        insert_sleep_run(
            &web_state.app_state.db,
            "run-2",
            "agent-b",
            r#"[{"chat_id": 3}]"#,
        );

        let state = AxumState(web_state);
        let query = Query(HashMap::new());
        let result = list_sleep_runs(state, query).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        let runs = body["runs"].as_array().expect("runs array");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[1]["id"], "run-1");
        assert_eq!(runs[1]["session_count"], 2);
        assert_eq!(runs[0]["id"], "run-2");
        assert_eq!(runs[0]["session_count"], 1);
    }

    #[tokio::test]
    async fn api_sleep_runs_filters_by_agent_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(&web_state.app_state.db, "run-a", "agent-a", "[]");
        insert_sleep_run(&web_state.app_state.db, "run-b", "agent-b", "[]");

        let state = AxumState(web_state);
        let query = Query(HashMap::from([(
            "agent_id".to_string(),
            "agent-a".to_string(),
        )]));
        let result = list_sleep_runs(state, query).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        let runs = body["runs"].as_array().expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["id"], "run-a");
        assert_eq!(runs[0]["agent_id"], "agent-a");
    }

    #[tokio::test]
    async fn api_sleep_runs_respects_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(&web_state.app_state.db, "run-1", "agent-a", "[]");
        insert_sleep_run(&web_state.app_state.db, "run-2", "agent-a", "[]");
        insert_sleep_run(&web_state.app_state.db, "run-3", "agent-a", "[]");

        let state = AxumState(web_state);
        let query = Query(HashMap::from([("limit".to_string(), "1".to_string())]));
        let result = list_sleep_runs(state, query).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        let runs = body["runs"].as_array().expect("runs array");
        assert_eq!(runs.len(), 1);
    }

    #[tokio::test]
    async fn api_sleep_runs_offset_paginates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(&web_state.app_state.db, "run-1", "agent-a", "[]");
        insert_sleep_run(&web_state.app_state.db, "run-2", "agent-a", "[]");
        insert_sleep_run(&web_state.app_state.db, "run-3", "agent-a", "[]");

        let state = AxumState(web_state);
        let query = Query(HashMap::from([
            ("limit".to_string(), "1".to_string()),
            ("offset".to_string(), "1".to_string()),
        ]));
        let result = list_sleep_runs(state, query).await.expect("ok");
        let body = result.0;
        let runs = body["runs"].as_array().expect("runs array");
        assert_eq!(runs.len(), 1);
        // Newest-first ordering: skipping the first page lands on run-2.
        assert_eq!(runs[0]["id"], "run-2");
    }

    #[tokio::test]
    async fn api_sleep_runs_default_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(&web_state.app_state.db, "run-1", "agent-a", "[]");
        insert_sleep_run(&web_state.app_state.db, "run-2", "agent-a", "[]");

        let state = AxumState(web_state);
        let query = Query(HashMap::new());
        let result = list_sleep_runs(state, query).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        let runs = body["runs"].as_array().expect("runs array");
        assert_eq!(runs.len(), 2);
    }

    #[tokio::test]
    async fn api_sleep_runs_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let state = AxumState(web_state);
        let query = Query(HashMap::new());
        let result = list_sleep_runs(state, query).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        let runs = body["runs"].as_array().expect("runs array");
        assert!(runs.is_empty());
    }

    #[tokio::test]
    async fn api_sleep_run_detail_returns_run_and_snapshots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(
            &web_state.app_state.db,
            "run-1",
            "agent-a",
            r#"[{"chat_id": 1}]"#,
        );
        insert_memory_snapshot(&web_state.app_state.db, "snap-1", "run-1", "agent-a");

        let state = AxumState(web_state);
        let path = Path("run-1".to_string());
        let result = get_sleep_run_detail(state, path).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        assert_eq!(body["run"]["id"], "run-1");
        assert_eq!(body["run"]["agent_id"], "agent-a");
        assert_eq!(body["run"]["status"], "success");
        assert_eq!(body["run"]["trigger"], "manual");

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
        let query = Query(HashMap::from([(
            "agent_id".to_string(),
            "agent-a".to_string(),
        )]));
        let result = list_sleep_runs(state, query).await.expect("ok");
        let body = result.0;
        let runs = body["runs"].as_array().expect("runs array");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["status"], "partial_failure");
    }
}

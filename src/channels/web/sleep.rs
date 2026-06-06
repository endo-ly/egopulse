//! Sleep batch audit API endpoints.
//!
//! Provides REST handlers for listing agents with sleep run records,
//! listing sleep runs, and retrieving individual run details with
//! associated memory snapshots.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use std::sync::Arc;

use crate::storage::call_blocking;

use super::WebState;

const DEFAULT_LIMIT: i64 = 20;

/// Lists distinct agent IDs that have sleep run records.
pub(super) async fn list_agents(
    State(state): State<WebState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let db = Arc::clone(&state.app_state.db);
    match call_blocking(db, |db| db.list_distinct_agent_ids()).await {
        Ok(agents) => Ok(Json(serde_json::json!({"ok": true, "agents": agents}))),
        Err(error) => {
            tracing::warn!(%error, "failed to list distinct agent IDs");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": error.to_string()})),
            ))
        }
    }
}

/// Lists sleep runs, optionally filtered by agent_id.
///
/// Query parameters:
/// - `agent_id` (optional): filter runs to a specific agent
/// - `limit` (optional, default 20): maximum number of runs to return
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

    let runs = match call_blocking(db, move |db| {
        if let Some(ref agent_id) = agent_id {
            db.list_sleep_runs(agent_id, limit)
        } else {
            db.list_all_sleep_runs(limit)
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

/// Gets a single sleep run with its memory snapshots.
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

    let snapshots = match call_blocking(db, {
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

    Ok(Json(
        serde_json::json!({"ok": true, "run": run_json, "snapshots": snapshots_json}),
    ))
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

    #[tokio::test]
    async fn api_agents_returns_distinct_agent_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        insert_sleep_run(&web_state.app_state.db, "run-1", "agent-a", "[]");
        insert_sleep_run(&web_state.app_state.db, "run-2", "agent-a", "[]");
        insert_sleep_run(&web_state.app_state.db, "run-3", "agent-b", "[]");

        let result = list_agents(AxumState(web_state)).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        let agents = body["agents"].as_array().expect("agents array");
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0], "agent-a");
        assert_eq!(agents[1], "agent-b");
    }

    #[tokio::test]
    async fn api_agents_returns_empty_array() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let result = list_agents(AxumState(web_state)).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));
        let agents = body["agents"].as_array().expect("agents array");
        assert!(agents.is_empty());
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

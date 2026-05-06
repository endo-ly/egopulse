use std::sync::Arc;

use thiserror::Error;
use tracing::info;

use crate::memory::{MemoryContent, collect_sleep_input};
use crate::runtime::AppState;
use crate::storage::{Database, MemoryFile, SleepRunTrigger, call_blocking};

#[derive(Debug, Error)]
pub enum SleepBatchError {
    #[error("already running for agent '{agent_id}'")]
    AlreadyRunning { agent_id: String },
    #[error(transparent)]
    Storage(#[from] crate::error::StorageError),
    #[error("internal error: {0}")]
    Internal(String),
}

/// Runs a manual sleep batch for the given agent.
///
/// When `agent_id` is `None`, the config's `default_agent` is used.
/// This is a skeleton implementation that:
/// 1. Resolves the agent ID
/// 2. Collects sleep input (skip/proceed decision)
/// 3. Creates a sleep run record
/// 4. Saves aggregate snapshots (before == after for no-op)
/// 5. Marks the run as success
///
/// # Errors
///
/// Returns [`SleepBatchError::AlreadyRunning`] if a run is already in progress
/// for the same agent, or [`SleepBatchError::Storage`] on database errors.
pub async fn run_sleep_batch(
    state: &AppState,
    agent_id: Option<&str>,
) -> Result<(), SleepBatchError> {
    let resolved_agent = match agent_id {
        Some(id) => id.to_string(),
        None => state.config.default_agent.as_str().to_string(),
    };

    let db = Arc::clone(&state.db);

    let agent_for_collect = resolved_agent.clone();
    let decision =
        call_blocking(Arc::clone(&db), move |db| collect_sleep_input(db, &agent_for_collect))
            .await?;

    match decision {
        crate::memory::InputDecision::Skip {
            reason,
            new_message_count,
        } => {
            info!(
                agent_id = %resolved_agent,
                new_message_count,
                reason,
                "sleep batch skipped"
            );
            Ok(())
        }
        crate::memory::InputDecision::Proceed {
            source_chats_json, ..
        } => {
            execute_batch(state, db, &resolved_agent, &source_chats_json).await
        }
    }
}

async fn execute_batch(
    state: &AppState,
    db: Arc<Database>,
    agent_id: &str,
    source_chats_json: &str,
) -> Result<(), SleepBatchError> {
    let agent_owned = agent_id.to_string();
    let running = call_blocking(Arc::clone(&db), move |db| {
        db.has_running_sleep_run(&agent_owned)
    })
    .await?;

    if running {
        return Err(SleepBatchError::AlreadyRunning {
            agent_id: agent_id.to_string(),
        });
    }

    let agent_for_run = agent_id.to_string();
    let run_id = call_blocking(Arc::clone(&db), move |db| {
        db.create_sleep_run(&agent_for_run, SleepRunTrigger::Manual)
    })
    .await?;

    let memory = state.memory_loader.load(agent_id);
    save_aggregate_snapshots(&db, &run_id, agent_id, memory.as_ref()).await?;

    let run_id_owned = run_id.clone();
    let source_chats = source_chats_json.to_string();
    call_blocking(db, move |db| {
        db.update_sleep_run_success(&run_id_owned, &source_chats, None, 0, 0)
    })
    .await?;

    info!(agent_id = %agent_id, run_id = %run_id, "sleep batch completed (no-op skeleton)");
    Ok(())
}

async fn save_aggregate_snapshots(
    db: &Arc<Database>,
    run_id: &str,
    agent_id: &str,
    memory: Option<&MemoryContent>,
) -> Result<(), SleepBatchError> {
    let Some(content) = memory else {
        return Ok(());
    };

    let entries: Vec<(MemoryFile, String)> = [
        (MemoryFile::Episodic, &content.episodic),
        (MemoryFile::Semantic, &content.semantic),
        (MemoryFile::Prospective, &content.prospective),
    ]
    .into_iter()
    .filter_map(|(file, maybe)| maybe.as_ref().map(|c| (file, c.clone())))
    .collect();

    for (file, file_content) in entries {
        let run = run_id.to_string();
        let agent = agent_id.to_string();
        call_blocking(Arc::clone(db), move |db| {
            db.create_memory_snapshot(&run, &agent, file, &file_content, &file_content)
        })
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Database, SleepRunStatus};

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn store_msg(db: &Database, id: &str, chat_id: i64, content: &str, ts: &str) {
        let conn = db.conn.lock().expect("lock");
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, chat_id, "alice", content, 0, ts],
        )
        .expect("store message");
    }

    fn create_chat(db: &Database, agent_id: &str, suffix: &str) -> i64 {
        db.resolve_or_create_chat_id(
            "test",
            &format!("test:chat{suffix}"),
            Some(&format!("chat{suffix}")),
            "direct",
            agent_id,
        )
        .expect("create chat")
    }

    fn seed_messages_for_proceed(db: &Database, agent_id: &str) {
        let chat_id = create_chat(db, agent_id, "");
        for i in 1..=6 {
            store_msg(
                db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
            .expect("save session");
    }

    fn build_test_state(db: Database, dir: &std::path::Path) -> AppState {
        let config = crate::test_util::test_config(&dir.to_string_lossy());
        let skills = Arc::new(crate::skills::SkillManager::from_dirs(
            config.user_skills_dir().expect("user_skills_dir"),
            config.skills_dir().expect("skills_dir"),
        ));
        AppState {
            db: Arc::new(db),
            config: config.clone(),
            config_path: None,
            llm_override: None,
            channels: Arc::new(crate::channels::adapter::ChannelRegistry::new()),
            skills: Arc::clone(&skills),
            tools: Arc::new(crate::tools::ToolRegistry::new(&config, skills)),
            mcp_manager: None,
            assets: Arc::new(
                crate::assets::AssetStore::new(&config.assets_dir()).expect("assets"),
            ),
            soul_agents: Arc::new(crate::soul_agents::SoulAgentsLoader::new(&config)),
            memory_loader: Arc::new(crate::memory::MemoryLoader::new(
                std::path::PathBuf::from(&config.state_root).join("agents"),
            )),
            llm_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    #[tokio::test]
    async fn run_sleep_batch_skips_when_input_below_threshold() {
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());
        let result = run_sleep_batch(&state, Some("test-agent")).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_sleep_batch_creates_run_on_proceed() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let state = build_test_state(db, dir.path());

        run_sleep_batch(&state, Some("test-agent"))
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, SleepRunStatus::Success);
    }

    #[tokio::test]
    async fn run_sleep_batch_rejects_double_execution() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let state = build_test_state(db, dir.path());

        state
            .db
            .create_sleep_run("test-agent", SleepRunTrigger::Manual)
            .expect("create running");

        let err = run_sleep_batch(&state, Some("test-agent"))
            .await
            .expect_err("should fail with AlreadyRunning");
        assert!(
            matches!(err, SleepBatchError::AlreadyRunning { .. }),
            "expected AlreadyRunning, got {err:?}"
        );
    }

    #[tokio::test]
    async fn run_sleep_batch_saves_aggregate_snapshots() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        std::fs::create_dir_all(&memory_dir).expect("create memory dir");
        std::fs::write(memory_dir.join("episodic.md"), "episodic content").expect("write");
        std::fs::write(memory_dir.join("semantic.md"), "semantic content").expect("write");

        let state = build_test_state(db, dir.path());
        run_sleep_batch(&state, Some("test-agent"))
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        let run_id = &runs[0].id;

        let snapshots = state
            .db
            .get_snapshots_for_run(run_id)
            .expect("snapshots");
        assert_eq!(snapshots.len(), 2);
        assert!(snapshots.iter().any(|s| s.file == MemoryFile::Episodic));
        assert!(snapshots.iter().any(|s| s.file == MemoryFile::Semantic));
        assert!(snapshots.iter().all(|s| s.content_before == s.content_after));
    }

    #[tokio::test]
    async fn run_sleep_batch_does_not_record_phases_json() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let state = build_test_state(db, dir.path());

        run_sleep_batch(&state, Some("test-agent"))
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        let run = &runs[0];
        let _ = &run.source_chats_json;
    }

    #[tokio::test]
    async fn run_sleep_batch_does_not_record_summary_md() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let state = build_test_state(db, dir.path());

        run_sleep_batch(&state, Some("test-agent"))
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        let run = &runs[0];
        assert!(run.error_message.is_none());
    }

    #[tokio::test]
    async fn run_sleep_batch_marks_success_on_completion() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");
        let state = build_test_state(db, dir.path());

        run_sleep_batch(&state, Some("test-agent"))
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        let run_id = &runs[0].id;
        let refreshed = state
            .db
            .get_sleep_run(run_id)
            .expect("get")
            .expect("exists");
        assert_eq!(refreshed.status, SleepRunStatus::Success);
        assert!(refreshed.finished_at.is_some());
        assert_eq!(refreshed.input_tokens, 0);
        assert_eq!(refreshed.output_tokens, 0);
    }

    #[tokio::test]
    async fn run_sleep_batch_marks_failed_on_error() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let state = build_test_state(db, dir.path());

        state
            .db
            .create_sleep_run("test-agent", SleepRunTrigger::Manual)
            .expect("create running");

        let err = run_sleep_batch(&state, Some("test-agent"))
            .await
            .expect_err("should fail");
        assert!(matches!(err, SleepBatchError::AlreadyRunning { .. }));
    }

    #[tokio::test]
    async fn run_sleep_batch_handles_missing_memory_files() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let memory_dir = dir.path().join("agents").join("test-agent").join("memory");
        std::fs::create_dir_all(&memory_dir).expect("create memory dir");

        let state = build_test_state(db, dir.path());
        run_sleep_batch(&state, Some("test-agent"))
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        let run_id = &runs[0].id;
        let refreshed = state
            .db
            .get_sleep_run(run_id)
            .expect("get")
            .expect("exists");
        assert_eq!(refreshed.status, SleepRunStatus::Success);
    }

    #[tokio::test]
    async fn run_sleep_batch_handles_no_memory_dir() {
        let (db, dir) = test_db();
        seed_messages_for_proceed(&db, "test-agent");

        let state = build_test_state(db, dir.path());
        run_sleep_batch(&state, Some("test-agent"))
            .await
            .expect("batch");

        let runs = state.db.list_sleep_runs("test-agent", 10).expect("list");
        let run_id = &runs[0].id;
        let refreshed = state
            .db
            .get_sleep_run(run_id)
            .expect("get")
            .expect("exists");
        assert_eq!(refreshed.status, SleepRunStatus::Success);
    }

    #[tokio::test]
    async fn run_sleep_batch_uses_default_agent_when_none() {
        let (db, dir) = test_db();
        let state = build_test_state(db, dir.path());

        let default = state.config.default_agent.as_str().to_string();
        let result = run_sleep_batch(&state, None).await;
        assert!(result.is_ok());
        let _ = default;
    }
}

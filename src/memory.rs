use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::error::StorageError;
use crate::storage::{AgentSessionInfo, Database};

/// Threshold (≤ 4) at which sleep is skipped due to too few new messages.
const SKIP_THRESHOLD: i64 = 4;
/// Maximum number of source sessions included in sleep input.
const MAX_SOURCE_SESSIONS: usize = 20;

/// Decision from checking whether enough new messages exist for a sleep run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputDecision {
    /// Not enough new messages → skip the sleep run.
    Skip {
        /// Human-readable reason for the skip.
        reason: String,
        /// Number of new messages found (≤ SKIP_THRESHOLD).
        new_message_count: i64,
    },
    /// Enough new messages → proceed with sleep run.
    Proceed {
        /// Source sessions (limited to MAX_SOURCE_SESSIONS).
        sessions: Vec<AgentSessionInfo>,
        /// JSON array of source chat metadata (for sleep_runs.source_chats_json).
        source_chats_json: String,
    },
}

/// Collects sleep input from the database: counts new messages since the last
/// successful sleep run, and if above threshold, fetches source session info.
///
/// # Errors
///
/// Returns [`StorageError`] if DB queries fail.
#[allow(dead_code)]
pub(crate) fn collect_sleep_input(
    db: &Database,
    agent_id: &str,
) -> Result<InputDecision, StorageError> {
    let latest_run = db.get_latest_successful_run(agent_id)?;
    let cutoff = latest_run.as_ref().and_then(|r| r.finished_at.as_deref());

    let new_message_count = db.count_agent_messages_since(agent_id, cutoff)?;

    if new_message_count <= SKIP_THRESHOLD {
        let reason =
            format!("new messages ({new_message_count}) at or below threshold ({SKIP_THRESHOLD})");
        return Ok(InputDecision::Skip {
            reason,
            new_message_count,
        });
    }

    let sessions = db.get_agent_sessions_since(agent_id, cutoff, MAX_SOURCE_SESSIONS)?;
    let source_chats_json =
        serde_json::to_string(&sessions).map_err(StorageError::SessionSerialize)?;

    Ok(InputDecision::Proceed {
        sessions,
        source_chats_json,
    })
}

/// Agent long-term memory content loaded from markdown files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub(crate) struct MemoryContent {
    pub episodic: Option<String>,
    pub semantic: Option<String>,
    pub prospective: Option<String>,
}

struct CachedContent {
    path: PathBuf,
    content: String,
    mtime: SystemTime,
}

/// Loads agent long-term memory files from `{agents_dir}/{agent_id}/memory/`.
///
/// Follows the same caching pattern as `SoulAgentsLoader` — tracks file mtime
/// and re-reads only when changed.
#[allow(dead_code)]
pub(crate) struct MemoryLoader {
    agents_dir: PathBuf,
    // TODO: Phase 2 で HashMap<PathBuf, CachedContent> に移行（マルチエージェント時にスラッシュ防止）
    episodic_cache: Mutex<Option<CachedContent>>,
    semantic_cache: Mutex<Option<CachedContent>>,
    prospective_cache: Mutex<Option<CachedContent>>,
}

#[allow(dead_code)]
impl MemoryLoader {
    pub(crate) fn new(agents_dir: PathBuf) -> Self {
        Self {
            agents_dir,
            episodic_cache: Mutex::new(None),
            semantic_cache: Mutex::new(None),
            prospective_cache: Mutex::new(None),
        }
    }

    /// Loads memory files for the given agent.
    ///
    /// Reads `agents/{agent_id}/memory/{episodic,semantic,prospective}.md`.
    /// Returns `None` if all files are missing or empty.
    pub(crate) fn load(&self, agent_id: &str) -> Option<MemoryContent> {
        let agent_id = agent_id.trim();
        if !safe_agent_id(agent_id) {
            return None;
        }

        let memory_dir = self.agents_dir.join(agent_id).join("memory");

        let episodic =
            self.cached_read_trimmed(&memory_dir.join("episodic.md"), &self.episodic_cache);
        let semantic =
            self.cached_read_trimmed(&memory_dir.join("semantic.md"), &self.semantic_cache);
        let prospective =
            self.cached_read_trimmed(&memory_dir.join("prospective.md"), &self.prospective_cache);

        if episodic.is_none() && semantic.is_none() && prospective.is_none() {
            return None;
        }

        Some(MemoryContent {
            episodic,
            semantic,
            prospective,
        })
    }

    fn cached_read_trimmed(
        &self,
        path: &Path,
        cache: &Mutex<Option<CachedContent>>,
    ) -> Option<String> {
        let current_mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
        let mut guard = cache.lock().expect("memory cache lock");
        if let (Some(cached), Some(mtime)) = (&*guard, current_mtime) {
            if cached.path == path && cached.mtime == mtime {
                return Some(cached.content.clone());
            }
        }
        let content = read_trimmed(path)?;
        if let Some(mtime) = current_mtime {
            *guard = Some(CachedContent {
                path: path.to_path_buf(),
                content: content.clone(),
                mtime,
            });
        }
        Some(content)
    }
}

#[allow(dead_code)]
fn safe_agent_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && !id.contains("..")
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains(':')
}

#[allow(dead_code)]
fn read_trimmed(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_loader(dir: &Path) -> MemoryLoader {
        MemoryLoader::new(dir.join("agents"))
    }

    fn write_memory_file(dir: &Path, agent_id: &str, file_name: &str, content: &str) {
        let path = dir
            .join("agents")
            .join(agent_id)
            .join("memory")
            .join(file_name);
        fs::create_dir_all(path.parent().expect("memory dir has parent"))
            .expect("create memory dir");
        fs::write(path, content).expect("write memory file");
    }

    fn write_raw_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("file has parent")).expect("create dirs");
        fs::write(path, content).expect("write file");
    }

    // --- Test 1: load all three files ---

    #[test]
    fn load_memory_reads_all_three_files() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "episodic.md", "episodic content");
        write_memory_file(dir.path(), "testagent", "semantic.md", "semantic content");
        write_memory_file(
            dir.path(),
            "testagent",
            "prospective.md",
            "prospective content",
        );

        let loader = make_loader(dir.path());
        let result = loader.load("testagent");

        let mem = result.expect("should return Some");
        assert_eq!(mem.episodic, Some("episodic content".to_string()));
        assert_eq!(mem.semantic, Some("semantic content".to_string()));
        assert_eq!(mem.prospective, Some("prospective content".to_string()));
    }

    // --- Test 2: no memory dir at all ---

    #[test]
    fn load_memory_returns_none_when_no_memory_dir() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load("testagent");
        assert_eq!(result, None);
    }

    // --- Test 3: memory dir exists but no files ---

    #[test]
    fn load_memory_returns_none_when_all_files_missing() {
        let dir = tempfile::tempdir().unwrap();
        let memory_dir = dir.path().join("agents").join("testagent").join("memory");
        fs::create_dir_all(&memory_dir).unwrap();

        let loader = make_loader(dir.path());
        let result = loader.load("testagent");
        assert_eq!(result, None);
    }

    // --- Test 4: skips empty files ---

    #[test]
    fn load_memory_skips_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "episodic.md", "episodic content");
        write_memory_file(dir.path(), "testagent", "semantic.md", "   \n\n  ");
        write_memory_file(
            dir.path(),
            "testagent",
            "prospective.md",
            "prospective content",
        );

        let loader = make_loader(dir.path());
        let result = loader.load("testagent");

        let mem = result.expect("should return Some");
        assert_eq!(mem.episodic, Some("episodic content".to_string()));
        assert_eq!(mem.semantic, None);
        assert_eq!(mem.prospective, Some("prospective content".to_string()));
    }

    // --- Test 5: only episodic exists ---

    #[test]
    fn load_memory_individual_episodic() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "episodic.md", "episodic only");

        let loader = make_loader(dir.path());
        let mem = loader.load("testagent").expect("should return Some");

        assert_eq!(mem.episodic, Some("episodic only".to_string()));
        assert_eq!(mem.semantic, None);
        assert_eq!(mem.prospective, None);
    }

    // --- Test 6: only semantic exists ---

    #[test]
    fn load_memory_individual_semantic() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "semantic.md", "semantic only");

        let loader = make_loader(dir.path());
        let mem = loader.load("testagent").expect("should return Some");

        assert_eq!(mem.episodic, None);
        assert_eq!(mem.semantic, Some("semantic only".to_string()));
        assert_eq!(mem.prospective, None);
    }

    // --- Test 7: only prospective exists ---

    #[test]
    fn load_memory_individual_prospective() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(
            dir.path(),
            "testagent",
            "prospective.md",
            "prospective only",
        );

        let loader = make_loader(dir.path());
        let mem = loader.load("testagent").expect("should return Some");

        assert_eq!(mem.episodic, None);
        assert_eq!(mem.semantic, None);
        assert_eq!(mem.prospective, Some("prospective only".to_string()));
    }

    // --- Test 8: path traversal rejection ---

    #[test]
    fn load_memory_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load("../etc");
        assert_eq!(result, None);
    }

    // --- Test 9: empty agent_id rejection ---

    #[test]
    fn load_memory_rejects_empty_agent_id() {
        let dir = tempfile::tempdir().unwrap();
        let loader = make_loader(dir.path());

        let result = loader.load("");
        assert_eq!(result, None);
    }

    // --- Test 10: cache unchanged file ---

    #[test]
    fn load_memory_caches_unchanged_file() {
        let dir = tempfile::tempdir().unwrap();
        write_memory_file(dir.path(), "testagent", "episodic.md", "cached content");

        let loader = make_loader(dir.path());
        let first = loader.load("testagent");
        let second = loader.load("testagent");

        assert_eq!(first, second);
        assert_eq!(first.unwrap().episodic, Some("cached content".to_string()));
    }

    // --- Test 11: cache invalidation on mtime change ---

    #[test]
    fn load_memory_invalidates_on_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir
            .path()
            .join("agents")
            .join("testagent")
            .join("memory")
            .join("episodic.md");
        write_raw_file(&file_path, "original content");

        let loader = make_loader(dir.path());
        let first = loader.load("testagent");
        assert_eq!(
            first.unwrap().episodic,
            Some("original content".to_string())
        );

        // Ensure mtime differs — modify and force flush
        write_raw_file(&file_path, "updated content");
        // Filesystems have 1s mtime resolution; wait to guarantee a different mtime
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&file_path, "updated content").unwrap();

        let second = loader.load("testagent");
        assert_eq!(
            second.unwrap().episodic,
            Some("updated content".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // collect_sleep_input tests
    // -----------------------------------------------------------------------

    use crate::storage::{Database, SleepRunTrigger};

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn ensure_sleep_runs_table(db: &Database) {
        let conn = db.conn.lock().expect("lock");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sleep_runs (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                trigger_type TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                source_chats_json TEXT NOT NULL DEFAULT '[]',
                source_digest_md TEXT,
                phases_json TEXT NOT NULL DEFAULT '[]',
                summary_md TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                error_message TEXT
            )",
        )
        .expect("create sleep_runs table");
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
        let ext_id = format!("test:chat{suffix}");
        db.resolve_or_create_chat_id(
            "test",
            &ext_id,
            Some(&format!("chat{suffix}")),
            "direct",
            agent_id,
        )
        .expect("create chat")
    }

    fn create_completed_sleep_run(db: &Database, agent_id: &str) -> String {
        ensure_sleep_runs_table(db);
        let run_id = db
            .create_sleep_run(agent_id, SleepRunTrigger::Manual)
            .expect("create sleep run");
        db.update_sleep_run_success(&run_id, "[]", None, "[]", None, 10, 5)
            .expect("complete sleep run");
        run_id
    }

    // --- Test 1: no messages → Skip ---

    #[test]
    fn collect_returns_skip_when_no_messages() {
        let (db, _dir) = test_db();
        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Skip {
                reason,
                new_message_count,
            } => {
                assert_eq!(new_message_count, 0);
                assert!(reason.contains("0"));
                assert!(reason.contains("threshold"));
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    // --- Test 2: ≤ 4 messages → Skip ---

    #[test]
    fn collect_returns_skip_when_below_threshold() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        store_msg(&db, "m-1", chat_id, "hi", "2025-01-01T00:00:01Z");
        store_msg(&db, "m-2", chat_id, "hi", "2025-01-01T00:00:02Z");
        store_msg(&db, "m-3", chat_id, "hi", "2025-01-01T00:00:03Z");
        store_msg(&db, "m-4", chat_id, "hi", "2025-01-01T00:00:04Z");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Skip {
                reason: _,
                new_message_count,
            } => {
                assert_eq!(new_message_count, 4);
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    // --- Test 3: 5 messages → Proceed ---

    #[test]
    fn collect_returns_proceed_above_threshold() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=5 {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
            .expect("save session");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Proceed {
                sessions,
                source_chats_json,
            } => {
                assert!(!sessions.is_empty());
                assert!(!source_chats_json.is_empty());
                assert!(source_chats_json.starts_with('['));
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    // --- Test 4: 10 messages → Proceed ---

    #[test]
    fn collect_returns_proceed_with_many_messages() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=10 {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
            .expect("save session");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        assert!(matches!(result, InputDecision::Proceed { .. }));
    }

    // --- Test 5: cutoff = finished_at of last successful run ---

    #[test]
    fn collect_since_last_successful_run() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");

        // Create a completed sleep run
        let run_id = create_completed_sleep_run(&db, "test-agent");
        let run = db.get_sleep_run(&run_id).expect("get run").expect("exists");
        let _finished_at = run.finished_at.expect("has finished_at");

        // Insert old messages (BEFORE finished_at) — should NOT be counted
        store_msg(&db, "old-1", chat_id, "old", "2020-01-01T00:00:01Z");
        store_msg(&db, "old-2", chat_id, "old", "2020-01-01T00:00:02Z");
        store_msg(&db, "old-3", chat_id, "old", "2020-01-01T00:00:03Z");

        // Create session
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
            .expect("save session");

        // Insert new messages (AFTER finished_at) — timestamps generated
        // after the sleep run was completed, so they're guaranteed to be later
        let after_cutoff = chrono::Utc::now().to_rfc3339();
        store_msg(&db, "new-1", chat_id, "new", &after_cutoff);
        store_msg(&db, "new-2", chat_id, "new", &after_cutoff);
        store_msg(&db, "new-3", chat_id, "new", &after_cutoff);
        store_msg(&db, "new-4", chat_id, "new", &after_cutoff);
        store_msg(&db, "new-5", chat_id, "new", &after_cutoff);

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        assert!(
            matches!(result, InputDecision::Proceed { .. }),
            "5 new messages (> 4 threshold) should trigger Proceed"
        );
    }

    // --- Test 6: first run — no previous run → all messages counted ---

    #[test]
    fn collect_first_run_no_previous_run() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=8 {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
            .expect("save session");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        assert!(
            matches!(result, InputDecision::Proceed { .. }),
            "8 messages with no previous run should trigger Proceed"
        );
    }

    // --- Test 7: respects MAX_SOURCE_SESSIONS limit (20) ---

    #[test]
    fn collect_respects_max_sessions_limit() {
        let (db, _dir) = test_db();
        for i in 0..25 {
            let cid = create_chat(&db, "test-agent", &format!("-{i}"));
            db.save_session(cid, r#"[{"role":"user","content":"hi"}]"#)
                .expect("save session");
            store_msg(&db, &format!("m{i}"), cid, "hi", "2025-06-01T00:00:00Z");
        }

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Proceed { sessions, .. } => {
                assert_eq!(sessions.len(), MAX_SOURCE_SESSIONS);
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    // --- Test 8: source_chats_json has correct fields ---

    #[test]
    fn collect_source_chats_json_format() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        for i in 1..=6 {
            store_msg(
                &db,
                &format!("m-{i}"),
                chat_id,
                "hi",
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }
        db.save_session(chat_id, r#"[{"role":"user","content":"hi"}]"#)
            .expect("save session");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        let json_str = match &result {
            InputDecision::Proceed {
                source_chats_json, ..
            } => source_chats_json,
            other => panic!("expected Proceed, got {other:?}"),
        };

        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(json_str).expect("valid JSON array");
        assert!(!parsed.is_empty(), "should contain at least one entry");

        let entry = &parsed[0];
        assert!(entry.get("chat_id").is_some(), "missing chat_id");
        assert!(entry.get("channel").is_some(), "missing channel");
        assert!(
            entry.get("external_chat_id").is_some(),
            "missing external_chat_id"
        );
        assert!(entry.get("updated_at").is_some(), "missing updated_at");
        assert!(
            entry.get("message_count").is_some(),
            "missing message_count"
        );
        assert!(
            entry.get("estimated_tokens").is_some(),
            "missing estimated_tokens"
        );
    }

    // --- Test 9: source_chats_json sorted newest first ---

    #[test]
    fn collect_source_chats_json_sorted_newest_first() {
        let (db, _dir) = test_db();
        for i in 0..8 {
            let cid = create_chat(&db, "test-agent", &format!("-{i}"));
            store_msg(&db, &format!("m{i}"), cid, "hi", "2025-06-01T00:00:00Z");
            db.save_session(cid, r#"[{"role":"user","content":"hi"}]"#)
                .expect("save session");
        }

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        let json_str = match &result {
            InputDecision::Proceed {
                source_chats_json, ..
            } => source_chats_json,
            other => panic!("expected Proceed, got {other:?}"),
        };

        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(json_str).expect("valid JSON array");
        assert!(
            parsed.len() >= 2,
            "need at least 2 entries to check ordering"
        );

        let timestamps: Vec<String> = parsed
            .iter()
            .map(|v| v["updated_at"].as_str().unwrap_or("").to_string())
            .collect();

        // Verify DESC order (newest first)
        for i in 0..timestamps.len() - 1 {
            assert!(
                timestamps[i] >= timestamps[i + 1],
                "expected newest first: {i}='{}' < {j}='{}'",
                timestamps[i],
                timestamps[i + 1],
                j = i + 1,
            );
        }
    }

    // --- Test 10: Skip variant includes reason and count ---

    #[test]
    fn collect_skip_includes_reason_and_count() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "");
        store_msg(&db, "m-1", chat_id, "hi", "2025-01-01T00:00:01Z");
        store_msg(&db, "m-2", chat_id, "hi", "2025-01-01T00:00:02Z");
        store_msg(&db, "m-3", chat_id, "hi", "2025-01-01T00:00:03Z");

        let result = collect_sleep_input(&db, "test-agent").expect("collect");
        match result {
            InputDecision::Skip {
                reason,
                new_message_count,
            } => {
                assert!(!reason.is_empty(), "reason should not be empty");
                assert!(reason.contains("3"), "reason should mention count");
                assert!(
                    reason.contains("threshold"),
                    "reason should mention threshold"
                );
                assert_eq!(new_message_count, 3);
            }
            other => panic!("expected Skip, got {other:?}"),
        }
    }
}

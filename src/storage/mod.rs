use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::StorageError;

mod migration;
mod queries;

/// Thread-safe SQLite database wrapper for conversation persistence.
pub struct Database {
    pub(crate) conn: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredMessage {
    pub id: String,
    pub chat_id: i64,
    pub sender_name: String,
    pub content: String,
    pub is_from_bot: bool,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub chat_id: i64,
    pub channel: String,
    pub surface_thread: String,
    pub chat_title: Option<String>,
    pub last_message_time: String,
    pub last_message_preview: Option<String>,
    pub agent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatInfo {
    pub chat_id: i64,
    pub channel: String,
    pub external_chat_id: String,
    pub chat_type: String,
    pub agent_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionSnapshot {
    pub messages_json: Option<String>,
    pub updated_at: Option<String>,
    pub recent_messages: Vec<StoredMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct AgentSessionInfo {
    pub chat_id: i64,
    pub channel: String,
    pub external_chat_id: String,
    pub updated_at: String,
    pub message_count: i64,
    pub estimated_tokens: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct ToolCall {
    pub id: String,
    pub chat_id: i64,
    pub message_id: String,
    pub tool_name: String,
    pub tool_input: String,
    pub tool_output: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SleepRunStatus {
    Running,
    Success,
    Failed,
    Skipped,
}

impl fmt::Display for SleepRunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Success => write!(f, "success"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

impl FromStr for SleepRunStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            other => Err(format!("invalid sleep run status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SleepRunTrigger {
    Manual,
    Scheduled,
}

impl fmt::Display for SleepRunTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manual => write!(f, "manual"),
            Self::Scheduled => write!(f, "scheduled"),
        }
    }
}

impl FromStr for SleepRunTrigger {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "manual" => Ok(Self::Manual),
            "scheduled" => Ok(Self::Scheduled),
            other => Err(format!("invalid sleep run trigger: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SleepRun {
    pub id: String,
    pub agent_id: String,
    pub status: SleepRunStatus,
    pub trigger: SleepRunTrigger,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub source_chats_json: String,
    pub source_digest_md: Option<String>,
    pub phases_json: String,
    pub summary_md: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotPhase {
    Pruning,
    Consolidation,
    Compression,
}

impl fmt::Display for SnapshotPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pruning => write!(f, "pruning"),
            Self::Consolidation => write!(f, "consolidation"),
            Self::Compression => write!(f, "compression"),
        }
    }
}

impl FromStr for SnapshotPhase {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pruning" => Ok(Self::Pruning),
            "consolidation" => Ok(Self::Consolidation),
            "compression" => Ok(Self::Compression),
            other => Err(format!("invalid snapshot phase: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryFile {
    Episodic,
    Semantic,
    Prospective,
}

impl fmt::Display for MemoryFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Episodic => write!(f, "episodic"),
            Self::Semantic => write!(f, "semantic"),
            Self::Prospective => write!(f, "prospective"),
        }
    }
}

impl FromStr for MemoryFile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "episodic" => Ok(Self::Episodic),
            "semantic" => Ok(Self::Semantic),
            "prospective" => Ok(Self::Prospective),
            other => Err(format!("invalid memory file: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemorySnapshot {
    pub id: String,
    pub run_id: String,
    pub agent_id: String,
    pub phase: SnapshotPhase,
    pub file: MemoryFile,
    pub content_before: String,
    pub content_after: String,
    pub created_at: String,
}

pub(crate) struct LlmUsageLogEntry<'a> {
    pub chat_id: i64,
    pub caller_channel: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub request_kind: &'a str,
}

const _: () = {
    const fn assert_display<T: fmt::Display>() {}
    const fn assert_from_str<T: FromStr>() {}

    assert_display::<SleepRunStatus>();
    assert_display::<SleepRunTrigger>();
    assert_display::<SnapshotPhase>();
    assert_display::<MemoryFile>();
    assert_from_str::<SleepRunStatus>();
    assert_from_str::<SleepRunTrigger>();
    assert_from_str::<SnapshotPhase>();
    assert_from_str::<MemoryFile>();

    assert_display::<SleepRun>();
    assert_display::<MemorySnapshot>();
};

impl fmt::Display for SleepRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sleep_run({})", self.id)
    }
}

impl fmt::Display for MemorySnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "memory_snapshot({}, {})", self.id, self.phase)
    }
}

pub async fn call_blocking<T, F>(db: Arc<Database>, f: F) -> Result<T, StorageError>
where
    T: Send + 'static,
    F: FnOnce(&Database) -> Result<T, StorageError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || f(db.as_ref()))
        .await
        .map_err(|error| StorageError::TaskJoin(error.to_string()))?
}

impl Database {
    pub(crate) fn new(db_path: &Path) -> Result<Self, StorageError> {
        let legacy_db = db_path
            .parent()
            .and_then(|runtime| runtime.parent())
            .map(|root| root.join("data").join("egopulse.db"))
            .unwrap_or_else(|| Path::new("data").join("egopulse.db"));
        if legacy_db.exists() && !db_path.exists() {
            return Err(StorageError::InitFailed(format!(
                "legacy_db_pending_migration: found {}, but {} does not exist. \
                 run 'mv {} {}' to migrate.",
                legacy_db.display(),
                db_path.display(),
                legacy_db.display(),
                db_path.display(),
            )));
        }

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.busy_timeout(Duration::from_secs(5))?;

        migration::run_migrations(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
        self.conn
            .lock()
            .map_err(|error| StorageError::InitFailed(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn sleep_run_status_display() {
        assert_eq!(SleepRunStatus::Running.to_string(), "running");
        assert_eq!(SleepRunStatus::Success.to_string(), "success");
        assert_eq!(SleepRunStatus::Failed.to_string(), "failed");
        assert_eq!(SleepRunStatus::Skipped.to_string(), "skipped");
    }

    #[test]
    fn sleep_run_trigger_display() {
        assert_eq!(SleepRunTrigger::Manual.to_string(), "manual");
        assert_eq!(SleepRunTrigger::Scheduled.to_string(), "scheduled");
    }

    #[test]
    fn snapshot_phase_display() {
        assert_eq!(SnapshotPhase::Pruning.to_string(), "pruning");
        assert_eq!(SnapshotPhase::Consolidation.to_string(), "consolidation");
        assert_eq!(SnapshotPhase::Compression.to_string(), "compression");
    }

    #[test]
    fn memory_file_display() {
        assert_eq!(MemoryFile::Episodic.to_string(), "episodic");
        assert_eq!(MemoryFile::Semantic.to_string(), "semantic");
        assert_eq!(MemoryFile::Prospective.to_string(), "prospective");
    }

    #[test]
    fn sleep_run_status_from_str() {
        assert_eq!(
            SleepRunStatus::from_str("running").unwrap(),
            SleepRunStatus::Running
        );
        assert_eq!(
            SleepRunStatus::from_str("success").unwrap(),
            SleepRunStatus::Success
        );
        assert_eq!(
            SleepRunStatus::from_str("failed").unwrap(),
            SleepRunStatus::Failed
        );
        assert_eq!(
            SleepRunStatus::from_str("skipped").unwrap(),
            SleepRunStatus::Skipped
        );
        assert!(SleepRunStatus::from_str("invalid").is_err());
    }

    #[test]
    fn sleep_run_trigger_from_str() {
        assert_eq!(
            SleepRunTrigger::from_str("manual").unwrap(),
            SleepRunTrigger::Manual
        );
        assert_eq!(
            SleepRunTrigger::from_str("scheduled").unwrap(),
            SleepRunTrigger::Scheduled
        );
        assert!(SleepRunTrigger::from_str("invalid").is_err());
    }

    #[test]
    fn snapshot_phase_from_str() {
        assert_eq!(
            SnapshotPhase::from_str("pruning").unwrap(),
            SnapshotPhase::Pruning
        );
        assert_eq!(
            SnapshotPhase::from_str("consolidation").unwrap(),
            SnapshotPhase::Consolidation
        );
        assert_eq!(
            SnapshotPhase::from_str("compression").unwrap(),
            SnapshotPhase::Compression
        );
        assert!(SnapshotPhase::from_str("invalid").is_err());
    }

    #[test]
    fn memory_file_from_str() {
        assert_eq!(
            MemoryFile::from_str("episodic").unwrap(),
            MemoryFile::Episodic
        );
        assert_eq!(
            MemoryFile::from_str("semantic").unwrap(),
            MemoryFile::Semantic
        );
        assert_eq!(
            MemoryFile::from_str("prospective").unwrap(),
            MemoryFile::Prospective
        );
        assert!(MemoryFile::from_str("invalid").is_err());
    }
}

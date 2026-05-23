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
    pub message_kind: MessageKind,
    pub sender_agent_id: Option<String>,
    pub recipient_agent_id: Option<String>,
}

impl StoredMessage {
    /// Creates a bot-originated message with auto-generated id and timestamp.
    pub(crate) fn bot(chat_id: i64, content: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            chat_id,
            sender_name: "egopulse".to_string(),
            content,
            is_from_bot: true,
            timestamp: chrono::Utc::now().to_rfc3339(),
            message_kind: MessageKind::Message,
            sender_agent_id: None,
            recipient_agent_id: None,
        }
    }

    /// Creates a user-originated message with auto-generated id and timestamp.
    pub(crate) fn user(chat_id: i64, sender_name: String, content: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            chat_id,
            sender_name,
            content,
            is_from_bot: false,
            timestamp: chrono::Utc::now().to_rfc3339(),
            message_kind: MessageKind::Message,
            sender_agent_id: None,
            recipient_agent_id: None,
        }
    }
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
pub(crate) enum MessageKind {
    Message,
    AgentSend,
    SystemEvent,
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message => write!(f, "message"),
            Self::AgentSend => write!(f, "agent_send"),
            Self::SystemEvent => write!(f, "system_event"),
        }
    }
}

impl FromStr for MessageKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "message" => Ok(Self::Message),
            "agent_send" => Ok(Self::AgentSend),
            "system_event" => Ok(Self::SystemEvent),
            other => Err(format!("invalid message kind: {other}")),
        }
    }
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

/// How a sleep batch run was initiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepRunTrigger {
    /// User-triggered via CLI or WebUI.
    Manual,
    /// Triggered by the automatic scheduler.
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
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub error_message: Option<String>,
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
    pub file: MemoryFile,
    pub content_before: String,
    pub content_after: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PulseRunStatus {
    Running,
    Success,
    Failed,
    Skipped,
}

impl fmt::Display for PulseRunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Success => write!(f, "success"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

impl FromStr for PulseRunStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            other => Err(format!("invalid pulse run status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PulseOutputKind {
    /// No notification sent (silent success).
    Silent,
    /// Notification sent to the home surface.
    Notify,
}

impl fmt::Display for PulseOutputKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Silent => write!(f, "silent"),
            Self::Notify => write!(f, "notify"),
        }
    }
}

impl FromStr for PulseOutputKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "silent" => Ok(Self::Silent),
            "notify" => Ok(Self::Notify),
            other => Err(format!("invalid pulse output kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PulseRun {
    pub id: String,
    pub agent_id: String,
    pub intention_id: String,
    pub due_key: String,
    pub chat_id: Option<i64>,
    pub message_id: Option<String>,
    pub status: PulseRunStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub output_kind: Option<PulseOutputKind>,
    pub output_text: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EpisodeEventKind {
    Self_,
    Relationship,
    World,
    Feat,
    Anomaly,
    Decision,
    Insight,
    Rhythm,
}

impl fmt::Display for EpisodeEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Self_ => write!(f, "self"),
            Self::Relationship => write!(f, "relationship"),
            Self::World => write!(f, "world"),
            Self::Feat => write!(f, "feat"),
            Self::Anomaly => write!(f, "anomaly"),
            Self::Decision => write!(f, "decision"),
            Self::Insight => write!(f, "insight"),
            Self::Rhythm => write!(f, "rhythm"),
        }
    }
}

impl FromStr for EpisodeEventKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "self" => Ok(Self::Self_),
            "relationship" => Ok(Self::Relationship),
            "world" => Ok(Self::World),
            "feat" => Ok(Self::Feat),
            "anomaly" => Ok(Self::Anomaly),
            "decision" => Ok(Self::Decision),
            "insight" => Ok(Self::Insight),
            "rhythm" => Ok(Self::Rhythm),
            other => Err(format!("invalid episode event kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EpisodeEventCertainty {
    Observed,
    Inferred,
    Uncertain,
}

impl fmt::Display for EpisodeEventCertainty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Observed => write!(f, "observed"),
            Self::Inferred => write!(f, "inferred"),
            Self::Uncertain => write!(f, "uncertain"),
        }
    }
}

impl FromStr for EpisodeEventCertainty {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "observed" => Ok(Self::Observed),
            "inferred" => Ok(Self::Inferred),
            "uncertain" => Ok(Self::Uncertain),
            other => Err(format!("invalid episode event certainty: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpisodeEvent {
    pub id: String,
    pub agent_id: String,
    pub experienced_at: String,
    pub encoded_at: String,
    pub kind: EpisodeEventKind,
    pub title: String,
    pub body_md: String,
    pub ripple_strength: i64,
    pub certainty: EpisodeEventCertainty,
    pub sleep_run_id: String,
    pub source_refs_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
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
    assert_display::<MemoryFile>();
    assert_display::<MessageKind>();
    assert_from_str::<SleepRunStatus>();
    assert_from_str::<SleepRunTrigger>();
    assert_from_str::<MemoryFile>();
    assert_from_str::<MessageKind>();

    assert_display::<PulseRunStatus>();
    assert_from_str::<PulseRunStatus>();
    assert_display::<PulseOutputKind>();
    assert_from_str::<PulseOutputKind>();

    assert_display::<SleepRun>();
    assert_display::<MemorySnapshot>();
    assert_display::<PulseRun>();

    assert_display::<EpisodeEventKind>();
    assert_from_str::<EpisodeEventKind>();
    assert_display::<EpisodeEventCertainty>();
    assert_from_str::<EpisodeEventCertainty>();
};

impl fmt::Display for SleepRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sleep_run({})", self.id)
    }
}

impl fmt::Display for MemorySnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "memory_snapshot({}, {})", self.id, self.file)
    }
}

impl fmt::Display for PulseRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pulse_run({})", self.id)
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
    fn message_kind_display_message() {
        assert_eq!(MessageKind::Message.to_string(), "message");
    }

    #[test]
    fn message_kind_display_agent_send() {
        assert_eq!(MessageKind::AgentSend.to_string(), "agent_send");
    }

    #[test]
    fn message_kind_display_system_event() {
        assert_eq!(MessageKind::SystemEvent.to_string(), "system_event");
    }

    #[test]
    fn message_kind_from_str_valid() {
        assert_eq!(
            MessageKind::from_str("message").unwrap(),
            MessageKind::Message
        );
        assert_eq!(
            MessageKind::from_str("agent_send").unwrap(),
            MessageKind::AgentSend
        );
        assert_eq!(
            MessageKind::from_str("system_event").unwrap(),
            MessageKind::SystemEvent
        );
    }

    #[test]
    fn message_kind_from_str_unknown() {
        assert!(MessageKind::from_str("unknown").is_err());
    }

    #[test]
    fn sleep_run_trigger_display() {
        assert_eq!(SleepRunTrigger::Manual.to_string(), "manual");
        assert_eq!(SleepRunTrigger::Scheduled.to_string(), "scheduled");
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

    #[test]
    fn episode_event_kind_display() {
        assert_eq!(EpisodeEventKind::Self_.to_string(), "self");
        assert_eq!(EpisodeEventKind::Relationship.to_string(), "relationship");
        assert_eq!(EpisodeEventKind::World.to_string(), "world");
        assert_eq!(EpisodeEventKind::Feat.to_string(), "feat");
        assert_eq!(EpisodeEventKind::Anomaly.to_string(), "anomaly");
        assert_eq!(EpisodeEventKind::Decision.to_string(), "decision");
        assert_eq!(EpisodeEventKind::Insight.to_string(), "insight");
        assert_eq!(EpisodeEventKind::Rhythm.to_string(), "rhythm");
    }

    #[test]
    fn episode_event_kind_from_str_valid() {
        assert_eq!(
            EpisodeEventKind::from_str("self").unwrap(),
            EpisodeEventKind::Self_
        );
        assert_eq!(
            EpisodeEventKind::from_str("relationship").unwrap(),
            EpisodeEventKind::Relationship
        );
        assert_eq!(
            EpisodeEventKind::from_str("world").unwrap(),
            EpisodeEventKind::World
        );
        assert_eq!(
            EpisodeEventKind::from_str("feat").unwrap(),
            EpisodeEventKind::Feat
        );
        assert_eq!(
            EpisodeEventKind::from_str("anomaly").unwrap(),
            EpisodeEventKind::Anomaly
        );
        assert_eq!(
            EpisodeEventKind::from_str("decision").unwrap(),
            EpisodeEventKind::Decision
        );
        assert_eq!(
            EpisodeEventKind::from_str("insight").unwrap(),
            EpisodeEventKind::Insight
        );
        assert_eq!(
            EpisodeEventKind::from_str("rhythm").unwrap(),
            EpisodeEventKind::Rhythm
        );
    }

    #[test]
    fn episode_event_kind_from_str_invalid() {
        assert!(EpisodeEventKind::from_str("unknown").is_err());
    }

    #[test]
    fn episode_event_certainty_display() {
        assert_eq!(EpisodeEventCertainty::Observed.to_string(), "observed");
        assert_eq!(EpisodeEventCertainty::Inferred.to_string(), "inferred");
        assert_eq!(EpisodeEventCertainty::Uncertain.to_string(), "uncertain");
    }

    #[test]
    fn episode_event_certainty_from_str_valid() {
        assert_eq!(
            EpisodeEventCertainty::from_str("observed").unwrap(),
            EpisodeEventCertainty::Observed
        );
        assert_eq!(
            EpisodeEventCertainty::from_str("inferred").unwrap(),
            EpisodeEventCertainty::Inferred
        );
        assert_eq!(
            EpisodeEventCertainty::from_str("uncertain").unwrap(),
            EpisodeEventCertainty::Uncertain
        );
    }

    #[test]
    fn episode_event_certainty_from_str_invalid() {
        assert!(EpisodeEventCertainty::from_str("invalid").is_err());
    }
}

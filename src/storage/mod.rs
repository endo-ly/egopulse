use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use r2d2::ManageConnection;
use rusqlite::Connection;

use crate::error::StorageError;

macro_rules! define_enum {
    (
        $(#[$outer:meta])*
        $vis:vis enum $name:ident {
            $($variant:ident => $str:expr),+ $(,)?
        }
    ) => {
        $(#[$outer])*
        $vis enum $name {
            $($variant,)+
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    $(Self::$variant => write!(f, $str),)+
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($str => Ok(Self::$variant),)+
                    other => Err(format!(concat!("invalid ", stringify!($name), ": {}"), other)),
                }
            }
        }

        const _: () = {
            const fn assert_traits<T: fmt::Display + std::str::FromStr>() {}
            assert_traits::<$name>();
        };
    };
}

macro_rules! parse_row_enum {
    ($row:expr, $idx:expr, $ty:ty) => {{
        let s: String = $row.get($idx)?;
        <$ty>::from_str(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                $idx,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })
    }};
}

pub(crate) mod backup;
mod chat;
mod episode;
mod migration;
mod pulse;
mod sleep;
mod tool;

pub(crate) use backup::BackupSettings;

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Connection factory that opens a SQLite database file with connection-local
/// SQLite settings. Database-file settings such as WAL mode are initialized
/// once before the pool is built.
#[derive(Debug)]
pub(crate) struct SqliteConnectionManager {
    path: PathBuf,
}

impl SqliteConnectionManager {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl ManageConnection for SqliteConnectionManager {
    type Connection = Connection;
    type Error = rusqlite::Error;

    fn connect(&self) -> Result<Connection, Self::Error> {
        let conn = Connection::open(&self.path)?;
        configure_connection(&conn)?;
        Ok(conn)
    }

    fn is_valid(&self, conn: &mut Connection) -> Result<(), Self::Error> {
        conn.execute_batch("")
    }

    fn has_broken(&self, _conn: &mut Connection) -> bool {
        false
    }
}

type Pool = r2d2::Pool<SqliteConnectionManager>;
type PooledConn = r2d2::PooledConnection<SqliteConnectionManager>;

fn configure_connection(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    Ok(())
}

fn initialize_database_file(db_path: &Path) -> Result<(), StorageError> {
    let conn = Connection::open(db_path)?;
    configure_connection(&conn)?;
    let journal_mode: String = conn.query_row("PRAGMA journal_mode=WAL;", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StorageError::InitFailed(format!(
            "failed to enable sqlite wal mode for {}: journal_mode={journal_mode}",
            db_path.display(),
        )));
    }
    Ok(())
}

/// Thread-safe SQLite database wrapper backed by a connection pool.
///
/// The database file is initialized in WAL mode before pooling starts, and
/// each pooled connection receives connection-local settings such as
/// `busy_timeout = 5 s`.
pub struct Database {
    pool: Pool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredMessage {
    pub id: String,
    pub chat_id: i64,
    pub sender_id: String,
    pub content: String,
    pub sender_kind: SenderKind,
    pub timestamp: String,
    pub message_kind: MessageKind,
    pub recipient_agent_id: Option<String>,
}

impl StoredMessage {
    fn new(
        chat_id: i64,
        sender_id: String,
        content: String,
        sender_kind: SenderKind,
        recipient_agent_id: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            chat_id,
            sender_id,
            content,
            sender_kind,
            timestamp: chrono::Utc::now().to_rfc3339(),
            message_kind: MessageKind::Message,
            recipient_agent_id,
        }
    }

    /// Creates an assistant-originated message with auto-generated id and timestamp.
    pub(crate) fn assistant(chat_id: i64, sender_id: String, content: String) -> Self {
        Self::new(chat_id, sender_id, content, SenderKind::Assistant, None)
    }

    /// Creates a user-originated message with auto-generated id and timestamp.
    pub(crate) fn user(chat_id: i64, sender_id: String, content: String) -> Self {
        Self::new(chat_id, sender_id, content, SenderKind::User, None)
    }

    pub(crate) fn system(chat_id: i64, content: String) -> Self {
        Self::new(
            chat_id,
            "system".to_string(),
            content,
            SenderKind::System,
            None,
        )
    }

    pub(crate) fn tool(
        chat_id: i64,
        sender_id: String,
        recipient_id: String,
        content: String,
    ) -> Self {
        Self::new(
            chat_id,
            sender_id,
            content,
            SenderKind::Tool,
            Some(recipient_id),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub chat_id: i64,
    pub channel: String,
    pub external_chat_id: String,
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

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum MessageKind {
        Message => "message",
        AgentSend => "agent_send",
        SystemEvent => "system_event",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SenderKind {
        User => "user",
        Assistant => "assistant",
        System => "system",
        Tool => "tool",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SleepRunStatus {
        Running => "running",
        Success => "success",
        PartialFailure => "partial_failure",
        Failed => "failed",
        Skipped => "skipped",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SleepRunTrigger {
        Manual => "manual",
        Scheduled => "scheduled",
        Backfill => "backfill",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SleepStepName {
        EventExtraction => "event_extraction",
        EpisodicUpdate => "episodic_update",
        SemanticUpdate => "semantic_update",
        ProspectiveUpdate => "prospective_update",
    }
}

impl SleepStepName {
    pub(crate) const ALL: [Self; 4] = [
        Self::EventExtraction,
        Self::EpisodicUpdate,
        Self::SemanticUpdate,
        Self::ProspectiveUpdate,
    ];
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum SleepStepStatus {
        Pending => "pending",
        Running => "running",
        Success => "success",
        Failed => "failed",
        Skipped => "skipped",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum CheckpointSourceKind {
        Messages => "messages",
        EpisodeEvents => "episode_events",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum MemoryFile {
        Episodic => "episodic",
        Semantic => "semantic",
        Prospective => "prospective",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum PulseRunStatus {
        Running => "running",
        Success => "success",
        Failed => "failed",
        Skipped => "skipped",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum PulseOutputKind {
        Silent => "silent",
        Notify => "notify",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum EpisodeEventKind {
        Self_ => "self",
        Relationship => "relationship",
        World => "world",
        Feat => "feat",
        Anomaly => "anomaly",
        Decision => "decision",
        Insight => "insight",
        Rhythm => "rhythm",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum EpisodeEventCertainty {
        Stated => "stated",
        Derived => "derived",
        Tentative => "tentative",
    }
}

define_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum RollupGranularity {
        Week => "week",
        Month => "month",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SleepRunStep {
    pub sleep_run_id: String,
    pub step_name: SleepStepName,
    pub status: SleepStepStatus,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub error_message: Option<String>,
    pub metadata_json: Option<String>,
}

pub(crate) struct SleepStepResult<'a> {
    pub status: SleepStepStatus,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub error_message: Option<&'a str>,
    pub metadata_json: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SleepStepCheckpoint {
    pub agent_id: String,
    pub step_name: SleepStepName,
    pub source_kind: CheckpointSourceKind,
    pub source_id: String,
    pub cursor_at: String,
    pub cursor_id: String,
    pub updated_at: String,
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

/// A derived summary rollup over a week or month of episode events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpisodeRollup {
    pub id: String,
    pub agent_id: String,
    pub granularity: RollupGranularity,
    pub period_key: String,
    pub period_start: String,
    pub period_end_exclusive: String,
    pub summary_md: String,
    pub max_ripple: i64,
    pub event_count: i64,
    pub generated_run_id: String,
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

    assert_display::<SleepRun>();
    assert_display::<MemorySnapshot>();
    assert_display::<PulseRun>();
    assert_display::<SleepRunStep>();
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

impl fmt::Display for SleepRunStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sleep_run_step({}, {})",
            self.sleep_run_id, self.step_name
        )
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
    /// Constructs a `Database` without running a pre-migration backup.
    ///
    /// Only used by tests; production code must use [`Database::new_with_backup`]
    /// so the runtime can perform a backup before migrations touch the schema.
    #[cfg(test)]
    pub(crate) fn new(db_path: &Path) -> Result<Self, StorageError> {
        prepare_db_path(db_path)?;
        initialize_database_file(db_path)?;
        let pool = build_pool_and_migrate(db_path)?;
        Ok(Self { pool })
    }

    /// Initializes the database with a pre-migration backup when `settings.enabled`
    /// is `true` and the DB file already exists. The backup runs against a raw
    /// connection before the pool is constructed, so any failure is logged and
    /// swallowed to keep the startup path best-effort. After the backup (or
    /// skip), this function behaves exactly like [`Database::new`].
    pub(crate) fn new_with_backup(
        db_path: &Path,
        settings: &backup::BackupSettings,
    ) -> Result<Self, StorageError> {
        prepare_db_path(db_path)?;
        if db_path.exists() && settings.enabled {
            if let Err(error) = backup::run_startup_backup(db_path, settings) {
                tracing::warn!(%error, "startup backup failed; continuing");
            }
        }
        initialize_database_file(db_path)?;
        let pool = build_pool_and_migrate(db_path)?;
        Ok(Self { pool })
    }

    /// Constructs a `Database` for the secret DB (`secret.db`).
    ///
    /// Runs `run_secret_migrations` instead of `run_migrations`. The secret DB
    /// contains only `chats`, `messages`, `sessions`, `llm_usage_logs`, `db_meta`,
    /// and `schema_migrations` — no `tool_calls`, `sleep_runs`, etc.
    pub(crate) fn new_secret(db_path: &Path) -> Result<Self, StorageError> {
        prepare_db_path(db_path)?;
        initialize_database_file(db_path)?;
        let pool = build_secret_pool_and_migrate(db_path)?;
        Ok(Self { pool })
    }

    pub(crate) fn get_conn(&self) -> Result<PooledConn, StorageError> {
        self.pool
            .get()
            .map_err(|e| StorageError::InitFailed(e.to_string()))
    }

    /// Creates a pool-backed Database without running migrations.
    /// Used by migration tests that need a specific schema version.
    #[cfg(test)]
    pub(crate) fn new_unchecked(db_path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        initialize_database_file(db_path)?;

        let manager = SqliteConnectionManager::new(db_path.to_path_buf());
        let pool = r2d2::Pool::builder()
            .max_size(4)
            .build(manager)
            .map_err(|e| StorageError::InitFailed(e.to_string()))?;
        Ok(Self { pool })
    }
}

fn prepare_db_path(db_path: &Path) -> Result<(), StorageError> {
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
    Ok(())
}

fn build_pool_and_migrate(db_path: &Path) -> Result<Pool, StorageError> {
    let manager = SqliteConnectionManager::new(db_path.to_path_buf());
    let pool = r2d2::Pool::builder()
        .max_size(4)
        .build(manager)
        .map_err(|e| StorageError::InitFailed(e.to_string()))?;

    {
        let conn = pool
            .get()
            .map_err(|e| StorageError::InitFailed(e.to_string()))?;
        migration::run_migrations(&conn)?;
    }
    Ok(pool)
}

fn build_secret_pool_and_migrate(db_path: &Path) -> Result<Pool, StorageError> {
    let manager = SqliteConnectionManager::new(db_path.to_path_buf());
    let pool = r2d2::Pool::builder()
        .max_size(4)
        .build(manager)
        .map_err(|e| StorageError::InitFailed(e.to_string()))?;

    {
        let conn = pool
            .get()
            .map_err(|e| StorageError::InitFailed(e.to_string()))?;
        migration::run_secret_migrations(&conn)?;
    }
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("runtime").join("egopulse.db")
    }

    #[test]
    fn database_new_initializes_wal_before_pooling_and_configures_busy_timeout() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_db_path(&dir);

        // Act
        let db = Database::new(&db_path).expect("db");
        let conn = db.get_conn().expect("conn");
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .expect("journal_mode");
        let busy_timeout_ms: i64 = conn
            .query_row("PRAGMA busy_timeout;", [], |row| row.get(0))
            .expect("busy_timeout");

        // Assert
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(busy_timeout_ms, SQLITE_BUSY_TIMEOUT.as_millis() as i64);
    }

    #[test]
    fn stored_message_assistant_factory() {
        let msg = StoredMessage::assistant(42, "lyre".to_string(), "hello".to_string());
        assert_eq!(msg.sender_id, "lyre");
        assert_eq!(msg.sender_kind, SenderKind::Assistant);
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.chat_id, 42);
        assert!(msg.recipient_agent_id.is_none());
    }

    #[test]
    fn stored_message_user_factory() {
        let msg = StoredMessage::user(10, "user:cli:default".to_string(), "hi".to_string());
        assert_eq!(msg.sender_id, "user:cli:default");
        assert_eq!(msg.sender_kind, SenderKind::User);
        assert_eq!(msg.content, "hi");
        assert!(msg.recipient_agent_id.is_none());
    }

    #[test]
    fn new_secret_opens_wal_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("secret.db");

        let db = Database::new_secret(&db_path).expect("secret db");
        let conn = db.get_conn().expect("conn");
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .expect("journal_mode");

        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    }

    #[test]
    fn run_secret_migrations_creates_expected_tables() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("secret.db");

        let db = Database::new_secret(&db_path).expect("secret db");
        let conn = db.get_conn().expect("conn");
        let table_names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .expect("prepare")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query_map")
            .filter_map(std::result::Result::ok)
            .collect();

        for expected in &[
            "chats",
            "messages",
            "sessions",
            "llm_usage_logs",
            "db_meta",
            "schema_migrations",
        ] {
            assert!(
                table_names.iter().any(|t| t == expected),
                "missing table: {expected}"
            );
        }
        assert!(
            !table_names.iter().any(|t| t == "tool_calls"),
            "tool_calls must not exist in secret.db"
        );
        assert!(
            !table_names.iter().any(|t| t == "sleep_runs"),
            "sleep_runs must not exist in secret.db"
        );
    }

    #[test]
    fn run_secret_migrations_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("secret.db");

        let _db1 = Database::new_secret(&db_path).expect("first open");
        let db2 = Database::new_secret(&db_path).expect("second open");
        let conn = db2.get_conn().expect("conn");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert!(count >= 6, "expected at least 6 tables, got {count}");
    }
}

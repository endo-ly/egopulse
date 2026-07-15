//! アプリケーション全体で使用するエラー型。
//!
//! thiserror を用いて各ドメイン (Config / LLM / Storage / Channel / TUI / Logging) ごとに
//! エラーエニュームを定義し、`EgoPulseError` で統一的に扱う。

use std::path::PathBuf;

use thiserror::Error;

/// Top-level error type aggregating all domain-specific errors.
#[derive(Debug, Error)]
pub enum EgoPulseError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Logging(#[from] LoggingError),
    #[error(transparent)]
    Tui(#[from] TuiError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Channel(#[from] ChannelError),
    #[error(transparent)]
    Mcp(#[from] McpError),
    #[error(transparent)]
    #[allow(private_interfaces)]
    Pulse(#[from] crate::pulse::definition::PulseParseError),
    #[error(transparent)]
    #[allow(private_interfaces)]
    SetupWizard(#[from] crate::setup::SetupWizardError),
    #[error("shutdown_requested")]
    ShutdownRequested,
    /// Another EgoPulse process already holds the exclusive runtime instance
    /// lock for this state root. The `String` payload is the lock file path,
    /// surfaced to the operator so they can locate the conflicting process.
    /// Startup is refused before the database is opened.
    #[error(
        "another EgoPulse process already holds the runtime instance lock for this state root: {0}"
    )]
    RuntimeAlreadyRunning(String),
    #[error("internal_error: {0}")]
    Internal(String),
    /// A turn was already being executed by another executor when this one
    /// tried to begin. This is a benign, expected race (e.g. a recovered turn
    /// re-dispatched by the turn dispatcher, or a duplicate delivery) and must
    /// NOT be surfaced as a turn failure or mark the turn terminal.
    #[error("turn already claimed by another executor")]
    TurnConcurrencyConflict,
}

impl EgoPulseError {
    /// 構造化ログの `error_kind` フィールドやユーザー向けメッセージの振り分けに使用する分類タグ。
    pub(crate) fn error_kind(&self) -> &'static str {
        match self {
            Self::Config(_) => "config",
            Self::Llm(_) => "llm",
            Self::Logging(_) => "logging",
            Self::Tui(_) => "tui",
            Self::Storage(_) => "storage",
            Self::Channel(_) => "channel",
            Self::Mcp(_) => "mcp",
            Self::Pulse(_) => "pulse",
            Self::SetupWizard(_) => "setup",
            Self::ShutdownRequested => "shutdown",
            Self::RuntimeAlreadyRunning(_) => "instance_lock",
            Self::TurnConcurrencyConflict => "concurrency",
            Self::Internal(_) => "internal",
        }
    }

    /// エラー全文をそのままユーザーに返す。
    pub(crate) fn user_message(&self) -> String {
        format!("Error: {self}")
    }

    /// Returns a short, user-facing error summary suitable for chat channels.
    pub(crate) fn user_facing_summary(&self) -> String {
        match self {
            Self::Llm(LlmError::ApiError {
                status,
                retry_after_secs: Some(secs),
                ..
            }) if status.as_u16() == 429 => {
                format!("LLM API rate limited. Available in {secs}s.")
            }
            Self::Llm(LlmError::ApiError { status, .. }) => {
                if status.is_server_error() {
                    "LLM API server error. Please try again later.".to_string()
                } else if status.as_u16() == 429 {
                    "LLM API rate limited. Please try again later.".to_string()
                } else if status.is_client_error() {
                    "LLM API request error. Please check your configuration.".to_string()
                } else {
                    format!("LLM API error (HTTP {status})")
                }
            }
            Self::Llm(LlmError::RequestFailed(_)) => {
                "LLM API connection failed. Please try again later.".to_string()
            }
            Self::Llm(LlmError::InvalidResponse(_)) => {
                "LLM returned an invalid response.".to_string()
            }
            Self::Storage(_) => "Internal storage error.".to_string(),
            _ => "An unexpected error occurred.".to_string(),
        }
    }

    pub(crate) fn is_codex_auth_error(&self) -> bool {
        matches!(
            self,
            Self::Llm(LlmError::ApiError { status, .. })
                if status.as_u16() == 401
        )
    }
}

/// Configuration loading and validation errors.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config_not_found: {path}")]
    ConfigNotFound { path: PathBuf },
    #[error(
        "config_auto_discovery_failed: no egopulse.config.yaml found. searched={searched_paths:?}. run 'egopulse setup' or pass --config <PATH>"
    )]
    AutoConfigNotFound { searched_paths: Vec<PathBuf> },
    #[error("config_read_failed: {path}: {source}")]
    ConfigReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config_parse_failed: {path}: {detail}")]
    ConfigParseFailed { path: PathBuf, detail: String },
    #[error("missing_default_provider")]
    MissingDefaultProvider,
    #[error("missing_providers")]
    MissingProviders,
    #[error("missing_provider")]
    MissingProvider,
    #[error("missing_provider_base_url: {provider}")]
    MissingProviderBaseUrl { provider: String },
    #[error("missing_provider_default_model: {provider}")]
    MissingProviderDefaultModel { provider: String },
    #[error("invalid_provider_reference: {provider}")]
    InvalidProviderReference { provider: String },
    #[error("invalid_base_url")]
    InvalidBaseUrl,
    #[error("web_channel_disabled")]
    WebChannelDisabled,
    #[error("missing_web_auth_token")]
    MissingWebAuthToken,
    #[error("missing_voice_auth_token")]
    MissingVoiceAuthToken,
    #[error("voice_channel_requires_web_channel")]
    VoiceRequiresWebChannel,
    #[error("missing_provider_api_key: {provider}")]
    MissingProviderApiKey { provider: String },
    #[error("invalid_compaction_config: {0}")]
    InvalidCompactionConfig(String),
    #[error("no_active_channels: no enabled channel has a valid bot_token configured")]
    NoActiveChannels,
    #[error("invalid_agent_id: {id}")]
    InvalidAgentId { id: String },
    #[error("default_agent_not_found: {agent_id}")]
    DefaultAgentNotFound { agent_id: String },
    #[error("agent_not_found: {agent_id}")]
    AgentNotFound { agent_id: String },
    #[error("invalid_bot_id: {id}")]
    InvalidBotId { id: String },
    #[error(
        "discord_bot_channel_agent_not_found: bot={bot_id} channel={channel_id} agent={agent_id}"
    )]
    DiscordBotChannelAgentNotFound {
        bot_id: String,
        channel_id: u64,
        agent_id: String,
    },
    #[error("duplicate_bot_id: bot_id={bot_id} (normalized from '{original_key}')")]
    DuplicateBotId {
        bot_id: String,
        original_key: String,
    },
    #[error("invalid_channels_key: key='{key}' is not a valid u64")]
    InvalidChannelsKey { key: String },
    #[error("invalid_chats_key: key='{key}' is not a valid i64")]
    InvalidChatsKey { key: String },
    /// OS のホームディレクトリが解決できなかった。
    #[error("home_directory_unresolved: OS home directory could not be resolved")]
    HomeDirectoryUnresolved,
    /// SecretRef の解決に失敗した（環境変数が見つからない等）。
    #[error("secret_ref_unresolved: {reference}")]
    SecretRefUnresolved { reference: String },
    /// SecretRef の exec コマンド実行に失敗した。
    #[error("secret_ref_exec_failed: {command}: {detail}")]
    SecretRefExecFailed { command: String, detail: String },
    #[error("sleep_batch_enabled_requires_schedule")]
    SleepBatchEnabledRequiresSchedule,
    #[error("sleep_batch_unknown_agent: {agent_id}")]
    SleepBatchUnknownAgent { agent_id: String },
    #[error("sleep_batch_invalid_schedule: {schedule}")]
    SleepBatchInvalidSchedule { schedule: String },
    #[error("sleep_batch_invalid_retry: {detail}")]
    SleepBatchInvalidRetry { detail: String },
    #[error("discord_bot_channel_multi_agent_mismatch: bot={bot_id} channel={channel_id} {reason}")]
    DiscordBotChannelMultiAgentMismatch {
        bot_id: String,
        channel_id: u64,
        reason: String,
    },
    #[error("agent_discord_bot_not_found: agent={agent_id} bot={bot_id}")]
    AgentDiscordBotNotFound { agent_id: String, bot_id: String },
    #[error("telegram_bot_channel_agent_not_found: channel={channel_id} agent={agent_id}")]
    TelegramBotChannelAgentNotFound { channel_id: i64, agent_id: String },
    #[error("telegram_bot_channel_multi_agent_mismatch: channel={channel_id} {reason}")]
    TelegramBotChannelMultiAgentMismatch { channel_id: i64, reason: String },
    #[error("agent_telegram_bot_not_found: agent={agent_id} bot={bot_id}")]
    AgentTelegramBotNotFound { agent_id: String, bot_id: String },
    #[error("pulse_invalid_timezone: {timezone}")]
    PulseInvalidTimezone { timezone: String },
    #[error("pulse_invalid_tick_interval: {reason}")]
    PulseInvalidTickInterval { reason: String },
    #[error("invalid_timezone: {timezone}")]
    InvalidTimezone { timezone: String },
    #[error(
        "model_instructions_conflict: provider={provider} model={model}: specify either 'model_instructions' or 'model_instructions_file', not both"
    )]
    ModelInstructionsConflict { provider: String, model: String },
    #[error(
        "model_instructions_file_unreadable: provider={provider} model={model} path={path}: {detail}"
    )]
    ModelInstructionsFileUnreadable {
        provider: String,
        model: String,
        path: String,
        detail: String,
    },
    #[error("invalid_backup_config: {0}")]
    InvalidBackupConfig(String),
    #[error("invalid_webhook_receiver_id: {id}")]
    InvalidWebhookReceiverId { id: String },
    #[error("webhook_receiver_token_missing: {receiver_id}")]
    WebhookReceiverTokenMissing { receiver_id: String },
    #[error("webhook_target_channel_missing: {receiver_id}")]
    WebhookTargetChannelMissing { receiver_id: String },
    #[error("webhook_target_thread_required: {receiver_id} channel={channel}")]
    WebhookTargetThreadRequired {
        receiver_id: String,
        channel: String,
    },
}

/// TUI (Terminal User Interface) rendering and event errors.
#[derive(Debug, Error)]
pub enum TuiError {
    #[error("tui_init_failed: {0}")]
    InitFailed(String),
    #[error("tui_render_failed: {0}")]
    RenderFailed(String),
    #[error("tui_event_failed: {0}")]
    EventFailed(String),
}

/// LLM provider request and response errors.
#[derive(Debug, Error)]
pub enum LlmError {
    #[error("llm_init_failed: {0}")]
    InitFailed(String),
    #[error("llm_request_failed: {0}")]
    RequestFailed(#[from] reqwest::Error),
    #[error("llm_api_error: status={status} body_preview={body_preview}")]
    ApiError {
        status: reqwest::StatusCode,
        body_preview: String,
        retry_after_secs: Option<u64>,
    },
    #[error("llm_invalid_response: {0}")]
    InvalidResponse(String),
    #[error("llm_request_construction_failed: {0}")]
    RequestConstructionFailed(String),
}

impl LlmError {
    /// Returns `true` when the error represents a transient failure that is
    /// safe to retry without side effects: a network/timeout failure before
    /// the provider returned any body, or a 429 / 5xx response.
    ///
    /// Non-retryable variants (`InvalidResponse`, `InitFailed`,
    /// `RequestConstructionFailed`, and 4xx other than 429) either indicate a
    /// deterministic problem or imply the provider already produced a response
    /// that must not be silently discarded.
    pub(crate) fn is_retryable(&self) -> bool {
        match self {
            Self::RequestFailed(_) => true,
            Self::ApiError { status, .. } => status.as_u16() == 429 || status.is_server_error(),
            Self::InitFailed(_) | Self::InvalidResponse(_) | Self::RequestConstructionFailed(_) => {
                false
            }
        }
    }
}

/// Logging subsystem initialization errors.
#[derive(Debug, Error)]
pub enum LoggingError {
    #[error("logging_init_failed: {0}")]
    InitFailed(String),
}

/// SQLite storage and session persistence errors.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage_init_failed: {0}")]
    InitFailed(String),
    #[error("storage_invalid_asset: {0}")]
    InvalidAsset(String),
    #[error("storage_io_failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage_sqlite_failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("storage_task_join_failed: {0}")]
    TaskJoin(String),
    #[error("storage_session_serialize_failed: {0}")]
    SessionSerialize(#[from] serde_json::Error),
    #[error("storage_session_snapshot_conflict")]
    SessionSnapshotConflict,
    #[error("storage_not_found: {0}")]
    NotFound(String),
    #[error("storage_conflict: {0}")]
    Conflict(String),
    /// The per-session durable-pending queue (`turn_runs` in
    /// `accepted`/`input_committed`) reached its capacity. Decided inside the
    /// same transaction as the accept INSERT, so a 429 response never leaves a
    /// runnable row behind.
    #[error("storage_turn_session_queue_full")]
    TurnSessionQueueFull,
    /// The runtime-wide durable-pending queue reached its capacity. Same
    /// in-transaction guarantee as [`Self::TurnSessionQueueFull`].
    #[error("storage_turn_global_queue_full")]
    TurnGlobalQueueFull,
    /// 同一 `turn_id + tool_call_id` で異なる input hash で claim された。
    /// 実行前に拒否し、結果を推測しない。
    #[error(
        "storage_tool_input_conflict: tool_call_id={tool_call_id} stored_hash={stored_hash} requested_hash={requested_hash}"
    )]
    ToolInputConflict {
        tool_call_id: String,
        stored_hash: String,
        requested_hash: String,
    },
    #[error(
        "storage_unsupported_schema_version: database={database} found={found} supported={supported}"
    )]
    UnsupportedSchemaVersion {
        database: &'static str,
        found: i64,
        supported: i64,
    },
    /// migration 前の DB backup に失敗した。schema 変更は開始しない。
    #[error("storage_migration_backup_failed: {detail}")]
    MigrationBackupFailed { detail: String },
}

/// Channel (Web / Discord / Telegram) operational errors.
#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("channel_not_found: {0}")]
    NotFound(String),
    #[error("channel_send_failed: {0}")]
    SendFailed(String),
    #[error("channel_cross_chat_not_allowed")]
    CrossChatNotAllowed,
}

/// MCP (Model Context Protocol) client errors.
#[derive(Debug, Error)]
pub enum McpError {
    #[error("mcp_config_read_failed: {path}: {source}")]
    ConfigReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("mcp_config_parse_failed: {path}: {detail}")]
    ConfigParseFailed { path: PathBuf, detail: String },
    #[error("mcp_connection_failed: server={server} {detail}")]
    ConnectionFailed { server: String, detail: String },
    #[error("mcp_tool_call_failed: server={server} tool={tool} {detail}")]
    ToolCallFailed {
        server: String,
        tool: String,
        detail: String,
    },
    #[error("mcp_tool_list_failed: server={server} {detail}")]
    ToolListFailed { server: String, detail: String },
}

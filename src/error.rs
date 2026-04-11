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
    #[error("shutdown_requested")]
    ShutdownRequested,
    #[error("internal_error: {0}")]
    Internal(String),
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
    #[error("missing_provider_api_key: {provider}")]
    MissingProviderApiKey { provider: String },
    #[error("invalid_compaction_config: {0}")]
    InvalidCompactionConfig(String),
    #[error("no_active_channels: no enabled channel has a valid bot_token configured")]
    NoActiveChannels,
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
    },
    #[error("llm_invalid_response: {0}")]
    InvalidResponse(String),
    #[error("llm_request_construction_failed: {0}")]
    RequestConstructionFailed(String),
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

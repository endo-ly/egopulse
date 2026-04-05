use std::path::PathBuf;

use thiserror::Error;

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
    #[error("shutdown_requested")]
    ShutdownRequested,
    #[error("internal_error: {0}")]
    Internal(String),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config_not_found: {path}")]
    ConfigNotFound { path: PathBuf },
    #[error(
        "config_auto_discovery_failed: no egopulse.config.yaml found. searched={searched_paths:?}. run 'egopulse setup', pass --config <PATH>, or set EGOPULSE_MODEL / EGOPULSE_BASE_URL / EGOPULSE_API_KEY explicitly"
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
    #[error("missing_model")]
    MissingModel,
    #[error("missing_base_url")]
    MissingBaseUrl,
    #[error("invalid_base_url")]
    InvalidBaseUrl,
    #[error("web_channel_disabled")]
    WebChannelDisabled,
    #[error("missing_web_auth_token")]
    MissingWebAuthToken,
    #[error("missing_api_key")]
    MissingApiKey,
    #[error("no_active_channels: no enabled channel has a valid bot_token configured")]
    NoActiveChannels,
}

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("tui_init_failed: {0}")]
    InitFailed(String),
    #[error("tui_render_failed: {0}")]
    RenderFailed(String),
    #[error("tui_event_failed: {0}")]
    EventFailed(String),
}

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

#[derive(Debug, Error)]
pub enum LoggingError {
    #[error("logging_init_failed: {0}")]
    InitFailed(String),
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage_init_failed: {0}")]
    InitFailed(String),
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

#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("channel_not_found: {0}")]
    NotFound(String),
    #[error("channel_send_failed: {0}")]
    SendFailed(String),
    #[error("channel_cross_chat_not_allowed")]
    CrossChatNotAllowed,
}

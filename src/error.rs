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
    Storage(#[from] StorageError),
    #[error("shutdown_requested")]
    ShutdownRequested,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config_not_found: {path}")]
    ConfigNotFound { path: PathBuf },
    #[error("config_read_failed: {path}: {source}")]
    ConfigReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config_parse_failed: {path}: {source}")]
    ConfigParseFailed {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("missing_model")]
    MissingModel,
    #[error("missing_base_url")]
    MissingBaseUrl,
    #[error("invalid_base_url")]
    InvalidBaseUrl,
    #[error("missing_api_key")]
    MissingApiKey,
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
}

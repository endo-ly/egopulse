//! Sleep batch execution and scheduling.

use thiserror::Error;

pub(crate) mod scheduler;

mod episodic_renderer;
mod event_extraction;
mod event_rollup;
mod memory_update;
mod orchestrator;
mod prompt;

#[derive(Debug, Error)]
pub enum SleepBatchError {
    #[error("already running for agent '{agent_id}'")]
    AlreadyRunning { agent_id: String },
    #[error(transparent)]
    Storage(#[from] crate::error::StorageError),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("parse failed: {0}")]
    ParseFailed(String),
    #[error("context overflow for agent '{agent_id}'")]
    ContextOverflow { agent_id: String },
    #[error("I/O error: {0}")]
    Io(String),
    #[error("unsafe agent_id: {0}")]
    UnsafeAgentId(String),
    #[error("LLM error: {0}")]
    Llm(String),
}

pub(crate) use orchestrator::recover_memory_publication;
pub use orchestrator::{run_events_extract, run_sleep_batch};

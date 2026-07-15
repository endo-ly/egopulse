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
    /// Memory publication failed but the candidate snapshot set is intact, so
    /// the run is intentionally left `running` (not failed) for a retry — by
    /// the scheduler on the next cycle, or by startup recovery. The caller
    /// observes this as an error, never as success.
    #[error("memory publication pending for run '{run_id}' (agent '{agent_id}'): {reason}")]
    PublicationPending {
        agent_id: String,
        run_id: String,
        reason: String,
    },
    /// Archiving/truncating a source session failed, so the run must NOT be
    /// finalized as `Success`. The run is intentionally left `running` so a
    /// retry converges to the same end state (already-archived sessions are
    /// skipped via a per-session `archive` checkpoint). The caller observes
    /// this as an error, never as success.
    #[error("session archive pending for run '{run_id}' (agent '{agent_id}'): {reason}")]
    ArchivePending {
        agent_id: String,
        run_id: String,
        reason: String,
    },
}

pub(crate) use orchestrator::recover_memory_publication;
pub use orchestrator::{run_events_extract, run_sleep_batch};

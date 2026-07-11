//! Repository layer: the authoritative write boundaries for the
//! conversation / turn / tool-ledger data model introduced in
//! Work Package 2. Each repository borrows the owning [`crate::storage::Database`]
//! and centralizes every persistence path for its table group.

mod conversation_store;
mod tool_execution_store;
mod turn_run_store;

pub(crate) use conversation_store::ConversationStore;
pub(crate) use tool_execution_store::{
    ClaimOutcome, ClaimParams, ToolExecutionRepository, canonical_tool_input, input_hash,
};
pub(crate) use turn_run_store::{AcceptOutcome, TurnRepository, TurnRun};

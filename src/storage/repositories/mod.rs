//! Repository layer: the authoritative write boundaries for the
//! conversation / turn / tool-ledger data model introduced in
//! Work Package 2. Each repository borrows the owning [`crate::storage::Database`]
//! and centralizes every persistence path for its table group.

mod conversation_store;

pub(crate) use conversation_store::ConversationStore;

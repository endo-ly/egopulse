//! Agent の 1 Turn 実行を構成するモジュール群。
//!
//! Turn orchestration、Agent Loop、model step、tool execution、prompt、
//! session/compactionを責務別に分離する。

pub(crate) mod compaction;
pub(crate) mod event;
pub(crate) mod loop_runner;
pub(crate) mod message_format;
pub(crate) mod model_step;
pub(crate) mod prompt;
pub(crate) mod response_guard;
pub(crate) mod session;
pub(crate) mod session_snapshot;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod tool_execution;
pub(crate) mod turn;

pub(crate) use session::{list_sessions, load_session_messages, resolve_chat_id};
pub(crate) use turn::dependencies::TurnDependencies;
#[cfg(test)]
pub(crate) use turn::process_turn;
pub use turn::{ask_in_session, ask_in_session_with_state};
pub(crate) use turn::{
    process_turn_with_events, process_turn_with_events_and_snapshot, resume_input_committed_turn,
};

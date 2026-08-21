//! 会話ターン処理とセッション解決を束ねるモジュール。
//!
//! 各チャネルから渡された surface 情報をもとに永続セッションを特定し、
//! エージェントの 1 ターン処理へ橋渡しする。

pub(crate) mod compaction;
pub(crate) mod event;
pub(crate) mod execution;
pub(crate) mod formatting;
pub(crate) mod guards;
pub(crate) mod model_step;
pub(crate) mod prompt_builder;
pub(crate) mod session;
pub(crate) mod session_snapshot;
pub(crate) mod soul_agents;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod tool_execution;
pub(crate) mod turn;
pub(crate) mod turn_runtime;

pub(crate) use session::{list_sessions, load_session_messages, resolve_chat_id};
pub use turn::ask_in_session;
pub(crate) use turn::{
    process_turn, process_turn_with_events, process_turn_with_events_and_snapshot,
    resume_input_committed_turn, send_turn,
};
pub(crate) use turn_runtime::TurnRuntime;

//! 会話ターン処理とセッション解決を束ねるモジュール。
//!
//! 各チャネルから渡された surface 情報をもとに永続セッションを特定し、
//! エージェントの 1 ターン処理へ橋渡しする。

pub(crate) mod compaction;
mod formatting;
pub(crate) mod guards;
pub(crate) mod session;
pub(crate) mod turn;

pub use session::{list_sessions, load_session_messages};
pub use turn::{ask_in_session, process_turn, process_turn_with_events, send_turn};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Identifies the external conversation surface mapped to a persisted session.
pub struct SurfaceContext {
    pub channel: String,
    pub surface_user: String,
    pub surface_thread: String,
    pub chat_type: String,
}

impl SurfaceContext {
    /// Returns the stable session key in `channel:surface_thread` format.
    pub fn session_key(&self) -> String {
        format!("{}:{}", self.channel, self.surface_thread)
    }
}

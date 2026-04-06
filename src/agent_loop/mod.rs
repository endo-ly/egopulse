pub(crate) mod session;
mod turn;

pub use session::{list_sessions, load_session_messages};
pub use turn::{ask_in_session, process_turn, process_turn_with_events, send_turn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceContext {
    pub channel: String,
    pub surface_user: String,
    pub surface_thread: String,
    pub chat_type: String,
}

impl SurfaceContext {
    pub fn session_key(&self) -> String {
        format!("{}:{}", self.channel, self.surface_thread)
    }
}

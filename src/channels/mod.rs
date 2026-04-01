pub mod cli;
pub mod registry;
pub mod tui;
pub mod web;

pub use registry::{ChannelAdapter, ChannelRegistry, ConversationKind};
pub use web::WebAdapter;

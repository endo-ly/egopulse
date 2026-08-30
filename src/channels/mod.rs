//! チャネル実装群。
//!
//! ローカル TUI と、feature 有効時の Discord / Telegram アダプターを提供し、
//! 各入力面を共通の agent runtime に接続する。

pub(crate) mod adapter;
#[cfg(feature = "channel-discord")]
pub(crate) mod discord;
#[cfg(feature = "channel-telegram")]
pub(crate) mod telegram;
pub(crate) mod tui;
pub(crate) mod utils;
pub(crate) mod voice;
pub(crate) mod web;

/// Runs the local TUI as a client of the shared runtime.
pub async fn run_tui(
    socket_path: std::path::PathBuf,
    session: Option<&str>,
) -> Result<(), crate::error::EgoPulseError> {
    tui::run(socket_path, session).await
}

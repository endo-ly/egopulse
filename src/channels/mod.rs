//! チャネル実装群。
//!
//! ローカル CLI / TUI と、feature 有効時の Discord / Telegram アダプターを提供し、
//! 各入力面を共通の agent runtime に接続する。

pub mod cli;
#[cfg(feature = "channel-discord")]
pub mod discord;
#[cfg(feature = "channel-telegram")]
pub mod telegram;
pub mod tui;

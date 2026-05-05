//! チャネル実装群。
//!
//! ローカル CLI / TUI と、feature 有効時の Discord / Telegram アダプターを提供し、
//! 各入力面を共通の agent runtime に接続する。

pub(crate) mod adapter;
pub mod cli;
#[cfg(feature = "channel-discord")]
pub(crate) mod discord;
#[cfg(feature = "channel-telegram")]
pub(crate) mod telegram;
pub(crate) mod tui;
pub(crate) mod utils;
pub(crate) mod web;

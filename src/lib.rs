//! EgoPulse クレート。
//!
//! 永続セッションを共有する AI エージェントランタイムとして、TUI / Web / Discord / Telegram
//! の各チャネルと設定・ストレージ・エージェント実行基盤をまとめて提供する。
//!
//! 公開 API は [`app`] モジュールに集約されている。バイナリ以外の利用者は
//! そこから必要なアイテムだけを参照すること。

pub mod app;
pub mod agent_loop;
pub(crate) mod assets;
pub(crate) mod builtin_skills;
pub mod channels;
pub mod config;
pub mod error;
pub(crate) mod llm;
pub(crate) mod memory;
pub(crate) mod pulse;
pub mod runtime;
pub mod setup;
pub(crate) mod skills;
pub(crate) mod slash_commands;
pub mod sleep;
pub mod storage;
pub(crate) mod tools;

#[cfg(test)]
mod test_env;

#[cfg(test)]
pub mod test_util;

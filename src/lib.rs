//! EgoPulse クレート。
//!
//! 永続セッションを共有する AI エージェントランタイムとして、TUI / Web / Discord / Telegram
//! の各チャネルと設定・ストレージ・エージェント実行基盤をまとめて提供する。
//!
//! 公開 API は [`app`] モジュールに集約されている。バイナリ以外の利用者は
//! そこから必要なアイテムだけを参照すること。

pub mod app;
pub(crate) mod agent_loop;
pub(crate) mod assets;
pub(crate) mod builtin_skills;
pub(crate) mod channels;
pub(crate) mod config;
pub(crate) mod error;
pub(crate) mod llm;
pub(crate) mod memory;
pub(crate) mod pulse;
pub(crate) mod runtime;
pub(crate) mod setup;
pub(crate) mod skills;
pub(crate) mod slash_commands;
pub(crate) mod sleep;
pub(crate) mod storage;
pub(crate) mod tools;

#[cfg(test)]
mod test_env;

#[cfg(test)]
pub mod test_util;

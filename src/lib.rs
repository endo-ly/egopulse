//! EgoPulse クレート。
//!
//! 永続セッションを共有する AI エージェントランタイムとして、TUI / Web / Discord / Telegram
//! の各チャネルと設定・ストレージ・エージェント実行基盤をまとめて提供する。

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
pub mod sleep_batch;
pub(crate) mod sleep_scheduler;
pub(crate) mod soul_agents;
pub mod storage;
pub(crate) mod tools;

#[cfg(test)]
mod test_env;

#[cfg(test)]
pub mod test_util;

//! EgoPulse クレート。
//!
//! 永続セッションを共有する AI エージェントランタイムとして、TUI / Web / Discord / Telegram
//! の各チャネルと設定・ストレージ・エージェント実行基盤をまとめて提供する。

pub mod agent_loop;
pub mod assets;
pub mod channel_adapter;
pub mod channels;
pub mod config;
pub mod config_store;
pub mod error;
pub mod gateway;
pub mod llm;
pub mod llm_profile;
pub mod logging;
pub mod mcp;
pub mod runtime;
pub mod setup;
pub mod skills;
pub mod slash_commands;
pub mod soul_agents;
pub mod status;
pub mod storage;
pub mod text;
pub mod tools;
pub mod web;

#[cfg(test)]
mod test_env;

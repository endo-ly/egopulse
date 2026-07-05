//! Web SSE 向けのイベント型を agent_loop から再公開するモジュール。
//!
//! [`crate::agent_loop::event::AgentEvent`] が正統な定義場所。
//! Web チャネルは SSE ペイロード生成のために本モジュール経由で参照する。

pub(crate) use crate::agent_loop::event::AgentEvent;

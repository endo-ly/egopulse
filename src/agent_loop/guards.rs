//! ランタイムガード: 宣言のみや空応答の検出とリトライメッセージ構築。

use crate::llm::Message;

pub(crate) fn runtime_guard_messages(
    messages: &[Message],
    assistant_text: &str,
    guard_text: &str,
) -> Vec<Message> {
    let mut retry_messages = messages.to_vec();
    retry_messages.push(Message::text("assistant", assistant_text.to_string()));
    retry_messages.push(Message::text("user", guard_text.to_string()));
    retry_messages
}

/// レスポンスが「宣言だけしてツールを実行しない」パターンに一致するか判定する。
/// microclaw の runtime guard パターンから移植。
pub(crate) fn is_declarative_only_reply(text: &str) -> bool {
    let lower = text.to_lowercase();
    let english_patterns = [
        "i'll ",
        "i will ",
        "i'll go ahead and ",
        "let me ",
        "sure, ",
        "of course, ",
        "i'd be happy to ",
        "absolutely, ",
        "i can help with that",
        "great, i'll",
        "alright, i'll",
        "okay, i'll",
        "sure thing",
        "i'm on it",
    ];
    let japanese_prefixes = [
        "了解しました",
        "承知しました",
        "わかりました",
        "かしこまりました",
        "はい、",
        "では、",
        "それでは、",
        "今から",
    ];
    let japanese_action_markers = [
        "実行します",
        "確認します",
        "試してみます",
        "やってみます",
        "見てみます",
        "調べます",
        "作成します",
        "書き込みます",
        "進めます",
    ];
    // 短いテキスト（≤200文字）だけを対象とする。長い応答は「宣言のみ」ではない。
    if lower.trim().chars().count() > 200 {
        return false;
    }
    let trimmed = text.trim();
    english_patterns.iter().any(|p| lower.trim().starts_with(p))
        || japanese_prefixes.iter().any(|p| trimmed.starts_with(p))
        || japanese_action_markers.iter().any(|p| trimmed.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::process_turn;
    use crate::agent_loop::turn::{FakeProvider, build_state_with_provider, cli_context};
    use crate::error::EgoPulseError;
    use crate::llm::{MessagesResponse, ToolCall};
    use serial_test::serial;
    use std::sync::Arc;

    #[test]
    fn is_declarative_only_reply_detects_patterns() {
        assert!(is_declarative_only_reply("I'll help you with that."));
        assert!(is_declarative_only_reply("Sure, let me check that."));
        assert!(is_declarative_only_reply("Of course, I can do that."));
        assert!(is_declarative_only_reply("Let me look into that."));
        assert!(is_declarative_only_reply("了解しました。実行します。"));
        assert!(is_declarative_only_reply("承知しました。確認します。"));
        assert!(is_declarative_only_reply("今から試してみます。"));

        let long = "I'll help you with that. ".repeat(20);
        assert!(!is_declarative_only_reply(&long));

        assert!(!is_declarative_only_reply(
            "The file contains the following:"
        ));
        assert!(!is_declarative_only_reply(
            "Here is the result of the search:"
        ));
    }

    #[tokio::test]
    #[serial]
    async fn empty_reply_guard_retries_once_then_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: String::new(),
                        tool_calls: Vec::new(),
                    },
                    MessagesResponse {
                        content: String::new(),
                        tool_calls: Vec::new(),
                    },
                ]),
            }),
        );

        let error = process_turn(&state, &cli_context("empty-guard"), "hello")
            .await
            .expect_err("should fail after retry");
        assert!(matches!(error, EgoPulseError::Llm(_)));
    }

    #[tokio::test]
    #[serial]
    async fn declarative_only_guard_retries_then_returns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: "Sure, I'll help you with that.".to_string(),
                        tool_calls: Vec::new(),
                    },
                    MessagesResponse {
                        content: "Here is the answer you need.".to_string(),
                        tool_calls: Vec::new(),
                    },
                ]),
            }),
        );

        let reply = process_turn(&state, &cli_context("declarative-guard"), "help me")
            .await
            .expect("should succeed after retry");
        assert_eq!(reply, "Here is the answer you need.");

        let chat_id = crate::storage::call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:declarative-guard",
                Some("declarative-guard"),
                "cli",
            )
        })
        .await
        .expect("chat id");
        let loaded = crate::agent_loop::session::load_messages_for_turn(&state, chat_id)
            .await
            .expect("loaded session");
        assert!(
            loaded
                .messages
                .iter()
                .all(|message| !message.content.as_text_lossy().contains("[runtime_guard]"))
        );
    }

    #[tokio::test]
    #[serial]
    async fn malformed_declarative_tool_reply_retries_then_returns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: "了解しました。実行します。".to_string(),
                        tool_calls: vec![ToolCall {
                            id: "call-malformed".to_string(),
                            name: String::new(),
                            arguments: serde_json::json!({}),
                        }],
                    },
                    MessagesResponse {
                        content: "実行結果です。".to_string(),
                        tool_calls: Vec::new(),
                    },
                ]),
            }),
        );

        let reply = process_turn(&state, &cli_context("malformed-declarative"), "test")
            .await
            .expect("should recover after retry");
        assert_eq!(reply, "実行結果です。");
    }
}

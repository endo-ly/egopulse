//! スラッシュコマンドの検出・ディスパッチ。
//!
//! Microclaw 準拠の公開 API を提供し、各チャネルから渡されたコマンドテキストを
//! 対応するハンドラに振り分ける。`is_slash_command` で判定し、
//! `handle_slash_command` で実行結果のメッセージを返す。

use std::sync::Arc;

use crate::agent_loop::SurfaceContext;
use crate::agent_loop::compaction::force_compact;
use crate::agent_loop::session::load_messages_for_turn;
use crate::llm_profile;
use crate::runtime::AppState;
use crate::storage::call_blocking;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// 入力テキストがスラッシュコマンドかどうかを判定する。
///
/// 先頭のメンション (`@botname `, `<@U123456> `, `@bot/command`) を除去した後、
/// 残りのテキストが `/` で始まる場合に `true` を返す。
/// `//` (二重スラッシュ) や単独の `/` はコマンドとは見なさない。
pub fn is_slash_command(text: &str) -> bool {
    let normalized = strip_mentions(text.trim());
    if normalized.is_empty() {
        return false;
    }
    if !normalized.starts_with('/') {
        return false;
    }
    // `//` を除外
    if normalized.starts_with("//") {
        return false;
    }
    // `/` 単独 (引数なし) を除外
    if normalized.len() == 1 {
        return false;
    }
    // `/ ` のようにスペースのみ続く場合も除外
    let trimmed = normalized.trim();
    if trimmed.len() == 1 {
        return false;
    }
    true
}

/// スラッシュコマンドを実行し、結果メッセージを返す。
///
/// コマンドが未知または空の場合は `None` を返す。
pub async fn handle_slash_command(
    state: &AppState,
    chat_id: i64,
    caller_channel: &str,
    command_text: &str,
    sender_id: Option<&str>,
) -> Option<String> {
    let normalized = strip_mentions(command_text.trim());
    if normalized.is_empty() {
        return None;
    }

    let parts: Vec<&str> = normalized.splitn(2, char::is_whitespace).collect();
    let raw_command = parts.first().copied().unwrap_or("");
    let bare_command = raw_command
        .split_once('@')
        .map(|(cmd, _)| cmd)
        .unwrap_or(raw_command);
    let command = bare_command.to_ascii_lowercase();
    let _args = parts.get(1).copied().unwrap_or("");

    match command.as_str() {
        "/new" => handle_new(state, chat_id).await,
        "/compact" => handle_compact(state, chat_id, caller_channel).await,
        "/status" => handle_status(state, chat_id, caller_channel, sender_id).await,
        "/skills" => Some(handle_skills(state)),
        "/restart" => Some(handle_restart()),
        "/providers" | "/provider" | "/models" | "/model" => {
            handle_llm_profile(state, caller_channel, normalized).await
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Mention stripping
// ---------------------------------------------------------------------------

/// 先頭のメンションプレフィクスを除去する。
///
/// パターン:
/// - `@botname ` (Telegram スタイル)
/// - `<@U123456> ` (Discord スタイル)
/// - `@botname/command` (メンションとスラッシュの間にスペースなし)
fn strip_mentions(text: &str) -> &str {
    let mut rest = text;

    // Discord スタイル: `<@...> `
    if let Some(end) = rest.find('>') {
        let prefix = &rest[..=end];
        if prefix.starts_with("<@")
            && prefix
                .chars()
                .all(|c| c == '<' || c == '@' || c == '>' || c.is_ascii_alphanumeric())
        {
            rest = rest[end + 1..].trim_start();
        }
    }

    // Telegram スタイル: `@botname ` または `@botname/command`
    if rest.starts_with('@') {
        // 次のスペースまたは `/` までをメンションとして扱う
        let mention_end = rest.find([' ', '/']).unwrap_or(rest.len());
        if mention_end > 1 {
            // `@` の後に少なくとも1文字ある
            let after_mention = &rest[mention_end..];
            if after_mention.starts_with('/') {
                // `@bot/command` パターン: メンション部分だけ除去
                rest = after_mention;
            } else if after_mention.starts_with(' ') {
                rest = after_mention.trim_start();
            } else {
                // `@bot` のみ → メンションはコマンドではない
                // (テキスト全体が `@bot` のような場合)
                rest = after_mention.trim_start();
            }
        }
    }

    rest
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

async fn handle_new(state: &AppState, chat_id: i64) -> Option<String> {
    match call_blocking(Arc::clone(&state.db), move |db| db.clear_session(chat_id)).await {
        Ok(_) => Some("Session cleared.".to_string()),
        Err(e) => {
            tracing::warn!("failed to clear session: {e}");
            Some(format!("Failed to clear session: {e}"))
        }
    }
}

async fn handle_compact(state: &AppState, chat_id: i64, caller_channel: &str) -> Option<String> {
    let loaded = match load_messages_for_turn(state, chat_id).await {
        Ok(loaded) => loaded,
        Err(e) => return Some(format!("Failed to load session: {e}")),
    };
    if loaded.messages.is_empty() {
        return Some("Session is empty.".to_string());
    }

    let count = loaded.messages.len();
    let context = SurfaceContext {
        channel: caller_channel.to_string(),
        surface_user: String::new(),
        surface_thread: String::new(),
        chat_type: String::new(),
    };
    let llm = match state.global_llm() {
        Ok(llm) => llm,
        Err(e) => return Some(format!("Failed to get LLM provider: {e}")),
    };

    match force_compact(state, &context, chat_id, &loaded.messages, &llm).await {
        Ok(compacted) => {
            let json = serde_json::to_string(&compacted).ok()?;
            call_blocking(Arc::clone(&state.db), move |db| {
                db.save_session(chat_id, &json)
            })
            .await
            .ok()?;
            Some(format!(
                "Compacted {count} messages to {}.",
                compacted.len()
            ))
        }
        Err(error) => Some(format!("Compaction failed: {error}")),
    }
}

async fn handle_status(
    state: &AppState,
    chat_id: i64,
    caller_channel: &str,
    sender_id: Option<&str>,
) -> Option<String> {
    let config = match state.try_current_config() {
        Ok(config) => config,
        Err(e) => return Some(format!("Failed to load config: {e}")),
    };
    let resolved = match config.resolve_llm_for_channel(caller_channel) {
        Ok(r) => r,
        Err(e) => return Some(format!("Failed to resolve LLM: {e}")),
    };

    let messages = call_blocking(Arc::clone(&state.db), move |db| {
        db.get_recent_messages(chat_id, 99999)
    })
    .await
    .unwrap_or_default();

    let session_line = if messages.is_empty() {
        "Session: empty".to_string()
    } else {
        format!("Session: active ({} messages)", messages.len())
    };

    let mut status = format!(
        "Status\n\
         Channel: {caller_channel}\n\
         Provider: {}\n\
         Model: {}\n\
         {session_line}",
        resolved.provider, resolved.model,
    );

    if let Some(id) = sender_id {
        status.push_str(&format!("\nSender: {id}"));
    }

    Some(status)
}

fn handle_skills(state: &AppState) -> String {
    state.skills.list_skills_formatted()
}

fn handle_restart() -> String {
    "Not implemented yet.".to_string()
}

async fn handle_llm_profile(state: &AppState, caller_channel: &str, input: &str) -> Option<String> {
    let context = SurfaceContext {
        channel: caller_channel.to_string(),
        surface_user: String::new(),
        surface_thread: String::new(),
        chat_type: caller_channel.to_string(),
    };
    llm_profile::handle_command(state, &context, input)
        .await
        .ok()?
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::agent_loop::turn::{build_state, test_config};
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::runtime::AppState;
    use crate::storage::{StoredMessage, call_blocking};

    use super::{handle_slash_command, is_slash_command};

    // -- is_slash_command tests ---------------------------------------------------

    #[test]
    fn is_slash_basic() {
        assert!(is_slash_command("/status"));
    }

    #[test]
    fn is_slash_with_args() {
        assert!(is_slash_command("/model gpt-5"));
    }

    #[test]
    fn is_slash_telegram_mention() {
        assert!(is_slash_command("@mybot /status"));
    }

    #[test]
    fn is_slash_discord_mention() {
        assert!(is_slash_command("<@U123456> /status"));
    }

    #[test]
    fn is_slash_mention_no_space() {
        assert!(is_slash_command("@bot/status"));
    }

    #[test]
    fn is_slash_plain_text() {
        assert!(!is_slash_command("hello world"));
    }

    #[test]
    fn is_slash_empty() {
        assert!(!is_slash_command(""));
    }

    #[test]
    fn is_slash_mention_only() {
        assert!(!is_slash_command("@bot"));
    }

    #[test]
    fn is_slash_double_slash() {
        assert!(!is_slash_command("// comment"));
    }

    #[test]
    fn is_slash_case_insensitive() {
        assert!(is_slash_command("/STATUS"));
    }

    // -- Test helper: no-op LLM provider -----------------------------------------

    struct NoOpProvider;

    #[async_trait]
    impl LlmProvider for NoOpProvider {
        async fn send_message(
            &self,
            _system: &str,
            _messages: Vec<Message>,
            _tools: Option<Vec<crate::llm::ToolDefinition>>,
        ) -> Result<MessagesResponse, LlmError> {
            Ok(MessagesResponse {
                content: "summary".to_string(),
                tool_calls: Vec::new(),
            })
        }
    }

    fn build_test_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8").to_string());
        let state = build_state(config, Box::new(NoOpProvider));
        (state, dir)
    }

    // -- handle_slash_command tests -----------------------------------------------

    #[tokio::test]
    async fn handle_new_clears_session() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = call_blocking(Arc::clone(&state.db), |db| {
            db.resolve_or_create_chat_id("cli", "cli:test-new", Some("test-new"), "cli")
        })
        .await
        .expect("chat_id");

        call_blocking(Arc::clone(&state.db), {
            move |db| {
                db.store_message(&StoredMessage {
                    id: "msg-1".to_string(),
                    chat_id,
                    sender_name: "user".to_string(),
                    content: "hello".to_string(),
                    is_from_bot: false,
                    timestamp: "2024-01-01T00:00:00Z".to_string(),
                })
            }
        })
        .await
        .expect("store message");

        // Act
        let result = handle_slash_command(&state, chat_id, "cli", "/new", None).await;

        // Assert
        assert_eq!(result, Some("Session cleared.".to_string()));
        let messages = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_recent_messages(chat_id, 10)
        })
        .await
        .expect("messages");
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn handle_compact_returns_count() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = call_blocking(Arc::clone(&state.db), |db| {
            db.resolve_or_create_chat_id("cli", "cli:test-compact", Some("test-compact"), "cli")
        })
        .await
        .expect("chat_id");

        let messages = vec![
            Message::text("user", "hello"),
            Message::text("assistant", "hi"),
        ];
        let json = serde_json::to_string(&messages).expect("json");
        call_blocking(Arc::clone(&state.db), {
            move |db| db.save_session(chat_id, &json)
        })
        .await
        .expect("save session");

        // Act
        let result = handle_slash_command(&state, chat_id, "cli", "/compact", None).await;

        // Assert
        let response = result.expect("response");
        assert!(response.contains("Compacted"), "response: {response}");
        assert!(response.contains("2 messages"), "response: {response}");
    }

    #[tokio::test]
    async fn handle_status_shows_info() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = call_blocking(Arc::clone(&state.db), |db| {
            db.resolve_or_create_chat_id("cli", "cli:test-status", Some("test-status"), "cli")
        })
        .await
        .expect("chat_id");

        // Act
        let result =
            handle_slash_command(&state, chat_id, "cli", "/status", Some("user-123")).await;

        // Assert
        let response = result.expect("response");
        assert!(response.contains("Channel: cli"), "response: {response}");
        assert!(response.contains("Provider:"), "response: {response}");
        assert!(response.contains("Model:"), "response: {response}");
        assert!(response.contains("Session:"), "response: {response}");
        assert!(
            response.contains("Sender: user-123"),
            "response: {response}"
        );
    }

    #[tokio::test]
    async fn handle_skills_empty() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = 1;

        // Act
        let result = handle_slash_command(&state, chat_id, "cli", "/skills", None).await;

        // Assert
        let response = result.expect("response");
        assert!(
            response.contains("No skills loaded."),
            "response: {response}"
        );
    }

    #[tokio::test]
    async fn handle_providers_delegates_to_llm_profile() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = 1;

        // Act
        let result = handle_slash_command(&state, chat_id, "cli", "/providers", None).await;

        // Assert
        let response = result.expect("response");
        assert!(
            response.contains("openai"),
            "response should contain provider name: {response}"
        );
    }

    #[tokio::test]
    async fn handle_unknown_returns_none() {
        // Arrange
        let (state, _dir) = build_test_state();

        // Act
        let result = handle_slash_command(&state, 1, "cli", "/foo", None).await;

        // Assert
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn handle_restart_returns_message() {
        // Arrange
        let (state, _dir) = build_test_state();

        // Act
        let result = handle_slash_command(&state, 1, "cli", "/restart", None).await;

        // Assert
        let response = result.expect("response");
        assert!(response.contains("Not implemented"), "response: {response}");
    }

    #[tokio::test]
    async fn handle_status_empty_session() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = 999;

        // Act
        let result = handle_slash_command(&state, chat_id, "cli", "/status", None).await;

        // Assert
        let response = result.expect("response");
        assert!(response.contains("Session: empty"), "response: {response}");
    }

    #[tokio::test]
    async fn handle_compact_empty_session() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = 998;

        // Act
        let result = handle_slash_command(&state, chat_id, "cli", "/compact", None).await;

        // Assert
        // load_messages_for_turn は chat_id を直接受け取るため、
        // チャット行が存在しなくても空セッションとして返す
        let response = result.expect("response");
        assert!(
            response.contains("Session is empty"),
            "response: {response}"
        );
    }
}

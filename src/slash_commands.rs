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
/// 先頭のメンションをループで除去した後、残りのテキストが `/` で始まる場合に
/// `true` を返す。`//` (二重スラッシュ) や単独の `/` はコマンドとは見なさない。
pub fn is_slash_command(text: &str) -> bool {
    let Some(normalized) = normalized_slash_command(text) else {
        return false;
    };
    // `//` を除外
    if normalized.starts_with("//") {
        return false;
    }
    // `/` 単独 (引数なし) を除外
    if normalized.len() == 1 {
        return false;
    }
    // `/ ` のようにスペースのみ続く場合も除外
    if normalized.trim().len() == 1 {
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
    let normalized = normalized_slash_command(command_text)?;

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
// 正規化
// ---------------------------------------------------------------------------

/// 先頭のメンションをループで除去し、スラッシュコマンド部分を返す。
///
/// 対応パターン:
/// - `<@U123456>` (Discord スタイル) — 複数回出現してもループで全て除去
/// - `@botname` (Telegram スタイル) — 複数回出現してもループで全て除去
fn normalized_slash_command(text: &str) -> Option<&str> {
    let mut s = text.trim_start();
    loop {
        if s.starts_with('/') {
            return Some(s);
        }
        if s.starts_with("<@") {
            let end = s.find('>')?;
            s = s[end + 1..].trim_start();
            continue;
        }
        if let Some(rest) = s.strip_prefix('@') {
            if rest.is_empty() {
                return None;
            }
            let end = rest
                .char_indices()
                .find(|(_, c)| c.is_whitespace() || *c == '/')
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            s = rest[end..].trim_start();
            continue;
        }
        return None;
    }
}

/// 未知コマンド時の応答メッセージを返す。
pub fn unknown_command_response() -> String {
    "Unknown command.".to_string()
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
            let json = match serde_json::to_string(&compacted) {
                Ok(j) => j,
                Err(e) => return Some(format!("Failed to serialize compacted session: {e}")),
            };
            match call_blocking(Arc::clone(&state.db), move |db| {
                db.save_session(chat_id, &json)
            })
            .await
            {
                Ok(_) => Some(format!(
                    "Compacted {count} messages to {}.",
                    compacted.len()
                )),
                Err(e) => Some(format!(
                    "Compacted {count} messages but failed to save session: {e}"
                )),
            }
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
    match llm_profile::handle_command(state, &context, input).await {
        Ok(result) => result,
        Err(e) => Some(format!("LLM profile error: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Command Metadata Registry
// ---------------------------------------------------------------------------

/// スラッシュコマンド定義のメタデータ。
///
/// 各チャネル (Telegram, Discord, WebUI) はこのレジストリを通じて
/// コマンド名・説明・使用法を参照する。
pub struct CommandDef {
    /// コマンド名（`/` なし）。
    pub name: &'static str,
    /// コマンドの短い説明。
    pub description: &'static str,
    /// 使用例（`/` で始まる）。
    pub usage: &'static str,
}

/// 登録済みコマンド一覧を返す。
pub const fn all_commands() -> &'static [CommandDef] {
    &[
        CommandDef {
            name: "new",
            description: "Clear current session",
            usage: "/new",
        },
        CommandDef {
            name: "compact",
            description: "Force compact session",
            usage: "/compact",
        },
        CommandDef {
            name: "status",
            description: "Show current status",
            usage: "/status",
        },
        CommandDef {
            name: "skills",
            description: "List available skills",
            usage: "/skills",
        },
        CommandDef {
            name: "restart",
            description: "Restart the bot",
            usage: "/restart",
        },
        CommandDef {
            name: "providers",
            description: "List LLM providers",
            usage: "/providers",
        },
        CommandDef {
            name: "provider",
            description: "Show/switch provider",
            usage: "/provider [name]",
        },
        CommandDef {
            name: "models",
            description: "List models",
            usage: "/models",
        },
        CommandDef {
            name: "model",
            description: "Show/switch model",
            usage: "/model [name]",
        },
    ]
}

/// 名前から CommandDef を検索する。
pub fn find_command(name: &str) -> Option<&'static CommandDef> {
    all_commands().iter().find(|c| c.name == name)
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

    use super::{all_commands, find_command, handle_slash_command, is_slash_command};

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

    #[test]
    fn is_slash_multiple_mentions() {
        assert!(is_slash_command("<@U123>   @bot   /status"));
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

    // -- CommandDef registry tests -----------------------------------------------

    #[test]
    fn all_commands_returns_all() {
        // Arrange & Act
        let commands = all_commands();

        // Assert: 9 コマンドが返る
        assert_eq!(commands.len(), 9);
        let names: Vec<&str> = commands.iter().map(|c| c.name).collect();
        assert!(names.contains(&"new"));
        assert!(names.contains(&"compact"));
        assert!(names.contains(&"status"));
        assert!(names.contains(&"skills"));
        assert!(names.contains(&"restart"));
        assert!(names.contains(&"providers"));
        assert!(names.contains(&"provider"));
        assert!(names.contains(&"models"));
        assert!(names.contains(&"model"));
    }

    #[test]
    fn all_commands_has_valid_metadata() {
        // Arrange & Act
        let commands = all_commands();

        // Assert: 各 CommandDef のメタデータが有効
        for cmd in commands {
            assert!(!cmd.name.is_empty(), "name must not be empty");
            assert!(
                !cmd.description.is_empty(),
                "description for '{}' must not be empty",
                cmd.name
            );
            assert!(
                !cmd.usage.is_empty(),
                "usage for '{}' must not be empty",
                cmd.name
            );
            assert!(
                cmd.usage.starts_with('/'),
                "usage for '{}' must start with '/': got '{}'",
                cmd.name,
                cmd.usage
            );
        }
    }

    #[test]
    fn all_commands_names_are_unique() {
        // Arrange & Act
        let commands = all_commands();

        // Assert: name が重複しない
        let names: Vec<&str> = commands.iter().map(|c| c.name).collect();
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(names.len(), unique.len());
    }

    #[test]
    fn find_command_by_name_known() {
        // Arrange & Act
        let found = find_command("status");

        // Assert
        let cmd = found.expect("status command should exist");
        assert_eq!(cmd.name, "status");
        assert_eq!(cmd.usage, "/status");
    }

    #[test]
    fn find_command_by_name_unknown() {
        // Arrange & Act
        let found = find_command("nonexistent");

        // Assert
        assert!(found.is_none());
    }
}

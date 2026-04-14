//! Telegram チャネルアダプター。
//!
//! teloxide 0.17 を用いて Telegram Bot API (long polling) からメッセージを受信し、
//! EgoPulse agent runtime で処理した結果を Telegram に返信する。
//!
//! Based on: microclaw `src/channels/telegram.rs`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, MessageEntityKind};
use tracing::{debug, error, info, warn};

use crate::agent_loop::SurfaceContext;
use crate::channel_adapter::ChannelAdapter;
use crate::channel_adapter::ConversationKind;
use crate::runtime::AppState;
use crate::slash_commands;
use crate::storage::call_blocking;
use crate::text::split_text;

/// Telegram メッセージ長制限 (文字数)。
const TELEGRAM_MAX_MESSAGE_LEN: usize = 4096;

/// タイピングインジケーターの送信間隔。
const TYPING_INTERVAL_SECS: u64 = 4;

/// bot_username 未設定警告の一度だけ出力フラグ。
static BOT_USERNAME_WARN_EMITTED: AtomicBool = AtomicBool::new(false);

/// Telegram チャネルアダプター。
///
/// アウトバウンドメッセージ送信用。Bot API 経由で Telegram にメッセージを送信する。
pub struct TelegramAdapter {
    bot: Bot,
}

impl TelegramAdapter {
    /// Creates a Telegram adapter backed by the provided bot client.
    pub fn new(bot: Bot) -> Self {
        Self { bot }
    }
}

#[async_trait]
impl ChannelAdapter for TelegramAdapter {
    fn name(&self) -> &str {
        "telegram"
    }

    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
        // microclaw パターン: concrete な chat_type を全て登録
        vec![
            ("telegram_private", ConversationKind::Private),
            ("private", ConversationKind::Private),
            ("telegram_group", ConversationKind::Group),
            ("group", ConversationKind::Group),
            ("supergroup", ConversationKind::Group),
            ("channel", ConversationKind::Group),
            ("telegram_supergroup", ConversationKind::Group),
            ("telegram_channel", ConversationKind::Group),
        ]
    }

    async fn send_text(&self, external_chat_id: &str, text: &str) -> Result<(), String> {
        let chat_id = parse_telegram_chat_id(external_chat_id)?;

        const MAX_RETRIES: u32 = 3;

        for chunk in split_text(text, TELEGRAM_MAX_MESSAGE_LEN) {
            let mut attempt = 0;
            loop {
                match self.bot.send_message(ChatId(chat_id), &chunk).await {
                    Ok(_) => break,
                    Err(e) => {
                        if let teloxide::RequestError::RetryAfter(seconds) = &e {
                            if attempt < MAX_RETRIES {
                                let wait = seconds.duration();
                                debug!(
                                    attempt = attempt + 1,
                                    retry_after = wait.as_secs(),
                                    "Telegram rate limited, retrying after {wait:?}"
                                );
                                tokio::time::sleep(wait).await;
                                attempt += 1;
                                continue;
                            }
                        }
                        return Err(format!("Telegram send_message failed: {e}"));
                    }
                }
            }
        }

        Ok(())
    }
}

/// Telegram メッセージハンドラ。
///
/// microclaw `src/channels/telegram.rs::handle_message` と同じパターン。
async fn handle_message(
    bot: Bot,
    msg: teloxide::types::Message,
    state: Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // テキストメッセージのみ処理
    let Some(text) = msg.text().map(str::to_string) else {
        return Ok(());
    };

    if text.is_empty() {
        return Ok(());
    }

    let raw_chat_id = msg.chat.id.0;

    // 送信者名を取得
    let sender_name = msg
        .from
        .as_ref()
        .map(|u| u.username.clone().unwrap_or_else(|| u.first_name.clone()))
        .unwrap_or_else(|| "unknown".to_string());

    // チャット種別判定 — microclaw パターンに準拠
    let chat_type = match &msg.chat.kind {
        teloxide::types::ChatKind::Private(_) => "telegram_private".to_string(),
        teloxide::types::ChatKind::Public(teloxide::types::ChatPublic {
            kind: teloxide::types::PublicChatKind::Group,
            ..
        }) => "telegram_group".to_string(),
        teloxide::types::ChatKind::Public(teloxide::types::ChatPublic {
            kind: teloxide::types::PublicChatKind::Supergroup(_),
            ..
        }) => "telegram_supergroup".to_string(),
        teloxide::types::ChatKind::Public(teloxide::types::ChatPublic {
            kind: teloxide::types::PublicChatKind::Channel(_),
            ..
        }) => {
            return Ok(());
        }
    };

    // アクセス制御: DM の場合、allowed_user_ids が設定されていればチェック
    if chat_type == "telegram_private" {
        if let Some(allowed_ids) = state
            .config
            .channels
            .get("telegram")
            .and_then(|c| c.allowed_user_ids.as_ref())
        {
            if !allowed_ids.is_empty() {
                let sender_id = msg.from.as_ref().and_then(|u| i64::try_from(u.id.0).ok());
                if !sender_id.is_some_and(|id| allowed_ids.contains(&id)) {
                    debug!(
                        chat_id = raw_chat_id,
                        "Telegram: rejecting unauthorized user in private chat"
                    );
                    return Ok(());
                }
            }
        }
    }

    let is_group = chat_type != "telegram_private";
    let external_chat_id = raw_chat_id.to_string();

    // グループメンション判定 (スラッシュコマンドと通常メッセージで共用)。
    // BotCommand エンティティの @botname またはテキストメンションのいずれかで true。
    let is_mentioned_in_group = if !is_group {
        true
    } else {
        let bot_username = state.config.telegram_bot_username();
        let msg_text = text.as_str();
        let is_own_command = msg
            .entities()
            .unwrap_or_default()
            .iter()
            .filter(|e| matches!(e.kind, MessageEntityKind::BotCommand))
            .any(|e| {
                let start = e.offset;
                let end = start + e.length;
                let cmd_text = msg_text.get(start..end).unwrap_or("");
                if let Some(at_pos) = cmd_text.find('@') {
                    let mention = &cmd_text[at_pos + 1..];
                    bot_username.is_some_and(|u| mention.eq_ignore_ascii_case(u))
                } else {
                    bot_username.is_some()
                }
            });

        if is_own_command {
            true
        } else {
            match &bot_username {
                Some(username) => msg
                    .parse_entities()
                    .into_iter()
                    .flatten()
                    .filter(|e| matches!(e.kind(), MessageEntityKind::Mention))
                    .any(|e| {
                        e.text()
                            .strip_prefix('@')
                            .is_some_and(|m| m.eq_ignore_ascii_case(username))
                    }),
                None => {
                    if BOT_USERNAME_WARN_EMITTED
                        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                    {
                        warn!(
                            chat_id = raw_chat_id,
                            "telegram_bot_username not set; group messages will be ignored"
                        );
                    }
                    false
                }
            }
        }
    };

    // --- スラッシュコマンドインターセプト ---
    // process_turn より先に chat_id を解決し、
    // スラッシュコマンドであればエージェントループに入らずに即応答する。
    if slash_commands::is_slash_command(&text) {
        // グループではメンション必須
        if !is_mentioned_in_group {
            debug!(
                chat_id = raw_chat_id,
                "Telegram: skipping non-mentioned slash command in group"
            );
            return Ok(());
        }

        let resolved_chat_id = match call_blocking(std::sync::Arc::clone(&state.db), {
            let channel = "telegram".to_string();
            let ext_id = external_chat_id.clone();
            move |db| db.resolve_or_create_chat_id(&channel, &ext_id, None, &chat_type)
        })
        .await
        {
            Ok(id) => id,
            Err(e) => {
                error!("failed to resolve chat_id for slash command: {e}");
                send_telegram_response(
                    &bot,
                    msg.chat.id,
                    "An error occurred processing the command.",
                )
                .await;
                return Ok(());
            }
        };

        let sender_id = msg.from.as_ref().map(|u| u.id.0.to_string());
        if let Some(response) = slash_commands::handle_slash_command(
            &state,
            resolved_chat_id,
            "telegram",
            &text,
            sender_id.as_deref(),
        )
        .await
        {
            send_telegram_response(&bot, msg.chat.id, &response).await;
        } else {
            send_telegram_response(
                &bot,
                msg.chat.id,
                &slash_commands::unknown_command_response(),
            )
            .await;
        }
        return Ok(());
    }
    // --- インターセプトここまで ---

    // グループ/スーパーグループではメンション必須 (通常メッセージ向け)
    if !is_mentioned_in_group {
        debug!(
            chat_id = raw_chat_id,
            "Telegram: skipping non-mentioned group message"
        );
        return Ok(());
    }

    let context = SurfaceContext {
        channel: "telegram".to_string(),
        surface_user: sender_name,
        surface_thread: external_chat_id.clone(),
        chat_type: chat_type.clone(),
    };

    info!(
        chat_id = raw_chat_id,
        sender = %context.surface_user,
        text_length = text.len(),
        "Telegram message received"
    );

    // タイピングインジケーター (バックグラウンドタスクで定期的に送信)
    let typing_bot = bot.clone();
    let typing_chat_id = msg.chat.id;
    let typing_handle = tokio::spawn(async move {
        loop {
            let _ = typing_bot
                .send_chat_action(typing_chat_id, ChatAction::Typing)
                .await;
            tokio::time::sleep(Duration::from_secs(TYPING_INTERVAL_SECS)).await;
        }
    });

    match crate::agent_loop::process_turn(&state, &context, &text).await {
        Ok(response) => {
            typing_handle.abort();
            if !response.is_empty() {
                send_telegram_response(&bot, msg.chat.id, &response).await;
            }
        }
        Err(e) => {
            typing_handle.abort();
            error!(
                chat_id = raw_chat_id,
                error = %e,
                error_debug = ?e,
                "Telegram: error processing message"
            );
            let _ = bot
                .send_message(msg.chat.id, "Sorry, an error occurred.")
                .await;
        }
    }

    Ok(())
}

/// Telegram にメッセージを送信 (4096文字制限で自動分割)。
async fn send_telegram_response(bot: &Bot, chat_id: ChatId, text: &str) {
    for chunk in split_text(text, TELEGRAM_MAX_MESSAGE_LEN) {
        if let Err(e) = bot.send_message(chat_id, &chunk).await {
            warn!("Telegram: failed to send message chunk, retrying: {e}");
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Err(e) = bot.send_message(chat_id, &chunk).await {
                error!("Telegram: failed to send message chunk after retry: {e}");
            }
        }
    }
}

fn parse_telegram_chat_id(external_chat_id: &str) -> Result<i64, String> {
    external_chat_id
        .strip_prefix("telegram:")
        .unwrap_or(external_chat_id)
        .parse::<i64>()
        .map_err(|_| format!("invalid Telegram external_chat_id: '{external_chat_id}'"))
}

/// Telegram bot を起動。
///
/// Long polling モードでメッセージの受信を開始する。
/// microclaw `src/channels/telegram.rs::start_telegram_bot` と同じパターン。
pub async fn start_telegram_bot(
    state: Arc<AppState>,
    token: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bot = Bot::new(&token);

    // 既存の webhook を削除して polling モードを確保
    bot.delete_webhook().await.inspect_err(|e| {
        error!("Telegram: failed to delete webhook: {e}");
    })?;

    // BotFather にコマンド一覧を登録 (メニュー表示用)
    {
        use teloxide::types::BotCommand;

        let commands = vec![
            BotCommand::new("new", "Clear current session"),
            BotCommand::new("compact", "Force compact session"),
            BotCommand::new("status", "Show current status"),
            BotCommand::new("skills", "List available skills"),
            BotCommand::new("restart", "Restart the bot"),
            BotCommand::new("providers", "List LLM providers"),
            BotCommand::new("provider", "Show/switch provider"),
            BotCommand::new("models", "List models"),
            BotCommand::new("model", "Show/switch model"),
        ];
        if let Err(e) = bot.set_my_commands(commands).await {
            warn!("Telegram: failed to set bot commands: {e}");
        }
    }

    info!("Starting Telegram bot...");

    let handler = Update::filter_message().endpoint(handle_message);

    let listener = teloxide::update_listeners::polling_default(bot.clone()).await;
    let listener_error_handler = teloxide::error_handlers::LoggingErrorHandler::with_custom_text(
        "An error from the Telegram update listener".to_string(),
    );

    let mut dispatcher = Dispatcher::builder(bot, handler)
        .default_handler(|_| async {})
        .dependencies(dptree::deps![state])
        .build();
    let shutdown_token = dispatcher.shutdown_token();
    let shutdown_task = tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("Telegram bot failed to listen for Ctrl-C: {e}");
            return;
        }
        if let Ok(wait_for_shutdown) = shutdown_token.shutdown() {
            wait_for_shutdown.await;
        }
    });

    dispatcher
        .try_dispatch_with_listener(listener, listener_error_handler)
        .await
        .inspect_err(|e| {
            error!("Telegram dispatcher exited with error: {e}");
        })?;

    shutdown_task.abort();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_name() {
        let bot = Bot::new("test-token");
        let adapter = TelegramAdapter::new(bot);
        assert_eq!(adapter.name(), "telegram");
    }

    #[test]
    fn adapter_chat_type_routes() {
        let bot = Bot::new("test-token");
        let adapter = TelegramAdapter::new(bot);
        let routes = adapter.chat_type_routes();
        assert!(routes.len() >= 6);
        assert!(
            routes
                .iter()
                .any(|(k, v)| { *k == "telegram_private" && *v == ConversationKind::Private })
        );
    }

    #[test]
    fn parse_telegram_chat_id_accepts_raw_and_prefixed_values() {
        assert_eq!(parse_telegram_chat_id("12345").expect("raw chat id"), 12345);
        assert_eq!(
            parse_telegram_chat_id("telegram:12345").expect("prefixed chat id"),
            12345
        );
        assert!(parse_telegram_chat_id("telegram:not-a-number").is_err());
    }
}

//! Telegram チャネルアダプター。
//!
//! teloxide 0.17 を用いて Telegram Bot API (long polling) からメッセージを受信し、
//! EgoPulse agent runtime で処理した結果を Telegram に返信する。
//!
//! Based on: microclaw `src/channels/telegram.rs`

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, MessageEntityKind};
use tracing::{debug, error, info};

use crate::agent_loop::SurfaceContext;
use crate::channel::ConversationKind;
use crate::channel_adapter::ChannelAdapter;
use crate::runtime::AppState;
use crate::text::split_text;

/// Telegram メッセージ長制限 (文字数)。
const TELEGRAM_MAX_MESSAGE_LEN: usize = 4096;

/// タイピングインジケーターの送信間隔。
const TYPING_INTERVAL_SECS: u64 = 4;

/// Telegram チャネルアダプター。
///
/// アウトバウンドメッセージ送信用。Bot API 経由で Telegram にメッセージを送信する。
pub struct TelegramAdapter {
    bot: Bot,
}

impl TelegramAdapter {
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
        let chat_id = external_chat_id
            .parse::<i64>()
            .map_err(|_| format!("invalid Telegram external_chat_id: '{external_chat_id}'"))?;

        for chunk in split_text(text, TELEGRAM_MAX_MESSAGE_LEN) {
            self.bot
                .send_message(ChatId(chat_id), &chunk)
                .await
                .map_err(|e| format!("Telegram send_message failed: {e}"))?;
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

    // グループ/スーパーグループでは message entity の Mention で正確に判定
    let is_group = chat_type != "telegram_private";
    if is_group {
        let bot_username = state.config.telegram_bot_username();
        let mentioned = match &bot_username {
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
            None => false,
        };

        if !mentioned {
            debug!(
                chat_id = raw_chat_id,
                "Telegram: skipping non-mentioned group message"
            );
            return Ok(());
        }
    }

    let external_chat_id = raw_chat_id.to_string();

    let context = SurfaceContext {
        channel: "telegram".to_string(),
        surface_user: sender_name,
        surface_thread: external_chat_id.clone(),
        chat_type,
    };

    info!(
        chat_id = raw_chat_id,
        sender = %context.surface_user,
        text_preview = %text.chars().take(100).collect::<String>(),
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

    // session 解決は process_turn() に一任 (二重解決を避ける)
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
                "Telegram: error processing message: {e}"
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
            error!("Telegram: failed to send message chunk: {e}");
            break;
        }
    }
}

/// Telegram bot を起動。
///
/// Long polling モードでメッセージの受信を開始する。
/// microclaw `src/channels/telegram.rs::start_telegram_bot` と同じパターン。
pub async fn start_telegram_bot(state: Arc<AppState>, token: String) {
    let bot = Bot::new(&token);

    // 既存の webhook を削除して polling モードを確保
    let _ = bot.delete_webhook().await;

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

    dispatcher
        .try_dispatch_with_listener(listener, listener_error_handler)
        .await
        .ok();
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
        // microclaw パターン: 全 concrete chat_type を登録
        assert!(routes.len() >= 6);
        assert_eq!(routes[0].0, "telegram_private");
        assert_eq!(routes[0].1, ConversationKind::Private);
    }
}

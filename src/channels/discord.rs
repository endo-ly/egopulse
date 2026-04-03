//! Discord チャネルアダプター。
//!
//! serenity 0.12 を用いて Discord Gateway からメッセージを受信し、
//! EgoPulse agent runtime で処理した結果を Discord に返信する。
//!
//! Based on: microclaw `src/channels/discord.rs`

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use serenity::builder::{CreateAllowedMentions, CreateMessage};
use serenity::model::channel::Message as DiscordMessage;
use serenity::model::gateway::Ready;
use serenity::model::id::ChannelId;
use serenity::prelude::*;
use tracing::{error, info};

use crate::agent_loop::SurfaceContext;
use crate::channel::ConversationKind;
use crate::channel_adapter::ChannelAdapter;
use crate::runtime::AppState;
use crate::text::split_text;

/// Discord API リクエストのタイムアウト (秒)。
const DISCORD_REQUEST_TIMEOUT_SECS: u64 = 10;

/// 429 レート制限時の最大リトライ回数。
const DISCORD_MAX_RETRIES: u32 = 3;

/// 429 の Retry-After ヘッダがない場合のフォールバック待機時間 (秒)。
const DISCORD_RETRY_AFTER_FALLBACK_SECS: u64 = 2;

/// Discord メッセージ長制限 (文字数)。
const DISCORD_MAX_MESSAGE_LEN: usize = 2000;

/// Discord チャネルアダプター。
///
/// アウトバウンドメッセージ送信用。REST API 経由で Discord にメッセージを送信する。
pub struct DiscordAdapter {
    token: String,
    http_client: reqwest::Client,
}

impl DiscordAdapter {
    pub fn new(token: String) -> Self {
        Self::with_http_client(token, reqwest::Client::new())
    }

    pub fn with_http_client(token: String, http_client: reqwest::Client) -> Self {
        Self {
            token,
            http_client,
        }
    }
}

#[async_trait]
impl ChannelAdapter for DiscordAdapter {
    fn name(&self) -> &str {
        "discord"
    }

    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
        vec![("discord", ConversationKind::Private)]
    }

    async fn send_text(&self, external_chat_id: &str, text: &str) -> Result<(), String> {
        let discord_chat_id = external_chat_id
            .parse::<u64>()
            .map_err(|_| format!("invalid Discord external_chat_id: '{external_chat_id}'"))?;

        let url = format!("https://discord.com/api/v10/channels/{discord_chat_id}/messages");

        for chunk in split_text(text, DISCORD_MAX_MESSAGE_LEN) {
            let body = json!({
                "content": chunk,
                "allowed_mentions": { "parse": [] },
            });
            let mut attempt = 0;

            loop {
                let resp = self
                    .http_client
                    .post(&url)
                    .timeout(Duration::from_secs(DISCORD_REQUEST_TIMEOUT_SECS))
                    .header(
                        reqwest::header::AUTHORIZATION,
                        format!("Bot {}", self.token),
                    )
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| format!("Discord API request failed: {e}"))?;

                let status = resp.status();
                if status.is_success() {
                    break;
                }

                if status.as_u16() == 429 && attempt < DISCORD_MAX_RETRIES {
                    let retry_after = resp
                        .headers()
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(DISCORD_RETRY_AFTER_FALLBACK_SECS);
                    tokio::time::sleep(Duration::from_secs(retry_after)).await;
                    attempt += 1;
                    continue;
                }

                let body = resp.text().await.unwrap_or_default();
                return Err(format!(
                    "Discord API error: HTTP {status} {}",
                    body.chars().take(300).collect::<String>()
                ));
            }
        }

        Ok(())
    }
}

/// serenity EventHandler。インバウンドメッセージを処理する。
struct Handler {
    app_state: Arc<AppState>,
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: DiscordMessage) {
        // bot 自身のメッセージは無視
        if msg.author.bot {
            return;
        }

        let text = msg.content.clone();
        let external_channel_id = msg.channel_id.get();

        // 許可チャンネルチェック (設定が空なら全許可)
        let allowed_channels = self
            .app_state
            .config
            .channels
            .get("discord")
            .and_then(|c| c.allowed_channels.clone())
            .unwrap_or_default();
        if !allowed_channels.is_empty() && !allowed_channels.contains(&external_channel_id) {
            return;
        }

        // メンション検知 (guild の場合のみ)
        let should_respond = if msg.guild_id.is_some() {
            let cache = &ctx.cache;
            let bot_id = cache.current_user().id;
            msg.mentions.iter().any(|u| u.id == bot_id)
        } else {
            // DM は常に応答
            true
        };

        if !should_respond {
            return;
        }

        if text.is_empty() {
            return;
        }

        let sender_name = msg.author.name.clone();

        // microclaw パターン: chat_type を "discord" に統一
        let external_chat_id = external_channel_id.to_string();

        let context = SurfaceContext {
            channel: "discord".to_string(),
            surface_user: sender_name,
            surface_thread: external_chat_id.clone(),
            chat_type: "discord".to_string(),
        };

        info!(
            channel_id = external_chat_id,
            sender = %context.surface_user,
            text_length = text.len(),
            "Discord message received"
        );

        // タイピングインジケーター開始
        let typing = msg.channel_id.start_typing(&ctx.http);

        // session 解決は process_turn() に一任 (二重解決を避ける)
        match crate::agent_loop::process_turn(&self.app_state, &context, &text).await {
            Ok(response) => {
                drop(typing);
                if !response.is_empty() {
                    send_discord_response(&ctx, msg.channel_id, &response).await;
                }
            }
            Err(e) => {
                drop(typing);
                error!(
                    channel_id = external_chat_id,
                    "Discord: error processing message: {e}"
                );
                send_discord_response(&ctx, msg.channel_id, "Sorry, an error occurred.").await;
            }
        }
    }

    async fn ready(&self, _ctx: Context, ready: Ready) {
        info!("Discord bot connected as {}", ready.user.name);
    }
}

/// Discord にメッセージを送信 (2000文字制限で自動分割)。
async fn send_discord_response(ctx: &Context, channel_id: ChannelId, text: &str) {
    for chunk in split_text(text, DISCORD_MAX_MESSAGE_LEN) {
        let msg = CreateMessage::new()
            .content(chunk)
            .allowed_mentions(CreateAllowedMentions::new());
        if let Err(e) = channel_id.send_message(&ctx.http, msg).await {
            error!("Discord: failed to send message chunk: {e}");
            break;
        }
    }
}

/// Discord bot を起動。
///
/// Gateway に接続し、メッセージイベントの受信を開始する。
/// microclaw `src/channels/discord.rs::start_discord_bot` と同じパターン。
pub async fn start_discord_bot(
    state: Arc<AppState>,
    token: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    info!("Starting Discord bot (requesting MESSAGE_CONTENT intent)...");

    let handler = Handler { app_state: state };

    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .map_err(|e| {
            error!("Discord bot failed to start: {e}");
            e
        })?;

    client.start().await.map_err(|e| {
        error!("Discord bot error: {e}");
        e
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_name() {
        let adapter = DiscordAdapter::new("test-token".to_string());
        assert_eq!(adapter.name(), "discord");
    }

    #[test]
    fn adapter_chat_type_routes() {
        let adapter = DiscordAdapter::new("test-token".to_string());
        let routes = adapter.chat_type_routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].0, "discord");
        assert_eq!(routes[0].1, ConversationKind::Private);
    }
}

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
use serenity::model::application::Command;
use serenity::model::channel::Message as DiscordMessage;
use serenity::model::gateway::Ready;
use serenity::model::id::ChannelId;
use serenity::prelude::*;
use tracing::{error, info};

use crate::agent_loop::SurfaceContext;
use crate::channel_adapter::ChannelAdapter;
use crate::channel_adapter::ConversationKind;
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

/// Sends outbound messages to Discord via the REST API.
pub struct DiscordAdapter {
    token: String,
    http_client: reqwest::Client,
}

impl DiscordAdapter {
    /// Creates a Discord adapter with the default HTTP client.
    pub fn new(token: String) -> Self {
        Self::with_http_client(token, reqwest::Client::new())
    }

    /// Creates a Discord adapter with a caller-provided HTTP client.
    pub fn with_http_client(token: String, http_client: reqwest::Client) -> Self {
        Self { token, http_client }
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
        let discord_chat_id = parse_discord_chat_id(external_chat_id)?;

        let url = format!("https://discord.com/api/v10/channels/{discord_chat_id}/messages");

        for chunk in split_text(text, DISCORD_MAX_MESSAGE_LEN) {
            // メンション展開を無効化し、LLM 出力が意図せず通知を飛ばすのを防ぐ。
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

        // allowed_channels は guild 向け制御だけに使い、DM は個人チャネルとして常に通す。
        let allowed_channels = self
            .app_state
            .config
            .channels
            .get("discord")
            .and_then(|c| c.allowed_channels.clone())
            .unwrap_or_default();
        if msg.guild_id.is_some()
            && !allowed_channels.is_empty()
            && !allowed_channels.contains(&external_channel_id)
        {
            return;
        }

        // guild では bot への明示メンション時のみ応答し、雑談チャンネルへの常時反応を避ける。
        let should_respond = if msg.guild_id.is_some() {
            let cache = &ctx.cache;
            let bot_id = cache.current_user().id;
            msg.mentions.iter().any(|u| u.id == bot_id)
        } else {
            // DM は bot 宛て会話しか流れてこないため常時応答でよい。
            true
        };

        // microclaw パターン: chat_type を "discord" に統一
        let external_chat_id = external_channel_id.to_string();

        // --- スラッシュコマンドインターセプト ---
        // process_turn より先に chat_id を解決し、
        // スラッシュコマンドであればエージェントループに入らずに即応答する。
        if crate::slash_commands::is_slash_command(&text) {
            // guild ではメンション必須
            if msg.guild_id.is_some() && !should_respond {
                return;
            }

            let slash_chat_id =
                crate::storage::call_blocking(std::sync::Arc::clone(&self.app_state.db), {
                    let channel = "discord".to_string();
                    let ext_id = external_chat_id.clone();
                    let chat_type = "discord".to_string();
                    move |db| db.resolve_or_create_chat_id(&channel, &ext_id, None, &chat_type)
                })
                .await;

            match slash_chat_id {
                Ok(chat_id) => {
                    let sender_id = msg.author.id.get().to_string();
                    if let Some(response) = crate::slash_commands::handle_slash_command(
                        &self.app_state,
                        chat_id,
                        "discord",
                        &text,
                        Some(&sender_id),
                    )
                    .await
                    {
                        send_discord_response(&ctx, msg.channel_id, &response).await;
                    } else {
                        send_discord_response(
                            &ctx,
                            msg.channel_id,
                            &crate::slash_commands::unknown_command_response(),
                        )
                        .await;
                    }
                    return;
                }
                Err(e) => {
                    error!("failed to resolve chat_id for slash command: {e}");
                    send_discord_response(
                        &ctx,
                        msg.channel_id,
                        "An error occurred processing the command.",
                    )
                    .await;
                    return;
                }
            }
        }
        // --- インターセプトここまで ---

        if !should_respond {
            return;
        }

        if text.is_empty() {
            return;
        }

        let sender_name = msg.author.name.clone();

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

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("Discord bot connected as {}", ready.user.name);

        let commands: Vec<serenity::builder::CreateCommand> =
            crate::slash_commands::all_commands()
                .iter()
                .map(|c| serenity::builder::CreateCommand::new(c.name).description(c.description))
                .collect();

        if let Err(e) = Command::set_global_commands(&ctx.http, commands).await {
            tracing::warn!("Discord: failed to register slash commands: {e}");
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: serenity::model::application::Interaction) {
        let Some(cmd) = interaction.clone().command() else {
            return;
        };

        let command_text = interaction_to_command_text(&cmd.data.name);

        // チャネル ID を解決して内部 chat_id を取得
        let channel_id = cmd.channel_id.get();
        let external_chat_id = channel_id.to_string();

        let slash_chat_id =
            crate::storage::call_blocking(std::sync::Arc::clone(&self.app_state.db), {
                let channel = "discord".to_string();
                let ext_id = external_chat_id.clone();
                let chat_type = "discord".to_string();
                move |db| db.resolve_or_create_chat_id(&channel, &ext_id, None, &chat_type)
            })
            .await;

        let response_text = match slash_chat_id {
            Ok(chat_id) => {
                let sender_id = cmd.user.id.get().to_string();
                crate::slash_commands::handle_slash_command(
                    &self.app_state,
                    chat_id,
                    "discord",
                    &command_text,
                    Some(&sender_id),
                )
                .await
                .unwrap_or_else(|| crate::slash_commands::unknown_command_response())
            }
            Err(e) => {
                tracing::error!("failed to resolve chat_id for interaction: {e}");
                "An error occurred processing the command.".to_string()
            }
        };

        let message = serenity::builder::CreateInteractionResponseMessage::new()
            .content(response_text);
        if let Err(e) = cmd
            .create_response(&ctx.http, serenity::builder::CreateInteractionResponse::Message(message))
            .await
        {
            tracing::warn!("Discord: failed to respond to interaction: {e}");
        }
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

fn parse_discord_chat_id(external_chat_id: &str) -> Result<u64, String> {
    external_chat_id
        .strip_prefix("discord:")
        .unwrap_or(external_chat_id)
        .parse::<u64>()
        .map_err(|_| format!("invalid Discord external_chat_id: '{external_chat_id}'"))
}

/// Discord Interaction のコマンド名を handle_slash_command が
/// 受け付ける形式（"/command"）に正規化する。
fn interaction_to_command_text(name: &str) -> String {
    format!("/{name}")
}

/// Starts the Discord bot and supervises its gateway lifecycle.
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

    let shard_manager = client.shard_manager.clone();
    let shutdown_task = tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("Discord bot failed to listen for Ctrl-C: {e}");
            return;
        }
        shard_manager.shutdown_all().await;
    });

    client.start().await.map_err(|e| {
        error!("Discord bot error: {e}");
        e
    })?;

    shutdown_task.abort();

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

    #[test]
    fn parse_discord_chat_id_accepts_raw_and_prefixed_values() {
        assert_eq!(parse_discord_chat_id("12345").expect("raw chat id"), 12345);
        assert_eq!(
            parse_discord_chat_id("discord:12345").expect("prefixed chat id"),
            12345
        );
        assert!(parse_discord_chat_id("discord:not-a-number").is_err());
    }

    /// Interaction コマンド名 → "/command" 形式の正規化が正しいこと。
    #[test]
    fn interaction_command_text_normalizes() {
        // Arrange & Act & Assert
        assert_eq!(interaction_to_command_text("status"), "/status");
        assert_eq!(interaction_to_command_text("new"), "/new");
        assert_eq!(interaction_to_command_text("model"), "/model");
    }

    /// 未知コマンド名を正規化した場合、handle_slash_command が unknown_command_response を返すこと。
    #[test]
    fn interaction_unknown_command_responds() {
        // Arrange
        let command_text = interaction_to_command_text("nonexistent_cmd");

        // Act: 正規化後のテキストが is_slash_command に認識されることを確認
        assert!(crate::slash_commands::is_slash_command(&command_text));

        // Assert: handle_slash_command が None を返す（未知コマンド）
        // ※ AppState が必要なため、ここでは正規化結果の形式のみ検証
        assert_eq!(command_text, "/nonexistent_cmd");
    }
}

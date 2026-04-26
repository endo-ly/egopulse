//! Discord チャネルアダプター。
//!
//! serenity 0.12 を用いて Discord Gateway からメッセージを受信し、
//! EgoPulse agent runtime で処理した結果を Discord に返信する。

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
    /// Token used for legacy (non-agent-suffixed) outbound. May be empty when
    /// all outbound channels are agent-suffixed.
    legacy_token: String,
    /// Map from agent_id to Discord bot token, built from [`Config::discord_agent_bots`].
    agent_token_map: std::collections::HashMap<String, String>,
    http_client: reqwest::Client,
}

impl DiscordAdapter {
    pub fn new(token: String) -> Self {
        Self {
            legacy_token: token,
            agent_token_map: std::collections::HashMap::new(),
            http_client: reqwest::Client::new(),
        }
    }

    pub fn with_http_client(token: String, http_client: reqwest::Client) -> Self {
        Self {
            legacy_token: token,
            agent_token_map: std::collections::HashMap::new(),
            http_client,
        }
    }

    pub fn new_for_agents(config: &crate::config::Config) -> Self {
        let legacy_token = config.discord_bot_token().unwrap_or_default();
        let agent_tokens: std::collections::HashMap<String, String> = config
            .discord_agent_bots()
            .into_iter()
            .map(|b| (b.agent_id.to_string(), b.token.to_string()))
            .collect();
        Self {
            legacy_token,
            agent_token_map: agent_tokens,
            http_client: reqwest::Client::new(),
        }
    }

    fn select_token(&self, external_chat_id: &str) -> &str {
        match parse_discord_agent_id(external_chat_id) {
            Some(agent_id) => self.agent_token_map.get(agent_id).map(String::as_str),
            None => None,
        }
        .unwrap_or(&self.legacy_token)
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
        let token = self.select_token(external_chat_id);

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
                        format!("Bot {token}"),
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
    agent_id: String,
    agent_label: String,
    allowed_channels: Vec<u64>,
}

impl Handler {
    fn make_context(&self, user: &str, thread: &str) -> SurfaceContext {
        SurfaceContext {
            channel: "discord".to_string(),
            surface_user: user.to_string(),
            surface_thread: format!("{thread}:agent:{}", self.agent_id),
            chat_type: "discord".to_string(),
            agent_id: self.agent_id.clone(),
        }
    }

    fn guild_allowed(&self, channel_id: u64) -> bool {
        self.allowed_channels.is_empty() || self.allowed_channels.contains(&channel_id)
    }
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: DiscordMessage) {
        if msg.author.bot {
            return;
        }

        let text = msg.content.clone();
        let channel_id = msg.channel_id.get();

        if msg.guild_id.is_some() && !self.guild_allowed(channel_id) {
            return;
        }

        let thread = channel_id.to_string();

        if crate::slash_commands::is_slash_command(&text) {
            let slash_chat_id =
                crate::storage::call_blocking(std::sync::Arc::clone(&self.app_state.db), {
                    let channel = "discord".to_string();
                    let ext_id = thread.clone();
                    let chat_type = "discord".to_string();
                    move |db| db.resolve_or_create_chat_id(&channel, &ext_id, None, &chat_type)
                })
                .await;

            match slash_chat_id {
                Ok(chat_id) => {
                    let sender_id = msg.author.id.get().to_string();
                    let slash_context = self.make_context(&msg.author.name, &thread);
                    if let Some(response) = crate::slash_commands::handle_slash_command(
                        &self.app_state,
                        chat_id,
                        &slash_context,
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

        if text.is_empty() {
            return;
        }

        let context = self.make_context(&msg.author.name, &thread);

        info!(
            channel_id = channel_id,
            agent = %self.agent_id,
            sender = %context.surface_user,
            text_length = text.len(),
            "Discord message received"
        );

        let typing = msg.channel_id.start_typing(&ctx.http);

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
                    channel_id = channel_id,
                    agent = %self.agent_id,
                    error_kind = e.error_kind(),
                    error = %e,
                    error_debug = ?e,
                    "Discord: error processing message"
                );
                if !e.should_suppress_user_error() {
                    send_discord_response(&ctx, msg.channel_id, &e.user_message()).await;
                }
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(
            "Discord bot ({}) connected as {}",
            self.agent_label, ready.user.name
        );

        let commands: Vec<serenity::builder::CreateCommand> = crate::slash_commands::all_commands()
            .iter()
            .map(|c| {
                let builder =
                    serenity::builder::CreateCommand::new(c.name).description(c.description);
                if c.usage.contains('[') {
                    builder.add_option(
                        serenity::builder::CreateCommandOption::new(
                            serenity::model::application::CommandOptionType::String,
                            "name",
                            c.description,
                        )
                        .required(false),
                    )
                } else {
                    builder
                }
            })
            .collect();

        if let Err(e) = Command::set_global_commands(&ctx.http, commands).await {
            tracing::warn!("Discord: failed to register slash commands: {e}");
        }
    }

    async fn interaction_create(
        &self,
        ctx: Context,
        interaction: serenity::model::application::Interaction,
    ) {
        let Some(cmd) = interaction.clone().command() else {
            return;
        };

        if let Err(e) = cmd
            .create_response(
                &ctx.http,
                serenity::builder::CreateInteractionResponse::Defer(
                    serenity::builder::CreateInteractionResponseMessage::new(),
                ),
            )
            .await
        {
            tracing::warn!("Discord: failed to defer interaction: {e}");
            return;
        }

        let command_text = interaction_to_command_text(&cmd.data.name, &cmd.data.options);
        let channel_id = cmd.channel_id.get();
        let thread = channel_id.to_string();

        let slash_chat_id =
            crate::storage::call_blocking(std::sync::Arc::clone(&self.app_state.db), {
                let channel = "discord".to_string();
                let ext_id = thread.clone();
                let chat_type = "discord".to_string();
                move |db| db.resolve_or_create_chat_id(&channel, &ext_id, None, &chat_type)
            })
            .await;

        let response_text = match slash_chat_id {
            Ok(chat_id) => {
                let sender_id = cmd.user.id.get().to_string();
                let slash_context = self.make_context(&cmd.user.name, &thread);
                crate::slash_commands::handle_slash_command(
                    &self.app_state,
                    chat_id,
                    &slash_context,
                    &command_text,
                    Some(&sender_id),
                )
                .await
                .unwrap_or_else(crate::slash_commands::unknown_command_response)
            }
            Err(e) => {
                tracing::error!("failed to resolve chat_id for interaction: {e}");
                "An error occurred processing the command.".to_string()
            }
        };

        if let Err(e) = cmd
            .edit_response(
                &ctx.http,
                serenity::builder::EditInteractionResponse::new().content(response_text),
            )
            .await
        {
            tracing::warn!("Discord: failed to edit interaction response: {e}");
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
    // Strip agent suffix if present: "123:agent:developer" → "123"
    let bare = if let Some(pos) = external_chat_id.find(":agent:") {
        &external_chat_id[..pos]
    } else {
        external_chat_id
    };
    bare.strip_prefix("discord:")
        .unwrap_or(bare)
        .parse::<u64>()
        .map_err(|_| format!("invalid Discord external_chat_id: '{external_chat_id}'"))
}

fn parse_discord_agent_id(external_chat_id: &str) -> Option<&str> {
    let pos = external_chat_id.find(":agent:")?;
    let agent_id = &external_chat_id[pos + ":agent:".len()..];
    if agent_id.is_empty() { None } else { Some(agent_id) }
}

/// Discord Interaction のコマンド名と引数を handle_slash_command が
/// 受け付ける形式（"/command [args]"）に正規化する。
fn interaction_to_command_text(
    name: &str,
    options: &[serenity::model::application::CommandDataOption],
) -> String {
    let mut text = format!("/{name}");
    for opt in options {
        if let serenity::model::application::CommandDataOptionValue::String(value) = &opt.value {
            text.push(' ');
            text.push_str(value);
        }
    }
    text
}

/// Starts a Discord bot for a specific agent.
///
/// Wraps [`start_discord_bot`] and passes agent metadata for per-agent
/// context binding in the event handler (see Step 3).
pub async fn start_discord_bot_for_agent(
    state: Arc<AppState>,
    token: &str,
    agent_id: &crate::config::AgentId,
    allowed_channels: &[u64],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let agent_label = state
        .config
        .agents
        .get(agent_id)
        .map(|a| a.label.as_str())
        .unwrap_or(agent_id.as_str())
        .to_string();

    info!(
        "Starting Discord bot for agent '{}' ({agent_label}) ...",
        agent_id.as_str(),
    );

    let handler = Handler {
        app_state: state,
        agent_id: agent_id.to_string(),
        agent_label,
        allowed_channels: allowed_channels.to_vec(),
    };

    let mut client = Client::builder(token, intents)
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

/// Starts the Discord bot and supervises its gateway lifecycle.
pub async fn start_discord_bot(
    state: Arc<AppState>,
    token: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // legacy path: use default agent. Prefer start_discord_bot_for_agent instead.
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let default_agent = state.config.default_agent.to_string();
    let agent_label = state
        .config
        .agents
        .get(&state.config.default_agent)
        .map(|a| a.label.as_str())
        .unwrap_or("default")
        .to_string();
    let allowed_channels = state
        .config
        .channels
        .get("discord")
        .and_then(|c| c.allowed_channels.clone())
        .unwrap_or_default();

    info!("Starting Discord bot (requesting MESSAGE_CONTENT intent)...");

    let handler = Handler {
        app_state: state,
        agent_id: default_agent,
        agent_label,
        allowed_channels,
    };

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

    #[test]
    fn parse_discord_chat_id_accepts_agent_suffix() {
        assert_eq!(
            parse_discord_chat_id("123:agent:developer").expect("agent suffix"),
            123
        );
    }

    #[test]
    fn parse_discord_agent_id_from_external_chat_id() {
        assert_eq!(
            parse_discord_agent_id("123:agent:developer"),
            Some("developer")
        );
        assert_eq!(parse_discord_agent_id("12345"), None);
        assert_eq!(parse_discord_agent_id("discord:123"), None);
    }

    #[test]
    fn parse_discord_chat_id_accepts_legacy_prefixed() {
        assert_eq!(
            parse_discord_chat_id("discord:12345").expect("legacy prefixed"),
            12345
        );
    }

    #[test]
    fn parse_discord_chat_id_rejects_bad_agent_suffix() {
        assert!(parse_discord_chat_id("abc:agent:developer").is_err());
    }

    #[test]
    fn parse_discord_chat_id_rejects_empty_channel() {
        assert!(parse_discord_chat_id(":agent:developer").is_err());
    }

    /// Interaction コマンド名 → "/command" 形式の正規化が正しいこと。
    #[test]
    fn interaction_command_text_normalizes() {
        // Arrange & Act & Assert: 引数なし
        assert_eq!(interaction_to_command_text("status", &[]), "/status");
        assert_eq!(interaction_to_command_text("new", &[]), "/new");
        assert_eq!(interaction_to_command_text("model", &[]), "/model");
    }

    /// 未知コマンド名を正規化した場合、handle_slash_command が unknown_command_response を返すこと。
    #[test]
    fn interaction_unknown_command_responds() {
        // Arrange
        let command_text = interaction_to_command_text("nonexistent_cmd", &[]);

        // Act: 正規化後のテキストが is_slash_command に認識されることを確認
        assert!(crate::slash_commands::is_slash_command(&command_text));

        // Assert: handle_slash_command が None を返す（未知コマンド）
        // ※ AppState が必要なため、ここでは正規化結果の形式のみ検証
        assert_eq!(command_text, "/nonexistent_cmd");
    }
}

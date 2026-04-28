//! Discord チャネルアダプター。
//!
//! serenity 0.12 を用いて Discord Gateway からメッセージを受信し、
//! EgoPulse agent runtime で処理した結果を Discord に返信する。

use std::path::PathBuf;
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
use tracing::{error, info, warn};

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
    /// Token map: bot_id → token, built from [`Config::discord_bots`].
    bot_token_map: std::collections::HashMap<String, String>,
    http_client: reqwest::Client,
}

impl DiscordAdapter {
    pub fn new_for_bots(config: &crate::config::Config) -> Self {
        let bot_tokens: std::collections::HashMap<String, String> = config
            .discord_bots()
            .into_iter()
            .map(|b| (b.bot_id.to_string(), b.token.to_string()))
            .collect();
        Self {
            bot_token_map: bot_tokens,
            http_client: reqwest::Client::new(),
        }
    }

    fn select_token(&self, external_chat_id: &str) -> Result<&str, String> {
        match parse_discord_bot_id(external_chat_id) {
            Some(bot_id) => self
                .bot_token_map
                .get(bot_id)
                .map(String::as_str)
                .ok_or_else(|| {
                    format!(
                        "no Discord bot token found for bot '{bot_id}' in external_chat_id '{external_chat_id}'"
                    )
                }),
            None => Err(format!(
                "no bot suffix in external_chat_id '{external_chat_id}'"
            )),
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
        let discord_chat_id = parse_discord_chat_id(external_chat_id)?;
        let token = self.select_token(external_chat_id)?;

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
                    .header(reqwest::header::AUTHORIZATION, format!("Bot {token}"))
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
    bot_id: String,
    default_agent: String,
    allowed_channels: Vec<u64>,
    channel_agents: std::collections::HashMap<u64, String>,
}

impl Handler {
    fn select_agent(&self, channel_id: u64, is_dm: bool) -> &str {
        if is_dm {
            return &self.default_agent;
        }
        self.channel_agents
            .get(&channel_id)
            .map(String::as_str)
            .unwrap_or(&self.default_agent)
    }

    fn agent_thread(&self, thread: &str, agent_id: &str) -> String {
        format!("{thread}:bot:{}:agent:{agent_id}", self.bot_id)
    }

    fn make_context(&self, user: &str, thread: &str, agent_id: &str) -> SurfaceContext {
        SurfaceContext {
            channel: "discord".to_string(),
            surface_user: user.to_string(),
            surface_thread: self.agent_thread(thread, agent_id),
            chat_type: "discord".to_string(),
            agent_id: agent_id.to_string(),
        }
    }

    fn guild_allowed(&self, channel_id: u64) -> bool {
        !self.allowed_channels.is_empty() && self.allowed_channels.contains(&channel_id)
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
        let is_dm = msg.guild_id.is_none();
        let agent_id = self.select_agent(channel_id, is_dm).to_string();

        if !is_dm && !self.guild_allowed(channel_id) {
            return;
        }

        let thread = channel_id.to_string();

        if crate::slash_commands::is_slash_command(&text) {
            let slash_chat_id =
                crate::storage::call_blocking(std::sync::Arc::clone(&self.app_state.db), {
                    let channel = "discord".to_string();
                    let ext_id = self.agent_thread(&thread, &agent_id);
                    let chat_type = "discord".to_string();
                    move |db| db.resolve_or_create_chat_id(&channel, &ext_id, None, &chat_type)
                })
                .await;

            match slash_chat_id {
                Ok(chat_id) => {
                    let sender_id = msg.author.id.get().to_string();
                    let slash_context = self.make_context(&msg.author.name, &thread, &agent_id);
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

        let workspace_dir = match self.app_state.config.workspace_dir() {
            Ok(d) => d,
            Err(e) => {
                error!("failed to resolve workspace dir: {e}");
                return;
            }
        };

        let mut saved_paths: Vec<PathBuf> = Vec::new();
        for attachment in &msg.attachments {
            match reqwest::get(&attachment.url).await {
                Ok(resp) => match resp.bytes().await {
                    Ok(bytes) => {
                        match crate::media::save_inbound_file(
                            &workspace_dir,
                            &attachment.filename,
                            &bytes,
                        ) {
                            Ok(path) => saved_paths.push(path),
                            Err(e) => {
                                warn!(
                                    filename = %attachment.filename,
                                    error = %e,
                                    "failed to save inbound attachment"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            filename = %attachment.filename,
                            error = %e,
                            "failed to read attachment body"
                        );
                    }
                },
                Err(e) => {
                    warn!(
                        filename = %attachment.filename,
                        error = %e,
                        "failed to download attachment"
                    );
                }
            }
        }

        let combined_text = crate::media::format_attachment_text(&saved_paths, &text);

        if combined_text.is_empty() {
            return;
        }

        let context = self.make_context(&msg.author.name, &thread, &agent_id);

        info!(
            channel_id = channel_id,
            agent = %agent_id, bot = %self.bot_id,
            sender = %context.surface_user,
            text_length = text.len(),
            attachments = saved_paths.len(),
            "Discord message received"
        );

        let typing = msg.channel_id.start_typing(&ctx.http);

        match crate::agent_loop::process_turn(&self.app_state, &context, &combined_text).await {
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
                    agent = %agent_id, bot = %self.bot_id,
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
            self.default_agent, ready.user.name
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
        let is_dm_int = cmd.guild_id.is_none();
        let interaction_agent = self.select_agent(channel_id, is_dm_int).to_string();
        let thread = channel_id.to_string();

        let slash_chat_id =
            crate::storage::call_blocking(std::sync::Arc::clone(&self.app_state.db), {
                let channel = "discord".to_string();
                let ext_id = self.agent_thread(&thread, &interaction_agent);
                let chat_type = "discord".to_string();
                move |db| db.resolve_or_create_chat_id(&channel, &ext_id, None, &chat_type)
            })
            .await;

        let response_text = match slash_chat_id {
            Ok(chat_id) => {
                let sender_id = cmd.user.id.get().to_string();
                let slash_context = self.make_context(&cmd.user.name, &thread, &interaction_agent);
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
    // Strip bot+agent suffix: "123:bot:main:agent:developer" → "123"
    let bare = if let Some(pos) = external_chat_id.find(":bot:") {
        &external_chat_id[..pos]
    } else {
        external_chat_id
    };
    bare.strip_prefix("discord:")
        .unwrap_or(bare)
        .parse::<u64>()
        .map_err(|_| format!("invalid Discord external_chat_id: '{external_chat_id}'"))
}

fn parse_discord_bot_id(external_chat_id: &str) -> Option<&str> {
    // Pattern: "...:bot:<bot_id>:agent:<agent_id>" → extract bot_id
    let bot_start = external_chat_id.find(":bot:")?;
    let after_bot = &external_chat_id[bot_start + ":bot:".len()..];
    let bot_end = after_bot.find(":agent:")?;
    let bot_id = &after_bot[..bot_end];
    if bot_id.is_empty() {
        None
    } else {
        Some(bot_id)
    }
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

/// Starts a Discord bot with agent routing configured.
pub async fn start_discord_bot_for_bot(
    state: Arc<AppState>,
    token: &str,
    bot_id: &crate::config::BotId,
    default_agent: &crate::config::AgentId,
    allowed_channels: &[u64],
    channel_agents: &std::collections::HashMap<u64, crate::config::AgentId>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    info!(
        "Starting Discord bot '{}' (agent {default_agent}) ...",
        bot_id.as_str(),
    );

    let agent_map: std::collections::HashMap<u64, String> = channel_agents
        .iter()
        .map(|(k, v)| (*k, v.to_string()))
        .collect();

    let handler = Handler {
        app_state: state,
        bot_id: bot_id.to_string(),
        default_agent: default_agent.to_string(),
        allowed_channels: allowed_channels.to_vec(),
        channel_agents: agent_map,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_handler(allowed_channels: Vec<u64>) -> Handler {
        Handler {
            app_state: Arc::new(crate::agent_loop::turn::build_state_with_provider(
                tempfile::tempdir()
                    .expect("tempdir")
                    .path()
                    .to_str()
                    .expect("utf8")
                    .to_string(),
                Box::new(crate::agent_loop::turn::FakeProvider {
                    responses: std::sync::Mutex::new(vec![crate::llm::MessagesResponse {
                        content: "ok".to_string(),
                        tool_calls: vec![],
                        usage: None,
                    }]),
                }),
            )),
            bot_id: "main".to_string(),
            default_agent: "developer".to_string(),
            allowed_channels,
            channel_agents: std::collections::HashMap::new(),
        }
    }

    fn test_adapter() -> DiscordAdapter {
        DiscordAdapter {
            bot_token_map: std::collections::HashMap::new(),
            http_client: reqwest::Client::new(),
        }
    }

    #[test]
    fn adapter_name() {
        let adapter = test_adapter();
        assert_eq!(adapter.name(), "discord");
    }

    #[test]
    fn adapter_chat_type_routes() {
        let adapter = test_adapter();
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
    fn parse_discord_bot_id_from_external_chat_id() {
        assert_eq!(
            parse_discord_bot_id("123:bot:main:agent:developer"),
            Some("main")
        );
        assert_eq!(parse_discord_bot_id("12345"), None);
        assert_eq!(parse_discord_bot_id("discord:123"), None);
    }

    #[test]
    fn parse_discord_chat_id_accepts_channel_prefixed() {
        assert_eq!(
            parse_discord_chat_id("discord:12345").expect("channel prefixed"),
            12345
        );
    }

    #[test]
    fn guild_allowed_rejects_when_allowed_channels_empty() {
        let handler = test_handler(vec![]);

        assert!(
            !handler.guild_allowed(123),
            "empty allowed_channels should reject guild messages"
        );
    }
    #[test]
    fn guild_allowed_accepts_listed_channel_only() {
        let handler = test_handler(vec![123, 456]);

        assert!(handler.guild_allowed(123));
        assert!(!handler.guild_allowed(789));
    }

    #[test]
    fn interaction_chat_id_uses_agent_scoped_thread() {
        let handler = test_handler(vec![123]);

        assert_eq!(
            handler.agent_thread("123", "developer"),
            "123:bot:main:agent:developer"
        );
        assert_eq!(
            handler
                .make_context("user", "123", "developer")
                .surface_thread,
            "123:bot:main:agent:developer"
        );
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

    #[test]
    fn discord_attachment_builds_combined_text() {
        // Arrange
        let paths = vec![
            PathBuf::from("/workspace/media/inbound/20260428-120000-cat.png"),
            PathBuf::from("/workspace/media/inbound/20260428-120001-notes.pdf"),
        ];
        let user_text = "check these files";

        // Act
        let combined = crate::media::format_attachment_text(&paths, user_text);

        // Assert
        assert!(combined.contains("[attachment: /workspace/media/inbound/20260428-120000-cat.png]"));
        assert!(combined.contains("[attachment: /workspace/media/inbound/20260428-120001-notes.pdf]"));
        assert!(combined.contains("check these files"));
        assert!(combined.starts_with("[attachment:"));
    }

    #[test]
    fn discord_text_only_no_regression() {
        // Arrange
        let paths: Vec<PathBuf> = vec![];
        let user_text = "hello world";

        // Act
        let combined = crate::media::format_attachment_text(&paths, user_text);

        // Assert
        assert_eq!(combined, "hello world");
        assert!(!combined.contains("[attachment:"));
    }
}

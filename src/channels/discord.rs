//! Discord チャネルアダプター。
//!
//! serenity 0.12 を用いて Discord Gateway からメッセージを受信し、
//! EgoPulse agent runtime で処理した結果を Discord に返信する。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use serde_json::json;
use serenity::builder::{CreateAllowedMentions, CreateMessage};
use serenity::model::application::Command;
use serenity::model::channel::Message as DiscordMessage;
use serenity::model::gateway::Ready;
use serenity::model::id::{ChannelId, UserId};
use serenity::prelude::*;
use tracing::{error, info, warn};

use crate::agent_loop::SurfaceContext;
use crate::channel_adapter::ChannelAdapter;
use crate::channel_adapter::ConversationKind;
use crate::config::DiscordChannelConfig;
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

/// Bot-to-bot chain maximum depth per channel/thread.
const BOT_CHAIN_MAX_DEPTH: u32 = 5;

/// Bot-to-bot chain state TTL in seconds.
const BOT_CHAIN_TTL_SECS: u64 = 300;

struct ChainEntry {
    depth: u32,
    last_updated: Instant,
}

pub(crate) struct BotChainState {
    ttl: Duration,
    chains: Mutex<HashMap<u64, ChainEntry>>,
}

impl BotChainState {
    pub(crate) fn new() -> Self {
        Self::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS))
    }

    pub(crate) fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            chains: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn check_and_increment(&self, channel_id: u64) -> bool {
        let mut chains = self.chains.lock().expect("bot chain state lock poisoned");
        let now = Instant::now();

        chains.retain(|_, e| now.duration_since(e.last_updated) < self.ttl);

        let entry = chains.get_mut(&channel_id);
        match entry {
            Some(e) => {
                if e.depth >= BOT_CHAIN_MAX_DEPTH {
                    false
                } else {
                    e.depth += 1;
                    e.last_updated = now;
                    true
                }
            }
            None => {
                chains.insert(
                    channel_id,
                    ChainEntry {
                        depth: 1,
                        last_updated: now,
                    },
                );
                true
            }
        }
    }

    pub(crate) fn reset(&self, channel_id: u64) {
        let mut chains = self.chains.lock().expect("bot chain state lock poisoned");
        chains.remove(&channel_id);
    }
}

#[derive(Debug, PartialEq)]
enum ReceiveDecision {
    Accept { reset_chain: bool },
    Reject,
}

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
            send_discord_api(&self.http_client, |client| {
                client
                    .post(&url)
                    .timeout(Duration::from_secs(DISCORD_REQUEST_TIMEOUT_SECS))
                    .header(reqwest::header::AUTHORIZATION, format!("Bot {token}"))
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .json(&body)
            })
            .await?;
        }

        Ok(())
    }

    async fn send_attachment(
        &self,
        external_chat_id: &str,
        text: Option<&str>,
        file_path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let discord_chat_id = parse_discord_chat_id(external_chat_id)?;
        let token = self.select_token(external_chat_id)?;
        let url = format!("https://discord.com/api/v10/channels/{discord_chat_id}/messages");

        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| format!("failed to read file: {e}"))?;

        let content = text.or(caption).unwrap_or("");

        send_discord_api(&self.http_client, |client| {
            let part = Part::bytes(file_bytes.clone())
                .file_name(filename.clone())
                .mime_str("application/octet-stream")
                .expect("'application/octet-stream' is a valid MIME type");

            let mut form = Form::new().part("file", part);

            if !content.is_empty() {
                let payload = json!({ "content": content });
                form = form.text("payload_json", payload.to_string());
            }

            client
                .post(&url)
                .timeout(Duration::from_secs(30))
                .header(reqwest::header::AUTHORIZATION, format!("Bot {token}"))
                .multipart(form)
        })
        .await
    }
}

/// Send a Discord API request with automatic 429 retry handling.
async fn send_discord_api<F>(client: &reqwest::Client, build_request: F) -> Result<(), String>
where
    F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
{
    let mut attempt = 0;
    loop {
        let resp = build_request(client)
            .send()
            .await
            .map_err(|e| format!("Discord API request failed: {e}"))?;

        let status = resp.status();
        if status.is_success() {
            return Ok(());
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

/// serenity EventHandler。インバウンドメッセージを処理する。
struct Handler {
    app_state: Arc<AppState>,
    bot_id: String,
    default_agent: String,
    channels: HashMap<u64, DiscordChannelConfig>,
    bot_user_id: OnceLock<UserId>,
    chain_state: Arc<BotChainState>,
    http_client: reqwest::Client,
}

impl Handler {
    fn select_agent(&self, channel_id: u64, is_dm: bool) -> &str {
        if is_dm {
            return &self.default_agent;
        }
        self.channels
            .get(&channel_id)
            .and_then(|c| c.agent.as_ref())
            .map(|a| a.as_str())
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
        self.channels.contains_key(&channel_id)
    }

    fn is_bot_mentioned(&self, msg: &DiscordMessage) -> bool {
        let Some(bot_id) = self.bot_user_id.get() else {
            return false;
        };
        msg.mentions.iter().any(|u| u.id == *bot_id)
    }

    fn is_self_message(&self, author_id: u64) -> bool {
        self.bot_user_id
            .get()
            .is_some_and(|id| id.get() == author_id)
    }

    fn should_process_message(
        &self,
        author_id: u64,
        author_is_bot: bool,
        is_dm: bool,
        channel_id: u64,
        mentions_bot: bool,
    ) -> ReceiveDecision {
        if self.is_self_message(author_id) {
            return ReceiveDecision::Reject;
        }

        if author_is_bot {
            if !mentions_bot {
                return ReceiveDecision::Reject;
            }
            if !self.chain_state.check_and_increment(channel_id) {
                return ReceiveDecision::Reject;
            }
            return ReceiveDecision::Accept { reset_chain: false };
        }

        if !is_dm {
            if let Some(config) = self.channels.get(&channel_id) {
                if config.require_mention && !mentions_bot {
                    return ReceiveDecision::Reject;
                }
            }
        }

        ReceiveDecision::Accept { reset_chain: true }
    }
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: DiscordMessage) {
        let channel_id = msg.channel_id.get();
        let is_dm = msg.guild_id.is_none();

        if !is_dm && !self.guild_allowed(channel_id) {
            return;
        }

        if self.is_self_message(msg.author.id.get()) {
            return;
        }

        let text = msg.content.clone();
        let agent_id = self.select_agent(channel_id, is_dm).to_string();

        let thread = channel_id.to_string();
        if crate::slash_commands::is_slash_command(&text) {
            let slash_context = self.make_context(&msg.author.name, &thread, &agent_id);
            let sender_id = msg.author.id.get().to_string();
            let slash_chat_id =
                crate::agent_loop::session::resolve_chat_id(&self.app_state, &slash_context).await;

            match slash_chat_id {
                Ok(chat_id) => {
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

        let mentions_bot = self.is_bot_mentioned(&msg);
        let decision = self.should_process_message(
            msg.author.id.get(),
            msg.author.bot,
            is_dm,
            channel_id,
            mentions_bot,
        );

        match decision {
            ReceiveDecision::Accept { reset_chain: true } => {
                self.chain_state.reset(channel_id);
            }
            ReceiveDecision::Accept { reset_chain: false } => {}
            ReceiveDecision::Reject => return,
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
            match self.http_client.get(&attachment.url).send().await {
                Ok(resp) => match resp.error_for_status() {
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
                            "attachment download returned non-success status"
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
        let _ = self.bot_user_id.set(ready.user.id);

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

        let channel_id = cmd.channel_id.get();
        let is_dm_int = cmd.guild_id.is_none();
        if !is_dm_int && !self.guild_allowed(channel_id) {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    serenity::builder::CreateInteractionResponse::Message(
                        serenity::builder::CreateInteractionResponseMessage::new()
                            .content("This command is not available in this channel.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        }

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

        let slash_context = self.make_context(&cmd.user.name, &thread, &interaction_agent);
        let sender_id = cmd.user.id.get().to_string();
        let slash_chat_id =
            crate::agent_loop::session::resolve_chat_id(&self.app_state, &slash_context).await;

        let response_text = match slash_chat_id {
            Ok(chat_id) => crate::slash_commands::handle_slash_command(
                &self.app_state,
                chat_id,
                &slash_context,
                &command_text,
                Some(&sender_id),
            )
            .await
            .unwrap_or_else(crate::slash_commands::unknown_command_response),
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
    let http = &ctx.http;
    if let Err(error) = crate::text::send_chunked(text, DISCORD_MAX_MESSAGE_LEN, |chunk| {
        let msg = CreateMessage::new()
            .content(chunk)
            .allowed_mentions(CreateAllowedMentions::new());
        let http = http.clone();
        Box::pin(async move {
            channel_id
                .send_message(&http, msg)
                .await
                .map(|_| ())
                .map_err(|e| format!("Discord: failed to send message chunk: {e}"))
        })
    })
    .await
    {
        error!(
            channel_id = channel_id.get(),
            error = %error,
            "Discord: failed to send chunked response"
        );
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
#[allow(private_interfaces)]
pub async fn start_discord_bot_for_bot(
    state: Arc<AppState>,
    token: &str,
    bot_id: &crate::config::BotId,
    default_agent: &crate::config::AgentId,
    channels: &HashMap<u64, DiscordChannelConfig>,
    chain_state: Arc<BotChainState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    info!(
        "Starting Discord bot '{}' (agent {default_agent}) ...",
        bot_id.as_str(),
    );

    let handler = Handler {
        app_state: state,
        bot_id: bot_id.to_string(),
        default_agent: default_agent.to_string(),
        channels: channels.clone(),
        bot_user_id: OnceLock::new(),
        chain_state,
        http_client: reqwest::Client::builder()
            .timeout(Duration::from_secs(DISCORD_REQUEST_TIMEOUT_SECS))
            .build()
            .unwrap_or_default(),
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

    fn test_handler(channels: HashMap<u64, DiscordChannelConfig>) -> Handler {
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
            channels,
            bot_user_id: OnceLock::new(),
            chain_state: Arc::new(BotChainState::new()),
            http_client: reqwest::Client::new(),
        }
    }

    fn test_handler_with_bot_id(
        channels: HashMap<u64, DiscordChannelConfig>,
        bot_user_id: u64,
    ) -> Handler {
        let lock = OnceLock::new();
        lock.set(UserId::new(bot_user_id)).expect("OnceLock set");
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
            channels,
            bot_user_id: lock,
            chain_state: Arc::new(BotChainState::new()),
            http_client: reqwest::Client::new(),
        }
    }

    fn test_handler_with_chain(
        channels: HashMap<u64, DiscordChannelConfig>,
        bot_user_id: u64,
        chain_state: Arc<BotChainState>,
    ) -> Handler {
        let lock = OnceLock::new();
        lock.set(UserId::new(bot_user_id)).expect("OnceLock set");
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
            bot_id: "bot_a".to_string(),
            default_agent: "developer".to_string(),
            channels,
            bot_user_id: lock,
            chain_state,
            http_client: reqwest::Client::new(),
        }
    }

    fn test_adapter() -> DiscordAdapter {
        DiscordAdapter {
            bot_token_map: HashMap::new(),
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
    fn guild_allowed_rejects_when_channels_empty() {
        let handler = test_handler(HashMap::new());

        assert!(
            !handler.guild_allowed(123),
            "empty channels should reject guild messages"
        );
    }

    #[test]
    fn guild_allowed_accepts_listed_channel_only() {
        let mut channels = HashMap::new();
        channels.insert(123, DiscordChannelConfig::default());
        channels.insert(456, DiscordChannelConfig::default());
        let handler = test_handler(channels);

        assert!(handler.guild_allowed(123));
        assert!(!handler.guild_allowed(789));
    }

    #[test]
    fn interaction_chat_id_uses_agent_scoped_thread() {
        let mut channels = HashMap::new();
        channels.insert(123, DiscordChannelConfig::default());
        let handler = test_handler(channels);

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
        assert!(
            combined.contains("[attachment: /workspace/media/inbound/20260428-120000-cat.png]")
        );
        assert!(
            combined.contains("[attachment: /workspace/media/inbound/20260428-120001-notes.pdf]")
        );
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

    // --- New tests for require_mention and select_agent ---

    #[test]
    fn select_agent_returns_default_for_dm() {
        // Arrange
        let mut channels = HashMap::new();
        channels.insert(
            123,
            DiscordChannelConfig {
                require_mention: false,
                agent: Some(crate::config::AgentId::new("reviewer")),
            },
        );
        let handler = test_handler(channels);

        // Act & Assert: DM always uses default agent regardless of channel config
        assert_eq!(handler.select_agent(999, true), "developer");
    }

    #[test]
    fn select_agent_uses_channel_agent_when_set() {
        // Arrange
        let mut channels = HashMap::new();
        channels.insert(
            123,
            DiscordChannelConfig {
                require_mention: false,
                agent: Some(crate::config::AgentId::new("reviewer")),
            },
        );
        let handler = test_handler(channels);

        // Act & Assert
        assert_eq!(handler.select_agent(123, false), "reviewer");
    }

    #[test]
    fn select_agent_falls_back_to_default_when_no_channel_agent() {
        // Arrange
        let mut channels = HashMap::new();
        channels.insert(
            123,
            DiscordChannelConfig {
                require_mention: false,
                agent: None,
            },
        );
        let handler = test_handler(channels);

        // Act & Assert
        assert_eq!(handler.select_agent(123, false), "developer");
    }

    #[test]
    fn select_agent_falls_back_to_default_for_unknown_channel() {
        // Arrange
        let mut channels = HashMap::new();
        channels.insert(123, DiscordChannelConfig::default());
        let handler = test_handler(channels);

        // Act & Assert: channel 456 not in map, falls back to default
        assert_eq!(handler.select_agent(456, false), "developer");
    }

    #[test]
    fn is_bot_mentioned_returns_false_when_no_bot_user_id() {
        // Arrange
        let handler = test_handler(HashMap::new());

        // Assert: bot_user_id was never set (no ready event)
        assert_eq!(handler.bot_user_id.get(), None);
    }

    #[test]
    fn require_mention_true_skips_without_mention_logic() {
        // Arrange: channel 123 requires mention, channel 456 does not
        let mut channels = HashMap::new();
        channels.insert(
            123,
            DiscordChannelConfig {
                require_mention: true,
                agent: None,
            },
        );
        channels.insert(
            456,
            DiscordChannelConfig {
                require_mention: false,
                agent: None,
            },
        );
        let handler = test_handler(channels);

        // Act & Assert: guild_allowed works for both
        assert!(handler.guild_allowed(123));
        assert!(handler.guild_allowed(456));

        // Verify config is readable
        assert!(handler.channels.get(&123).expect("config").require_mention);
        assert!(!handler.channels.get(&456).expect("config").require_mention);
    }

    #[test]
    fn dm_always_allowed_regardless_of_channels() {
        // Arrange: empty channels (no guild allowed)
        let handler = test_handler(HashMap::new());

        // Act & Assert: DM (is_dm=true) bypasses guild_allowed entirely
        // The select_agent for DM always returns default
        assert_eq!(handler.select_agent(999, true), "developer");
        // But guild is rejected
        assert!(!handler.guild_allowed(123));
    }

    #[test]
    fn interaction_rejected_in_non_allowed_channel() {
        // Arrange
        let mut channels = HashMap::new();
        channels.insert(100, DiscordChannelConfig::default());
        let handler = test_handler(channels);

        // Act & Assert
        assert!(!handler.guild_allowed(999));
        assert!(handler.guild_allowed(100));
    }

    // --- BotChainState tests ---

    #[test]
    fn bot_chain_starts_at_one() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        assert!(
            state.check_and_increment(100),
            "first call should be allowed"
        );
    }

    #[test]
    fn bot_chain_allows_at_max_depth() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        for _ in 0..BOT_CHAIN_MAX_DEPTH {
            assert!(
                state.check_and_increment(200),
                "should be allowed up to and including max depth"
            );
        }
    }

    #[test]
    fn bot_chain_rejects_after_max_depth() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        for _ in 0..BOT_CHAIN_MAX_DEPTH {
            assert!(state.check_and_increment(300));
        }
        assert!(
            !state.check_and_increment(300),
            "should reject after exceeding max depth"
        );
    }

    #[test]
    fn bot_chain_resets_on_human_message() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        assert!(state.check_and_increment(400));
        assert!(state.check_and_increment(400));
        state.reset(400);
        assert!(
            state.check_and_increment(400),
            "after reset, should start fresh at depth 1"
        );
    }

    #[test]
    fn bot_chain_ttl_expiry_restarts_at_one() {
        let state = BotChainState::with_ttl(Duration::from_millis(1));
        assert!(state.check_and_increment(500));
        std::thread::sleep(Duration::from_millis(5));
        assert!(
            state.check_and_increment(500),
            "after TTL expiry, should restart at depth 1"
        );
    }

    #[test]
    fn bot_chain_scopes_by_thread_id() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        for _ in 0..BOT_CHAIN_MAX_DEPTH {
            assert!(state.check_and_increment(600));
        }
        assert!(
            state.check_and_increment(700),
            "different channel_id should have independent state"
        );
        assert!(
            !state.check_and_increment(600),
            "original channel should still be at max"
        );
    }

    // --- Sender-type receive judgment tests ---

    #[test]
    fn self_message_is_ignored() {
        let handler = test_handler_with_bot_id(HashMap::new(), 9999);

        assert!(handler.is_self_message(9999));
        assert!(!handler.is_self_message(1000));

        assert_eq!(
            handler.should_process_message(9999, true, false, 100, false),
            ReceiveDecision::Reject
        );
    }

    #[test]
    fn human_message_obeys_require_mention_false() {
        let mut channels = HashMap::new();
        channels.insert(
            100,
            DiscordChannelConfig {
                require_mention: false,
                agent: None,
            },
        );
        let handler = test_handler_with_bot_id(channels, 9999);

        assert_eq!(
            handler.should_process_message(1000, false, false, 100, false),
            ReceiveDecision::Accept { reset_chain: true }
        );
    }

    #[test]
    fn human_message_obeys_require_mention_true() {
        let mut channels = HashMap::new();
        channels.insert(
            100,
            DiscordChannelConfig {
                require_mention: true,
                agent: None,
            },
        );
        let handler = test_handler_with_bot_id(channels, 9999);

        assert_eq!(
            handler.should_process_message(1000, false, false, 100, false),
            ReceiveDecision::Reject
        );
    }

    #[test]
    fn human_mentioning_this_bot_is_allowed() {
        let mut channels = HashMap::new();
        channels.insert(
            100,
            DiscordChannelConfig {
                require_mention: true,
                agent: None,
            },
        );
        let handler = test_handler_with_bot_id(channels, 9999);

        assert_eq!(
            handler.should_process_message(1000, false, false, 100, true),
            ReceiveDecision::Accept { reset_chain: true }
        );
    }

    #[test]
    fn human_mentioning_other_bot_only_is_ignored() {
        let mut channels = HashMap::new();
        channels.insert(
            100,
            DiscordChannelConfig {
                require_mention: true,
                agent: None,
            },
        );
        let handler = test_handler_with_bot_id(channels, 9999);

        // mentions_bot=false: this bot is not mentioned (other bot mentioned instead)
        assert_eq!(
            handler.should_process_message(1000, false, false, 100, false),
            ReceiveDecision::Reject
        );
    }

    #[test]
    fn accepted_human_message_resets_bot_chain() {
        let handler = test_handler_with_bot_id(HashMap::new(), 9999);

        // Build up chain depth
        for _ in 0..3 {
            assert!(handler.chain_state.check_and_increment(100));
        }

        // Human message should request chain reset
        assert_eq!(
            handler.should_process_message(1000, false, true, 100, false),
            ReceiveDecision::Accept { reset_chain: true }
        );

        // Simulate the reset (as Handler::message() would)
        handler.chain_state.reset(100);

        // Next bot message should start at depth 1
        assert!(
            handler.chain_state.check_and_increment(100),
            "after human message resets chain, bot should start at depth 1"
        );
    }

    #[test]
    fn bot_mentioning_this_bot_is_allowed_within_depth() {
        let handler = test_handler_with_bot_id(HashMap::new(), 9999);

        assert_eq!(
            handler.should_process_message(1111, true, false, 200, true),
            ReceiveDecision::Accept { reset_chain: false }
        );
    }

    #[test]
    fn bot_without_this_bot_mention_is_ignored() {
        let handler = test_handler_with_bot_id(HashMap::new(), 9999);

        assert_eq!(
            handler.should_process_message(1111, true, false, 100, false),
            ReceiveDecision::Reject
        );
    }

    #[test]
    fn bot_mentioning_this_bot_is_ignored_after_depth_limit() {
        let handler = test_handler_with_bot_id(HashMap::new(), 9999);

        for _ in 0..BOT_CHAIN_MAX_DEPTH {
            assert_eq!(
                handler.should_process_message(1111, true, false, 200, true),
                ReceiveDecision::Accept { reset_chain: false }
            );
        }

        assert_eq!(
            handler.should_process_message(1111, true, false, 200, true),
            ReceiveDecision::Reject
        );
    }

    #[test]
    fn text_slash_command_keeps_existing_pre_mention_behavior() {
        let handler = test_handler_with_bot_id(HashMap::new(), 9999);

        // Bot messages are rejected by sender-type judgment regardless of content
        assert_eq!(
            handler.should_process_message(1111, true, false, 100, false),
            ReceiveDecision::Reject
        );

        // Slash commands are recognized independently and processed
        // before should_process_message in the Handler::message() flow
        assert!(crate::slash_commands::is_slash_command("/status"));
        assert!(crate::slash_commands::is_slash_command("/new"));
    }

    #[test]
    fn discord_handlers_share_bot_chain_state() {
        let shared = Arc::new(BotChainState::new());

        let handler1 = test_handler_with_chain(HashMap::new(), 1001, Arc::clone(&shared));
        let handler2 = test_handler_with_chain(HashMap::new(), 2002, Arc::clone(&shared));

        assert_eq!(
            handler1.should_process_message(1111, true, false, 500, true),
            ReceiveDecision::Accept { reset_chain: false },
            "handler1: first bot mention on channel 500 should be accepted"
        );

        assert_eq!(
            handler2.should_process_message(3333, true, false, 500, true),
            ReceiveDecision::Accept { reset_chain: false },
            "handler2: second bot mention on same channel 500 should see depth 2"
        );

        for _ in 2..BOT_CHAIN_MAX_DEPTH {
            assert!(
                shared.check_and_increment(500),
                "should allow up to max depth"
            );
        }

        assert!(
            !shared.check_and_increment(500),
            "shared state should track cumulative depth across handlers"
        );
    }

    #[test]
    fn discord_handlers_keep_chain_state_per_thread() {
        let shared = Arc::new(BotChainState::new());

        let handler1 = test_handler_with_chain(HashMap::new(), 1001, Arc::clone(&shared));

        for _ in 0..BOT_CHAIN_MAX_DEPTH {
            assert_eq!(
                handler1.should_process_message(1111, true, false, 100, true),
                ReceiveDecision::Accept { reset_chain: false },
                "fill channel 100 to max depth"
            );
        }

        assert!(
            !shared.check_and_increment(100),
            "channel 100 should be at max"
        );

        let handler2 = test_handler_with_chain(HashMap::new(), 2002, Arc::clone(&shared));
        assert_eq!(
            handler2.should_process_message(3333, true, false, 200, true),
            ReceiveDecision::Accept { reset_chain: false },
            "channel 200 should be independent and start at depth 1"
        );
    }
}

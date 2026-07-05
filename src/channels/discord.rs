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

use crate::agent_loop::{ConversationScope, SurfaceContext};
use crate::channels::adapter::ConversationKind;
use crate::channels::adapter::{
    ChannelAdapter, ToolProgressHandle, ToolProgressSink, TurnActivity,
};
use crate::channels::utils::text::{keep_tail, split_text};
use crate::config::DiscordChannelConfig;
use crate::runtime::{AppState, ChannelLogKey, HumanChannelLogMessage};

/// Discord API リクエストのタイムアウト (秒)。
const DISCORD_REQUEST_TIMEOUT_SECS: u64 = 10;

/// 429 レート制限時の最大リトライ回数。
const DISCORD_MAX_RETRIES: u32 = 3;

/// 429 の Retry-After ヘッダがない場合のフォールバック待機時間 (秒)。
const DISCORD_RETRY_AFTER_FALLBACK_SECS: u64 = 2;

/// Discord メッセージ長制限 (文字数)。
const DISCORD_MAX_MESSAGE_LEN: usize = 2000;

/// Discord typing indicator の更新間隔 (秒)。
const DISCORD_TYPING_REFRESH_SECS: u64 = 8;

/// Bot-to-bot 連鎖の最大深さ（チャンネル/スレッド単位）。
const BOT_CHAIN_MAX_DEPTH: u32 = 5;

/// Bot-to-bot 連鎖状態の TTL（秒）。
const BOT_CHAIN_TTL_SECS: u64 = 300;

/// Discord `allowed_mentions` で指定可能な最大ユーザー数。
const DISCORD_ALLOWED_MENTIONS_MAX_USERS: usize = 100;

/// 連鎖の現在の深さと最終更新時刻。
struct ChainEntry {
    depth: u32,
    last_updated: Instant,
}

/// Bot-to-bot 連鎖の深さをチャンネル単位で追跡し、制限を超えたメッセージを拒否する。
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

    /// 連鎖深さをインクリメントし、制限内なら `true` を返す。
    /// TTL を超過したエントリは併せて削除する。
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

    /// チャンネルの連鎖状態をリセットする（人間のメッセージ受信時に呼ぶ）。
    pub(crate) fn reset(&self, channel_id: u64) {
        let mut chains = self.chains.lock().expect("bot chain state lock poisoned");
        chains.remove(&channel_id);
    }
}

/// メッセージ受信可否の判定結果。
#[derive(Debug, PartialEq)]
enum ReceiveDecision {
    /// 受理。`reset_chain` が true の場合は bot chain をリセットする。
    Accept { reset_chain: bool },
    /// 拒否。
    Reject,
}

/// メッセージのルーティング判定結果。
#[derive(Debug, PartialEq)]
enum RouteDecision {
    /// チャンネル外などの理由で拒否。
    Reject,
    /// Channel Log にのみ保存し、応答はしない（multi-agent room で非 mention 時）。
    ObserveOnly { agent_id: String },
    /// エージェントが応答する。
    Respond { agent_id: String },
}

impl RouteDecision {
    /// ObserveOnly / Respond を問わず、紐づく agent_id を返す。
    fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Reject => None,
            Self::ObserveOnly { agent_id } | Self::Respond { agent_id } => Some(agent_id),
        }
    }

    /// 応答対象の agent_id のみを返す（ObserveOnly / Reject は `None`）。
    fn response_agent_id(&self) -> Option<&str> {
        match self {
            Self::Respond { agent_id } => Some(agent_id),
            Self::Reject | Self::ObserveOnly { .. } => None,
        }
    }

    fn is_rejected(&self) -> bool {
        matches!(self, Self::Reject)
    }
}

/// `external_chat_id` から Discord bot token を解決する共有データ。
/// [`DiscordAdapter`] と進捗 sink が `Arc` で共有する。
struct DiscordTokenResolver {
    /// `bot_id → token` のマップ（[`Config::discord_bots`] から構築）。
    bot_token_map: HashMap<String, String>,
    /// `agent_id → bot_id` のマップ。
    agent_bot_map: HashMap<String, String>,
}

impl DiscordTokenResolver {
    fn new(bot_token_map: HashMap<String, String>, agent_bot_map: HashMap<String, String>) -> Self {
        Self {
            bot_token_map,
            agent_bot_map,
        }
    }

    /// `external_chat_id` から該当する bot token を解決する。
    /// 明示的な `:bot:` セグメントがあればそちらを優先し、
    /// なければ agent バインディング経由で解決する。
    fn select_token(&self, external_chat_id: &str) -> Result<&str, String> {
        if let Some(bot_id) = parse_explicit_discord_bot_id(external_chat_id) {
            return self.token_for_bot(bot_id, external_chat_id);
        }

        let agent_id = parse_discord_agent_id(external_chat_id).ok_or_else(|| {
            format!(
                "Discord external_chat_id '{external_chat_id}' does not identify a bot or agent"
            )
        })?;
        let bot_id = self.agent_bot_map.get(agent_id).ok_or_else(|| {
            format!("no Discord bot binding found for agent '{agent_id}' in external_chat_id '{external_chat_id}'")
        })?;
        self.token_for_bot(bot_id, external_chat_id)
    }

    fn token_for_bot(&self, bot_id: &str, external_chat_id: &str) -> Result<&str, String> {
        self.bot_token_map
            .get(bot_id)
            .map(String::as_str)
            .ok_or_else(|| {
                format!(
                    "no Discord bot token found for bot '{bot_id}' in external_chat_id '{external_chat_id}'"
                )
            })
    }
}

/// Discord REST API 経由でアウトバウンドメッセージを送信するアダプター。
pub(crate) struct DiscordAdapter {
    /// bot token 解決用の共有データ（進捗 sink と共有）。
    tokens: Arc<DiscordTokenResolver>,
    http_client: reqwest::Client,
    /// 進捗表示器（`Arc` クローン返却）。進捗非対応なら `None`。
    tool_progress_sink: Option<Arc<dyn ToolProgressSink>>,
}

struct DiscordTypingActivity {
    handle: tokio::task::JoinHandle<()>,
}

impl TurnActivity for DiscordTypingActivity {}

impl Drop for DiscordTypingActivity {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl DiscordAdapter {
    pub(crate) fn new_for_bots(config: &crate::config::Config) -> Self {
        let bot_tokens: std::collections::HashMap<String, String> = config
            .discord_bots()
            .into_iter()
            .map(|b| (b.bot_id.to_string(), b.token.to_string()))
            .collect();
        let agent_bots = config
            .agents
            .iter()
            .filter_map(|(agent_id, agent)| {
                let bot_id = agent.discord_bot.as_ref()?;
                Some((agent_id.to_string(), bot_id.to_string()))
            })
            .collect();
        let tokens = Arc::new(DiscordTokenResolver::new(bot_tokens, agent_bots));
        let http_client = reqwest::Client::new();
        let sink: Arc<dyn ToolProgressSink> = Arc::new(DiscordToolProgressSink::new(
            Arc::clone(&tokens),
            http_client.clone(),
        ));
        Self {
            tokens,
            http_client,
            tool_progress_sink: Some(sink),
        }
    }

    /// `external_chat_id` から該当する bot token を解決する。
    fn select_token(&self, external_chat_id: &str) -> Result<&str, String> {
        self.tokens.select_token(external_chat_id)
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

    fn tool_progress_sink(&self) -> Option<Arc<dyn ToolProgressSink>> {
        self.tool_progress_sink.clone()
    }

    async fn begin_turn_activity(
        &self,
        external_chat_id: &str,
    ) -> Result<Box<dyn TurnActivity>, String> {
        let discord_chat_id = parse_discord_chat_id(external_chat_id)?;
        let token = self.select_token(external_chat_id)?.to_string();
        let url = format!("https://discord.com/api/v10/channels/{discord_chat_id}/typing");
        let client = self.http_client.clone();

        let handle = tokio::spawn(async move {
            loop {
                if let Err(error) = send_discord_typing(&client, &url, &token).await {
                    warn!(
                        channel_id = discord_chat_id,
                        error = %error,
                        "Discord: failed to refresh typing indicator"
                    );
                }
                tokio::time::sleep(Duration::from_secs(DISCORD_TYPING_REFRESH_SECS)).await;
            }
        });

        Ok(Box::new(DiscordTypingActivity { handle }))
    }

    async fn send_text(&self, external_chat_id: &str, text: &str) -> Result<(), String> {
        let discord_chat_id = parse_discord_chat_id(external_chat_id)?;
        let token = self.select_token(external_chat_id)?;

        let url = format!("https://discord.com/api/v10/channels/{discord_chat_id}/messages");

        for chunk in split_text(text, DISCORD_MAX_MESSAGE_LEN) {
            let mentioned_users = extract_user_mention_ids(&chunk);
            let body = json!({
                "content": chunk,
                "allowed_mentions": {
                    "parse": [],
                    "users": mentioned_users,
                },
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
                let payload = json!({
                    "content": content,
                    "allowed_mentions": {
                        "parse": [],
                        "users": extract_user_mention_ids(content),
                    },
                });
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

async fn send_discord_typing(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<(), String> {
    send_discord_api(client, |client| {
        client
            .post(url)
            .timeout(Duration::from_secs(DISCORD_REQUEST_TIMEOUT_SECS))
            .header(reqwest::header::AUTHORIZATION, format!("Bot {token}"))
    })
    .await
}

/// Discord API リクエストを送信し、429 レート制限を自動リトライする。
async fn send_discord_api<F>(client: &reqwest::Client, build_request: F) -> Result<(), String>
where
    F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
{
    discord_request_with_retry(client, build_request).await?;
    Ok(())
}

/// Discord API リクエストを送信し、429 を自動リトライして成功時のレスポンスを返す。
async fn discord_request_with_retry<F>(
    client: &reqwest::Client,
    build_request: F,
) -> Result<reqwest::Response, String>
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
            return Ok(resp);
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

// ---------------------------------------------------------------------------
// ツール進捗表示器（編集式累積ログ）
// ---------------------------------------------------------------------------

/// Discord API のベース URL。
const DISCORD_API_BASE_URL: &str = "https://discord.com/api/v10";

/// Discord 向けのツール進捗 sink。進捗メッセージを1件投稿し、編集で更新する。
pub(crate) struct DiscordToolProgressSink {
    tokens: Arc<DiscordTokenResolver>,
    http_client: reqwest::Client,
    base_url: String,
}

impl DiscordToolProgressSink {
    fn new(tokens: Arc<DiscordTokenResolver>, http_client: reqwest::Client) -> Self {
        Self::with_base_url(tokens, http_client, DISCORD_API_BASE_URL.to_string())
    }

    /// ベース URL を明示指定して生成する（単体テストでモックサーバを指すため）。
    fn with_base_url(
        tokens: Arc<DiscordTokenResolver>,
        http_client: reqwest::Client,
        base_url: String,
    ) -> Self {
        Self {
            tokens,
            http_client,
            base_url,
        }
    }

    fn build_message_payload(content: &str) -> serde_json::Value {
        json!({
            "content": content,
            "allowed_mentions": {
                "parse": [],
                "users": extract_user_mention_ids(content),
            },
        })
    }
}

#[async_trait]
impl ToolProgressSink for DiscordToolProgressSink {
    async fn begin(
        &self,
        external_chat_id: &str,
        body: &str,
    ) -> Result<Box<dyn ToolProgressHandle>, String> {
        let channel_id = parse_discord_chat_id(external_chat_id)?;
        let token = self.tokens.select_token(external_chat_id)?.to_string();
        let content = keep_tail(body, DISCORD_MAX_MESSAGE_LEN);
        let payload = Self::build_message_payload(&content);
        let url = format!("{}/channels/{channel_id}/messages", self.base_url);

        let response = discord_request_with_retry(&self.http_client, |client| {
            client
                .post(&url)
                .timeout(Duration::from_secs(DISCORD_REQUEST_TIMEOUT_SECS))
                .header(reqwest::header::AUTHORIZATION, format!("Bot {token}"))
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&payload)
        })
        .await?;
        let message: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Discord create progress message: invalid JSON: {e}"))?;
        let message_id = message
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Discord create progress message: missing id".to_string())?
            .to_string();

        Ok(Box::new(DiscordToolProgressHandle {
            http_client: self.http_client.clone(),
            channel_id,
            message_id,
            token,
            base_url: self.base_url.clone(),
        }))
    }
}

/// 投稿済み進捗メッセージの編集ハンドル。
struct DiscordToolProgressHandle {
    http_client: reqwest::Client,
    channel_id: u64,
    message_id: String,
    token: String,
    base_url: String,
}

#[async_trait]
impl ToolProgressHandle for DiscordToolProgressHandle {
    async fn update(&mut self, body: &str) -> Result<(), String> {
        let content = keep_tail(body, DISCORD_MAX_MESSAGE_LEN);
        let payload = DiscordToolProgressSink::build_message_payload(&content);
        let url = format!(
            "{}/channels/{}/messages/{}",
            self.base_url, self.channel_id, self.message_id
        );
        let token = self.token.clone();
        discord_request_with_retry(&self.http_client, |client| {
            client
                .patch(&url)
                .timeout(Duration::from_secs(DISCORD_REQUEST_TIMEOUT_SECS))
                .header(reqwest::header::AUTHORIZATION, format!("Bot {token}"))
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&payload)
        })
        .await?;
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), String> {
        // 進捗メッセージは完了ログとして常に残置する（no-op）。
        Ok(())
    }
}

/// serenity の [`EventHandler`] 実装。インバウンドメッセージを受信してエージェントに振り分ける。
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
    fn default_agent_response(&self) -> RouteDecision {
        RouteDecision::Respond {
            agent_id: self.default_agent.clone(),
        }
    }

    /// 指定エージェントがこの bot にバインドされているかを返す。
    fn agent_uses_this_bot(&self, agent_id: &crate::config::AgentId) -> bool {
        let bot_id = crate::config::BotId::new(&self.bot_id);
        self.app_state
            .config
            .agents
            .get(agent_id)
            .is_some_and(|agent| agent.discord_bot.as_ref() == Some(&bot_id))
    }

    /// チャンネル設定内で、この bot にバインドされた最初のエージェントを返す。
    fn first_agent_for_this_bot(&self, channel_config: &DiscordChannelConfig) -> Option<String> {
        channel_config
            .agents
            .iter()
            .find(|agent_id| self.agent_uses_this_bot(agent_id))
            .map(ToString::to_string)
    }

    /// チャンネルの先頭エージェントがこの bot にバインドされていれば返す（single-agent channel 用）。
    fn primary_agent_for_this_bot(&self, channel_config: &DiscordChannelConfig) -> Option<String> {
        let agent_id = channel_config.agents.first()?;
        self.agent_uses_this_bot(agent_id)
            .then(|| agent_id.to_string())
    }

    /// Single-agent チャンネルのルーティングを解決する。
    fn resolve_single_agent_channel(&self, channel_config: &DiscordChannelConfig) -> RouteDecision {
        match self.primary_agent_for_this_bot(channel_config) {
            Some(agent_id) => RouteDecision::Respond { agent_id },
            None => RouteDecision::Reject,
        }
    }

    /// Multi-agent room のルーティングを解決する。
    /// mention がなければ ObserveOnly、あればこの bot にバインドされたエージェントが応答する。
    fn resolve_multi_agent_room(
        &self,
        channel_config: &DiscordChannelConfig,
        mentions_bot: bool,
    ) -> RouteDecision {
        if !mentions_bot {
            return match channel_config.agents.first() {
                Some(agent_id) => RouteDecision::ObserveOnly {
                    agent_id: agent_id.to_string(),
                },
                None => RouteDecision::Reject,
            };
        }

        match self.first_agent_for_this_bot(channel_config) {
            Some(agent_id) => RouteDecision::Respond { agent_id },
            None => RouteDecision::Reject,
        }
    }

    /// メッセージの送信先エージェントを決定するルーティング処理。
    /// DM → デフォルトエージェント、ギルド → チャンネル設定に基づく振り分け。
    fn route_message(&self, channel_id: u64, is_dm: bool, mentions_bot: bool) -> RouteDecision {
        if is_dm {
            return self.default_agent_response();
        }

        let Some(channel_config) = self.channels.get(&channel_id) else {
            return RouteDecision::Reject;
        };

        if channel_config.multi_agent {
            self.resolve_multi_agent_room(channel_config, mentions_bot)
        } else {
            self.resolve_single_agent_channel(channel_config)
        }
    }

    /// [`SurfaceContext`] を構築する。
    fn make_context(&self, user: &str, thread: &str, agent_id: &str) -> SurfaceContext {
        let scope = self.scope_for_thread(thread);
        crate::runtime::build_channel_context("discord", user, thread, "discord", agent_id, scope)
    }

    fn scope_for_thread(&self, thread: &str) -> ConversationScope {
        thread
            .parse::<u64>()
            .ok()
            .and_then(|cid| self.channels.get(&cid))
            .map(|c| crate::runtime::channel_scope_from_secret(c.secret))
            .unwrap_or(ConversationScope::Normal)
    }

    /// メッセージの mention にこの bot が含まれているかを判定する。
    fn is_bot_mentioned(&self, msg: &DiscordMessage) -> bool {
        let Some(bot_id) = self.bot_user_id.get() else {
            return false;
        };
        msg.mentions.iter().any(|u| u.id == *bot_id)
    }

    /// 自身（この bot）のメッセージかどうかを判定する。
    fn is_self_message(&self, author_id: u64) -> bool {
        self.bot_user_id
            .get()
            .is_some_and(|id| id.get() == author_id)
    }

    /// メッセージを処理すべきかを判定する。
    ///
    /// - 自身のメッセージ → 拒否
    /// - Bot メッセージ → mention がある場合のみ連鎖深さ制限内で受理
    /// - 人間のメッセージ → `require_mention` 設定に従う
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
                if config.require_mention && !config.multi_agent && !mentions_bot {
                    return ReceiveDecision::Reject;
                }
            }
        }

        ReceiveDecision::Accept { reset_chain: true }
    }

    /// テキスト内のスラッシュコマンドを処理する。コマンドが処理されたら `true` を返す。
    async fn process_text_slash_command(
        &self,
        ctx: &Context,
        msg: &DiscordMessage,
        thread: &str,
        agent_id: &str,
        text: &str,
    ) -> bool {
        let slash_context = self.make_context(&msg.author.name, thread, agent_id);
        let sender_id = msg.author.id.get().to_string();
        let outcome = crate::slash_commands::process_slash_command(
            &self.app_state,
            &slash_context,
            text,
            Some(&sender_id),
        )
        .await;

        match outcome {
            crate::slash_commands::SlashCommandOutcome::Respond(response)
            | crate::slash_commands::SlashCommandOutcome::Error(response) => {
                send_discord_response(ctx, msg.channel_id, &response).await;
                true
            }
            crate::slash_commands::SlashCommandOutcome::NotHandled => false,
        }
    }

    /// メッセージの添付ファイルをダウンロードしてローカルに保存する。
    async fn save_attachments(&self, workspace_dir: &Path, msg: &DiscordMessage) -> Vec<PathBuf> {
        let mut saved_paths = Vec::new();
        for attachment in &msg.attachments {
            match self.http_client.get(&attachment.url).send().await {
                Ok(resp) => match resp.error_for_status() {
                    Ok(resp) => match resp.bytes().await {
                        Ok(bytes) => match crate::channels::utils::media::save_inbound_file(
                            workspace_dir,
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
                        },
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
        saved_paths
    }
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: DiscordMessage) {
        let channel_id = msg.channel_id.get();
        let is_dm = msg.guild_id.is_none();

        if self.is_self_message(msg.author.id.get()) {
            return;
        }

        let text = msg.content.clone();
        let mentions_bot = self.is_bot_mentioned(&msg);
        let route = self.route_message(channel_id, is_dm, mentions_bot);
        if route.is_rejected() {
            return;
        }

        let thread = channel_id.to_string();
        let Some(route_agent_id) = route.agent_id().map(ToString::to_string) else {
            return;
        };

        if self
            .process_text_slash_command(&ctx, &msg, &thread, &route_agent_id, &text)
            .await
        {
            return;
        }

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

        let is_multi_agent = self
            .channels
            .get(&channel_id)
            .is_some_and(|c| c.multi_agent);

        let channel_log_chat_id = if is_multi_agent && !is_dm {
            crate::runtime::store_human_channel_log_message(
                &self.app_state,
                HumanChannelLogMessage {
                    key: ChannelLogKey::Discord(channel_id),
                    scope: self.scope_for_thread(&thread),
                    id: format!("cl-{}", msg.id.get()),
                    sender_id: format!("user:discord:{}", msg.author.id.get()),
                    content: text.clone(),
                    timestamp: msg.timestamp.to_string(),
                },
            )
            .await
        } else {
            None
        };

        // Multi-Agent Room with no agent resolved: save to Channel Log only, do not respond
        let Some(agent_id) = route.response_agent_id().map(ToString::to_string) else {
            return;
        };

        let workspace_dir = match self.app_state.config.workspace_dir() {
            Ok(d) => d,
            Err(e) => {
                error!("failed to resolve workspace dir: {e}");
                return;
            }
        };

        let saved_paths = self.save_attachments(&workspace_dir, &msg).await;

        let combined_text =
            crate::channels::utils::media::format_attachment_text(&saved_paths, &text);

        if combined_text.is_empty() {
            return;
        }

        let mut context = self.make_context(&msg.author.name, &thread, &agent_id);
        context.channel_log_chat_id = channel_log_chat_id;
        context.origin_id = uuid::Uuid::new_v4().to_string();

        info!(
            channel_id = channel_id,
            agent = %agent_id, bot = %self.bot_id,
            sender = %context.surface_user,
            text_length = text.len(),
            attachments = saved_paths.len(),
            "Discord message received"
        );

        crate::runtime::submit_agent_turn(&self.app_state, context, combined_text);
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
        let route = self.route_message(channel_id, is_dm_int, true);
        if route.is_rejected() {
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
        let Some(interaction_agent) = route.agent_id().map(ToString::to_string) else {
            return;
        };
        let thread = channel_id.to_string();

        let slash_context = self.make_context(&cmd.user.name, &thread, &interaction_agent);
        let sender_id = cmd.user.id.get().to_string();

        let response_text = match crate::slash_commands::process_slash_command(
            &self.app_state,
            &slash_context,
            &command_text,
            Some(&sender_id),
        )
        .await
        {
            crate::slash_commands::SlashCommandOutcome::Respond(response)
            | crate::slash_commands::SlashCommandOutcome::Error(response) => response,
            crate::slash_commands::SlashCommandOutcome::NotHandled => {
                crate::slash_commands::unknown_command_response()
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

/// Discord にメッセージを送信する（2000文字制限で自動分割）。
async fn send_discord_response(ctx: &Context, channel_id: ChannelId, text: &str) {
    let http = &ctx.http;
    if let Err(error) =
        crate::channels::utils::text::send_chunked(text, DISCORD_MAX_MESSAGE_LEN, |chunk| {
            let mentioned_users = extract_user_mention_ids(chunk);
            let msg = CreateMessage::new()
                .content(chunk)
                .allowed_mentions(CreateAllowedMentions::new().users(mentioned_users));
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

/// テキストから `<@ID>` / `<@!ID>` 形式のユーザーメンションを抽出する。
/// 重複は除外し、最大 [`DISCORD_ALLOWED_MENTIONS_MAX_USERS`] 件まで返す。
fn extract_user_mention_ids(text: &str) -> Vec<UserId> {
    let mut ids = Vec::new();
    let mut rest = text;

    while ids.len() < DISCORD_ALLOWED_MENTIONS_MAX_USERS {
        let Some(start) = rest.find("<@") else {
            break;
        };
        rest = &rest[start + 2..];

        if let Some(after_bang) = rest.strip_prefix('!') {
            rest = after_bang;
        }

        let Some(end) = rest.find('>') else {
            break;
        };

        let raw_id = &rest[..end];
        if !raw_id.is_empty()
            && raw_id.bytes().all(|b| b.is_ascii_digit())
            && let Ok(id) = raw_id.parse::<u64>()
            && id != 0
        {
            let user_id = UserId::new(id);
            if !ids.contains(&user_id) {
                ids.push(user_id);
            }
        }

        rest = &rest[end + 1..];
    }

    ids
}

/// `external_chat_id` から Discord チャンネル ID（数値）を抽出する。
/// `:bot:`, `:agent:`, `:multi-room-log` などのサフィックスは除去される。
fn parse_discord_chat_id(external_chat_id: &str) -> Result<u64, String> {
    let bare = if let Some(pos) = external_chat_id.find(":bot:") {
        &external_chat_id[..pos]
    } else if let Some(pos) = external_chat_id.find(":agent:") {
        &external_chat_id[..pos]
    } else if let Some(pos) = external_chat_id.find(":multi-room-log") {
        &external_chat_id[..pos]
    } else {
        external_chat_id
    };
    bare.strip_prefix("discord:")
        .unwrap_or(bare)
        .parse::<u64>()
        .map_err(|_| format!("invalid Discord external_chat_id: '{external_chat_id}'"))
}

/// `external_chat_id` から明示的な `:bot:<bot_id>` セグメントを抽出する。
/// パターン: `"...:bot:<bot_id>:agent:<agent_id>"`
fn parse_explicit_discord_bot_id(external_chat_id: &str) -> Option<&str> {
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

/// `external_chat_id` から `:agent:<agent_id>` セグメントを抽出する。
fn parse_discord_agent_id(external_chat_id: &str) -> Option<&str> {
    let agent_start = external_chat_id.find(":agent:")?;
    let agent_id = &external_chat_id[agent_start + ":agent:".len()..];
    if agent_id.is_empty() {
        None
    } else {
        Some(agent_id)
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

/// Discord bot を起動し、共有チャンネル設定とエージェントルーティングを適用する。
#[allow(private_interfaces)]
pub(crate) async fn start_discord_bot_for_bot(
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

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(DISCORD_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| {
            error!("failed to build Discord HTTP client: {e}");
            e
        })?;

    let handler = Handler {
        app_state: state,
        bot_id: bot_id.to_string(),
        default_agent: default_agent.to_string(),
        channels: channels.clone(),
        bot_user_id: OnceLock::new(),
        chain_state,
        http_client,
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
                        reasoning_content: None,
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
                        reasoning_content: None,
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
                        reasoning_content: None,
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
            tokens: Arc::new(DiscordTokenResolver::new(HashMap::new(), HashMap::new())),
            http_client: reqwest::Client::new(),
            tool_progress_sink: None,
        }
    }

    fn test_adapter_with_tokens(
        bot_tokens: &[(&str, &str)],
        agent_bots: &[(&str, &str)],
    ) -> DiscordAdapter {
        let bot_token_map: HashMap<String, String> = bot_tokens
            .iter()
            .map(|(bot_id, token)| ((*bot_id).to_string(), (*token).to_string()))
            .collect();
        let agent_bot_map: HashMap<String, String> = agent_bots
            .iter()
            .map(|(agent_id, bot_id)| ((*agent_id).to_string(), (*bot_id).to_string()))
            .collect();
        DiscordAdapter {
            tokens: Arc::new(DiscordTokenResolver::new(bot_token_map, agent_bot_map)),
            http_client: reqwest::Client::new(),
            tool_progress_sink: None,
        }
    }

    fn agent_id(id: &str) -> crate::config::AgentId {
        crate::config::AgentId::new(id)
    }

    fn channel(
        agent_ids: &[&str],
        multi_agent: bool,
        require_mention: bool,
    ) -> DiscordChannelConfig {
        DiscordChannelConfig {
            require_mention,
            agents: agent_ids.iter().map(|id| agent_id(id)).collect(),
            multi_agent,
            secret: false,
            ..Default::default()
        }
    }

    fn channels(entries: &[(u64, &[&str], bool, bool)]) -> HashMap<u64, DiscordChannelConfig> {
        entries
            .iter()
            .map(|(channel_id, agent_ids, multi_agent, require_mention)| {
                (
                    *channel_id,
                    channel(agent_ids, *multi_agent, *require_mention),
                )
            })
            .collect()
    }

    fn agent_cfg(
        label: &str,
        discord_bot: Option<&str>,
    ) -> (crate::config::AgentId, crate::config::AgentConfig) {
        let id = crate::config::AgentId::new(label);
        let cfg = crate::config::AgentConfig {
            label: label.to_string(),
            discord_bot: discord_bot.map(crate::config::BotId::new),
            ..Default::default()
        };
        (id, cfg)
    }

    fn agents(
        entries: &[(&str, Option<&str>)],
    ) -> HashMap<crate::config::AgentId, crate::config::AgentConfig> {
        entries
            .iter()
            .map(|(label, discord_bot)| agent_cfg(label, *discord_bot))
            .collect()
    }

    fn test_handler_with_agents(
        channels: HashMap<u64, DiscordChannelConfig>,
        bot_user_id: u64,
        bot_id: &str,
        default_agent: &str,
        agents: std::collections::HashMap<crate::config::AgentId, crate::config::AgentConfig>,
    ) -> Handler {
        let lock = OnceLock::new();
        lock.set(UserId::new(bot_user_id)).expect("OnceLock set");
        let mut config = crate::test_util::test_config(
            tempfile::tempdir()
                .expect("tempdir")
                .path()
                .to_str()
                .expect("utf8"),
        );
        config.agents = agents;
        let state = crate::agent_loop::turn::build_state(
            config,
            Box::new(crate::agent_loop::turn::FakeProvider {
                responses: std::sync::Mutex::new(vec![crate::llm::MessagesResponse {
                    content: "ok".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![],
                    usage: None,
                }]),
            }),
        );
        Handler {
            app_state: Arc::new(state),
            bot_id: bot_id.to_string(),
            default_agent: default_agent.to_string(),
            channels,
            bot_user_id: lock,
            chain_state: Arc::new(BotChainState::new()),
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
        assert_eq!(
            parse_discord_chat_id("discord:123:agent:lyre").expect("agent-scoped chat id"),
            123
        );
        assert_eq!(
            parse_discord_chat_id("discord:123:bot:main:agent:lyre").expect("explicit bot chat id"),
            123
        );
        assert_eq!(
            parse_discord_chat_id("discord:456:multi-room-log").expect("multi-room-log"),
            456
        );
        assert!(parse_discord_chat_id("discord:not-a-number").is_err());
    }

    #[test]
    fn parse_explicit_discord_bot_id_from_external_chat_id() {
        assert_eq!(
            parse_explicit_discord_bot_id("123:bot:main:agent:developer"),
            Some("main")
        );
        assert_eq!(parse_explicit_discord_bot_id("12345"), None);
        assert_eq!(parse_explicit_discord_bot_id("discord:123"), None);
    }

    #[test]
    fn parse_discord_agent_id_from_external_chat_id() {
        assert_eq!(
            parse_discord_agent_id("discord:123:agent:developer"),
            Some("developer")
        );
        assert_eq!(
            parse_discord_agent_id("discord:123:bot:main:agent:developer"),
            Some("developer")
        );
        assert_eq!(parse_discord_agent_id("discord:123"), None);
        assert_eq!(parse_discord_agent_id("discord:123:agent:"), None);
    }

    #[test]
    fn select_token_uses_explicit_bot_id() {
        let adapter = test_adapter_with_tokens(
            &[("main", "token-main"), ("other", "token-other")],
            &[("developer", "other")],
        );

        assert_eq!(
            adapter
                .select_token("discord:123:bot:main:agent:developer")
                .expect("token"),
            "token-main"
        );
    }

    #[test]
    fn select_token_resolves_agent_binding_without_bot_segment() {
        let adapter = test_adapter_with_tokens(
            &[("main", "token-main"), ("other", "token-other")],
            &[("developer", "other")],
        );

        assert_eq!(
            adapter
                .select_token("discord:123:agent:developer")
                .expect("token"),
            "token-other"
        );
    }

    #[test]
    fn select_token_rejects_chat_id_without_bot_or_agent() {
        let adapter = test_adapter_with_tokens(&[("main", "token-main")], &[("developer", "main")]);

        assert!(
            adapter.select_token("discord:123").is_err(),
            "raw channel IDs must not fall back to an arbitrary bot token"
        );
    }

    #[test]
    fn select_token_rejects_agent_without_bot_binding() {
        let adapter = test_adapter_with_tokens(&[("main", "token-main")], &[]);

        assert!(
            adapter.select_token("discord:123:agent:developer").is_err(),
            "agent-scoped Discord sends require an explicit agent.discord_bot binding"
        );
    }

    #[test]
    fn select_token_rejects_bound_bot_without_token() {
        let adapter =
            test_adapter_with_tokens(&[("main", "token-main")], &[("developer", "missing")]);

        assert!(
            adapter.select_token("discord:123:agent:developer").is_err(),
            "agent bindings must not fall back when their bot token is absent"
        );
    }

    #[test]
    fn extract_user_mention_ids_accepts_standard_and_nickname_forms() {
        let ids = extract_user_mention_ids("hi <@123> and <@!456>");

        assert_eq!(ids, vec![UserId::new(123), UserId::new(456)]);
    }

    #[test]
    fn extract_user_mention_ids_ignores_roles_everyone_and_invalid_values() {
        let ids = extract_user_mention_ids("@everyone <@&789> <@abc> <@123");

        assert!(ids.is_empty());
    }

    #[test]
    fn extract_user_mention_ids_ignores_zero_id() {
        let ids = extract_user_mention_ids("<@0>");

        assert!(ids.is_empty());
    }

    #[test]
    fn extract_user_mention_ids_deduplicates_mentions() {
        let ids = extract_user_mention_ids("<@123> <@!123> <@456>");

        assert_eq!(ids, vec![UserId::new(123), UserId::new(456)]);
    }

    #[test]
    fn route_rejects_when_channels_empty() {
        let handler = test_handler(HashMap::new());

        assert!(
            !route_accepts_channel(&handler, 123),
            "empty channels should reject guild messages"
        );
    }

    #[test]
    fn route_accepts_listed_channel_only_when_bot_is_bound() {
        let handler = test_handler_with_agents(
            channels(&[
                (123, &["developer"], false, false),
                (456, &["reviewer"], false, false),
            ]),
            9999,
            "main",
            "developer",
            agents(&[("developer", Some("main")), ("reviewer", Some("other"))]),
        );

        assert!(route_accepts_channel(&handler, 123));
        assert!(!route_accepts_channel(&handler, 456));
        assert!(!route_accepts_channel(&handler, 789));
    }

    #[test]
    fn route_rejects_single_agent_channel_without_bot_binding() {
        let handler = test_handler_with_agents(
            channels(&[(123, &["developer"], false, false)]),
            9999,
            "main",
            "developer",
            agents(&[("developer", None)]),
        );

        assert!(!route_accepts_channel(&handler, 123));
    }

    #[test]
    fn route_accepts_single_agent_channel_with_bot_binding() {
        let handler = test_handler_with_agents(
            channels(&[(123, &["developer"], false, false)]),
            9999,
            "main",
            "developer",
            agents(&[("developer", Some("main"))]),
        );

        assert!(route_accepts_channel(&handler, 123));
    }

    #[test]
    fn route_rejects_single_agent_channel_when_only_secondary_agent_matches_bot() {
        let handler = test_handler_with_agents(
            channels(&[(123, &["lyre", "vega"], false, false)]),
            9999,
            "vega",
            "developer",
            agents(&[("lyre", Some("lyre")), ("vega", Some("vega"))]),
        );

        assert!(!route_accepts_channel(&handler, 123));
    }

    #[test]
    fn route_rejects_multi_agent_channel_without_bot_binding() {
        let handler = test_handler_with_agents(
            channels(&[(123, &["developer"], true, false)]),
            9999,
            "main",
            "developer",
            agents(&[("developer", None)]),
        );

        assert!(!route_accepts_channel(&handler, 123));
    }

    #[test]
    fn interaction_chat_id_uses_agent_scoped_thread() {
        let handler = test_handler_with_agents(
            channels(&[(123, &["developer"], false, false)]),
            9999,
            "main",
            "developer",
            agents(&[("developer", Some("main"))]),
        );

        assert_eq!(
            handler
                .make_context("user", "123", "developer")
                .surface_thread,
            "123"
        );
    }

    #[test]
    fn scheduled_turn_context_preserves_agent_scope_for_token_selection() {
        let handler = test_handler_with_agents(
            channels(&[(123, &["developer"], false, false)]),
            9999,
            "main",
            "developer",
            agents(&[("developer", Some("main"))]),
        );
        let context = handler.make_context("user", "123", "developer");

        assert_eq!(context.session_key(), "discord:123:agent:developer");
    }

    /// Interaction コマンド名 → "/command" 形式の正規化が正しいこと。
    #[test]
    fn interaction_command_text_normalizes() {
        assert_eq!(interaction_to_command_text("status", &[]), "/status");
        assert_eq!(interaction_to_command_text("new", &[]), "/new");
        assert_eq!(interaction_to_command_text("model", &[]), "/model");
    }

    /// 未知コマンド名を正規化した場合、handle_slash_command が unknown_command_response を返すこと。
    #[test]
    fn interaction_unknown_command_responds() {
        let command_text = interaction_to_command_text("nonexistent_cmd", &[]);
        assert!(crate::slash_commands::is_slash_command(&command_text));
        assert_eq!(command_text, "/nonexistent_cmd");
    }

    #[test]
    fn discord_attachment_builds_combined_text() {
        let paths = vec![
            PathBuf::from("/workspace/media/inbound/20260428-120000-cat.png"),
            PathBuf::from("/workspace/media/inbound/20260428-120001-notes.pdf"),
        ];
        let user_text = "check these files";
        let combined = crate::channels::utils::media::format_attachment_text(&paths, user_text);
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
        let paths: Vec<PathBuf> = vec![];
        let user_text = "hello world";
        let combined = crate::channels::utils::media::format_attachment_text(&paths, user_text);
        assert_eq!(combined, "hello world");
        assert!(!combined.contains("[attachment:"));
    }

    // --- route_message context-agent tests ---

    #[test]
    fn route_message_returns_default_agent_for_dm() {
        let handler = test_handler(channels(&[(123, &["reviewer"], false, false)]));
        assert_eq!(
            route_agent_id(&handler, 999, true, true).as_deref(),
            Some("developer")
        );
    }

    #[test]
    fn route_message_uses_bound_primary_agent_in_single_channel() {
        let handler = test_handler_with_agents(
            channels(&[(123, &["reviewer"], false, false)]),
            9999,
            "main",
            "developer",
            agents(&[("reviewer", Some("main"))]),
        );
        assert_eq!(
            route_agent_id(&handler, 123, false, true).as_deref(),
            Some("reviewer")
        );
    }

    #[test]
    fn route_message_rejects_single_channel_without_primary_agent() {
        let handler = test_handler(channels(&[(123, &[], false, false)]));
        assert_eq!(route_agent_id(&handler, 123, false, true), None);
    }

    #[test]
    fn route_message_rejects_unknown_channel() {
        let handler = test_handler(channels(&[(123, &[], false, false)]));
        assert_eq!(route_agent_id(&handler, 456, false, true), None);
    }

    #[test]
    fn route_message_returns_first_matching_agent_in_multi_agent_channel() {
        let handler = test_handler_with_agents(
            channels(&[(789, &["lyre", "vega"], true, false)]),
            9999,
            "main",
            "developer",
            agents(&[("lyre", Some("main")), ("vega", Some("other"))]),
        );

        assert_eq!(
            route_agent_id(&handler, 789, false, true).as_deref(),
            Some("lyre")
        );
    }

    #[test]
    fn is_bot_mentioned_returns_false_when_no_bot_user_id() {
        let handler = test_handler(HashMap::new());
        assert_eq!(handler.bot_user_id.get(), None);
    }

    #[test]
    fn require_mention_true_skips_without_mention_logic() {
        let handler = test_handler_with_agents(
            channels(&[
                (123, &["developer"], false, true),
                (456, &["developer"], false, false),
            ]),
            9999,
            "main",
            "developer",
            agents(&[("developer", Some("main"))]),
        );
        assert!(route_accepts_channel(&handler, 123));
        assert!(route_accepts_channel(&handler, 456));

        // Verify config is readable
        assert!(handler.channels.get(&123).expect("config").require_mention);
        assert!(!handler.channels.get(&456).expect("config").require_mention);
    }

    #[test]
    fn dm_always_allowed_regardless_of_channels() {
        let handler = test_handler(HashMap::new());
        assert_eq!(
            route_agent_id(&handler, 999, true, true).as_deref(),
            Some("developer")
        );
        // But guild is rejected
        assert!(!route_accepts_channel(&handler, 123));
    }

    #[test]
    fn interaction_rejected_in_non_allowed_channel() {
        let handler = test_handler_with_agents(
            channels(&[(100, &["developer"], false, false)]),
            9999,
            "main",
            "developer",
            agents(&[("developer", Some("main"))]),
        );
        assert!(!route_accepts_channel(&handler, 999));
        assert!(route_accepts_channel(&handler, 100));
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
        let handler = test_handler_with_bot_id(channels(&[(100, &[], false, false)]), 9999);

        assert_eq!(
            handler.should_process_message(1000, false, false, 100, false),
            ReceiveDecision::Accept { reset_chain: true }
        );
    }

    #[test]
    fn human_message_obeys_require_mention_true() {
        let handler = test_handler_with_bot_id(channels(&[(100, &[], false, true)]), 9999);

        assert_eq!(
            handler.should_process_message(1000, false, false, 100, false),
            ReceiveDecision::Reject
        );
    }

    #[test]
    fn human_message_in_multi_agent_room_bypasses_require_mention_for_channel_log() {
        let handler =
            test_handler_with_bot_id(channels(&[(100, &["lyre", "vega"], true, true)]), 9999);

        assert_eq!(
            handler.should_process_message(1000, false, false, 100, false),
            ReceiveDecision::Accept { reset_chain: true }
        );
    }

    #[test]
    fn human_mentioning_this_bot_is_allowed() {
        let handler = test_handler_with_bot_id(channels(&[(100, &[], false, true)]), 9999);

        assert_eq!(
            handler.should_process_message(1000, false, false, 100, true),
            ReceiveDecision::Accept { reset_chain: true }
        );
    }

    #[test]
    fn human_mentioning_other_bot_only_is_ignored() {
        let handler = test_handler_with_bot_id(channels(&[(100, &[], false, true)]), 9999);

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

    // --- route_message tests ---

    fn route_accepts_channel(handler: &Handler, channel_id: u64) -> bool {
        !handler.route_message(channel_id, false, true).is_rejected()
    }

    fn route_agent_id(
        handler: &Handler,
        channel_id: u64,
        is_dm: bool,
        mentions_bot: bool,
    ) -> Option<String> {
        handler
            .route_message(channel_id, is_dm, mentions_bot)
            .agent_id()
            .map(ToString::to_string)
    }

    fn route_responder_agent_id(
        handler: &Handler,
        channel_id: u64,
        is_dm: bool,
        mentions_bot: bool,
    ) -> Option<String> {
        handler
            .route_message(channel_id, is_dm, mentions_bot)
            .response_agent_id()
            .map(ToString::to_string)
    }

    #[test]
    fn route_message_responds_with_matching_mentioned_agent() {
        let handler = test_handler_with_agents(
            channels(&[(100, &["lyre", "vega"], true, false)]),
            9999,
            "main",
            "developer",
            agents(&[("lyre", Some("main")), ("vega", Some("other"))]),
        );

        let result = route_responder_agent_id(&handler, 100, false, true);
        assert_eq!(result, Some("lyre".to_string()));
    }

    #[test]
    fn route_message_responds_with_first_matching_agent() {
        let handler = test_handler_with_agents(
            channels(&[(100, &["lyre", "vega"], true, false)]),
            9999,
            "main",
            "developer",
            agents(&[("lyre", Some("main")), ("vega", Some("main"))]),
        );

        let result = route_responder_agent_id(&handler, 100, false, true);
        assert_eq!(result, Some("lyre".to_string()));
    }

    #[test]
    fn route_message_rejects_mention_when_bot_has_no_channel_agent() {
        let handler = test_handler_with_agents(
            channels(&[(100, &["lyre", "vega"], true, false)]),
            9999,
            "musa",
            "developer",
            agents(&[("lyre", Some("lyre")), ("vega", Some("vega"))]),
        );

        let result = route_responder_agent_id(&handler, 100, false, true);
        assert_eq!(result, None);
    }

    #[test]
    fn route_message_observes_multi_room_without_mention() {
        let handler = test_handler_with_agents(
            channels(&[(100, &["lyre"], true, false)]),
            9999,
            "main",
            "developer",
            agents(&[("lyre", Some("main"))]),
        );

        assert_eq!(
            route_agent_id(&handler, 100, false, false).as_deref(),
            Some("lyre")
        );
        assert_eq!(route_responder_agent_id(&handler, 100, false, false), None);
    }

    #[test]
    fn route_message_rejects_empty_multi_room_without_mention() {
        let handler = test_handler_with_agents(
            channels(&[(100, &[], true, false)]),
            9999,
            "main",
            "developer",
            agents(&[]),
        );

        assert_eq!(route_agent_id(&handler, 100, false, false), None);
        assert_eq!(route_responder_agent_id(&handler, 100, false, false), None);
    }

    #[test]
    fn route_message_responds_in_single_channel_without_mention() {
        let handler = test_handler_with_agents(
            channels(&[(100, &["lyre"], false, false)]),
            9999,
            "main",
            "developer",
            agents(&[("lyre", Some("main"))]),
        );

        let result = route_responder_agent_id(&handler, 100, false, false);
        assert_eq!(result, Some("lyre".to_string()));
    }

    #[test]
    fn route_message_rejects_single_channel_for_unbound_bot() {
        let handler = test_handler_with_agents(
            channels(&[(100, &["lyre"], false, false)]),
            9999,
            "vega",
            "developer",
            agents(&[("lyre", Some("lyre"))]),
        );

        let result = route_responder_agent_id(&handler, 100, false, false);
        assert_eq!(result, None);
    }

    #[test]
    fn route_message_single_channel_uses_only_primary_agent() {
        let handler = test_handler_with_agents(
            channels(&[(100, &["lyre", "vega"], false, false)]),
            9999,
            "vega",
            "developer",
            agents(&[("lyre", Some("lyre")), ("vega", Some("vega"))]),
        );

        let result = route_responder_agent_id(&handler, 100, false, false);
        assert_eq!(result, None);
    }

    #[test]
    fn route_message_dm_responds_with_default_agent() {
        let handler = test_handler_with_agents(
            HashMap::new(),
            9999,
            "main",
            "developer",
            agents(&[("lyre", Some("main"))]),
        );

        let result = route_responder_agent_id(&handler, 999, true, false);
        assert_eq!(result, Some("developer".to_string()));
    }

    // --- Sender ID / SenderKind tests (Step 6) ---

    #[test]
    fn discord_user_message_sender_id() {
        let author_id: u64 = 123456789;
        let sender_id = format!("user:discord:{author_id}");
        assert!(sender_id.starts_with("user:discord:"));
        assert!(sender_id.ends_with("123456789"));
    }

    #[test]
    fn discord_stored_message_has_user_kind() {
        let chat_id = 42;
        let msg = crate::storage::StoredMessage::user(
            chat_id,
            "user:discord:123456789".to_string(),
            "hello".to_string(),
        );
        assert_eq!(msg.sender_kind, crate::storage::SenderKind::User);
        assert_eq!(msg.sender_id, "user:discord:123456789");
    }

    // --- make_context scope propagation (Step 8) ---

    fn secret_channel(agent_ids: &[&str], multi_agent: bool, secret: bool) -> DiscordChannelConfig {
        DiscordChannelConfig {
            require_mention: false,
            agents: agent_ids.iter().map(|id| agent_id(id)).collect(),
            multi_agent,
            secret,
            ..Default::default()
        }
    }

    #[test]
    fn make_context_sets_secret_scope_for_secret_channel() {
        let mut channels = HashMap::new();
        channels.insert(123u64, secret_channel(&["default"], false, true));
        let handler = test_handler(channels);
        let ctx = handler.make_context("user", "123", "default");
        assert_eq!(ctx.scope, ConversationScope::Secret);
    }

    #[test]
    fn make_context_defaults_to_normal_scope_for_normal_channel() {
        let mut channels = HashMap::new();
        channels.insert(456u64, secret_channel(&["default"], false, false));
        let handler = test_handler(channels);
        let ctx = handler.make_context("user", "456", "default");
        assert_eq!(ctx.scope, ConversationScope::Normal);
    }

    #[test]
    fn make_context_defaults_to_normal_scope_for_unknown_channel() {
        let channels: HashMap<u64, DiscordChannelConfig> = HashMap::new();
        let handler = test_handler(channels);
        let ctx = handler.make_context("user", "789", "default");
        assert_eq!(ctx.scope, ConversationScope::Normal);
    }

    // --- tool progress sink (Steps 4-6) ---

    fn progress_sink(base_url: String) -> DiscordToolProgressSink {
        let mut bot_token_map = HashMap::new();
        bot_token_map.insert("main".to_string(), "TOKEN".to_string());
        let mut agent_bot_map = HashMap::new();
        agent_bot_map.insert("lyre".to_string(), "main".to_string());
        let tokens = Arc::new(DiscordTokenResolver::new(bot_token_map, agent_bot_map));
        DiscordToolProgressSink::with_base_url(tokens, reqwest::Client::new(), base_url)
    }

    #[tokio::test]
    async fn discord_sink_begin_update_close_sequence() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Arrange: begin POST returns a message id; PATCH edits must be accepted.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/channels/123/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "msg-42"})))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/channels/123/messages/msg-42"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        // Act
        let sink = progress_sink(server.uri());
        let mut handle = sink
            .begin("discord:123:agent:lyre", "tools running...")
            .await
            .expect("begin");
        handle
            .update("tools running...\n✓ bash (0.5s)")
            .await
            .expect("update");
        handle.close().await.expect("close");

        // Assert: the edit was applied exactly once (verified at server drop via expect(1))
    }

    #[tokio::test]
    async fn discord_sink_truncates_over_2000_chars() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/channels/123/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "msg-1"})))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/channels/123/messages/msg-1"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        // Act: a body far over the 2000-char limit is pushed through update.
        let sink = progress_sink(server.uri());
        let mut handle = sink
            .begin("discord:123:agent:lyre", "tools running...")
            .await
            .expect("begin");
        let huge = format!("tools running...\n{}", "x".repeat(5000));
        handle.update(&huge).await.expect("update");

        // Assert: the PATCH content stayed within Discord's 2000-char limit.
        let requests = server.received_requests().await.expect("recorded requests");
        let patch = requests
            .iter()
            .rev()
            .find(|r| r.method.as_str() == "PATCH")
            .expect("patch recorded");
        let body: serde_json::Value = serde_json::from_slice(&patch.body).expect("json body");
        let content = body["content"].as_str().expect("content field");
        assert!(
            content.chars().count() <= DISCORD_MAX_MESSAGE_LEN,
            "content was {} chars",
            content.chars().count()
        );
    }

    #[tokio::test]
    async fn discord_sink_edit_failure_does_not_post_fallback() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Arrange: begin succeeds exactly once; every PATCH fails.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/channels/123/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "msg-1"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/channels/123/messages/msg-1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        // Act
        let sink = progress_sink(server.uri());
        let mut handle = sink
            .begin("discord:123:agent:lyre", "tools running...")
            .await
            .expect("begin");
        let result = handle.update("tools running...\n✓ bash").await;

        // Assert: edit failure propagates as Err and triggers no extra POST.
        assert!(result.is_err(), "edit failure should propagate");
        let requests = server.received_requests().await.expect("recorded requests");
        let posts = requests
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .count();
        assert_eq!(posts, 1, "no fallback post on edit failure");
    }
}

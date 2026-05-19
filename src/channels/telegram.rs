//! Telegram チャネルアダプター。
//!
//! teloxide 0.17 を用いて Telegram Bot API (long polling) からメッセージを受信し、
//! EgoPulse agent runtime で処理した結果を Telegram に返信する。
//!
//! Multi-Agent ルーティングは Discord と同じパターンに従う:
//! - `bots` マップが複数の Telegram Bot を定義
//! - `channels` マップがグループ/スーパーグループごとにエージェントを指定
//! - Single-Agent チャネルはバインドされたエージェントが応答
//! - Multi-Agent ルームは @mention された Bot のエージェントが応答

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{FileId, InputFile, MessageEntityKind};
use tracing::{debug, error, info, warn};

use crate::agent_loop::SurfaceContext;
use crate::channels::adapter::ChannelAdapter;
use crate::channels::adapter::ConversationKind;
use crate::channels::utils::text::split_text;
use crate::config::TelegramChatConfig;
use crate::runtime::AppState;
use crate::slash_commands::{self, SlashCommandOutcome, process_slash_command};

/// Telegram メッセージ長制限 (文字数)。
const TELEGRAM_MAX_MESSAGE_LEN: usize = 4096;

/// タイピングインジケーターの送信間隔。
#[allow(dead_code)]
const TYPING_INTERVAL_SECS: u64 = 4;

/// Bot-to-bot 連鎖の最大深さ（チャット単位）。
const BOT_CHAIN_MAX_DEPTH: u32 = 5;

/// Bot-to-bot 連鎖状態の TTL（秒）。
#[allow(dead_code)]
const BOT_CHAIN_TTL_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// Bot-to-bot 連鎖ガード (Discord の BotChainState と同一ロジック)
// ---------------------------------------------------------------------------

/// 連鎖の現在の深さと最終更新時刻。
struct ChainEntry {
    depth: u32,
    last_updated: Instant,
}

/// Bot-to-bot 連鎖の深さをチャット単位で追跡し、制限を超えたメッセージを拒否する。
#[allow(dead_code)]
pub(crate) struct BotChainState {
    ttl: Duration,
    chains: Mutex<HashMap<i64, ChainEntry>>,
}

#[allow(dead_code)]
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
    pub(crate) fn check_and_increment(&self, chat_id: i64) -> bool {
        let mut chains = self.chains.lock().expect("bot chain state lock poisoned");
        let now = Instant::now();
        chains.retain(|_, e| now.duration_since(e.last_updated) < self.ttl);

        match chains.get_mut(&chat_id) {
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
                    chat_id,
                    ChainEntry {
                        depth: 1,
                        last_updated: now,
                    },
                );
                true
            }
        }
    }

    /// チャットの連鎖状態をリセットする（人間のメッセージ受信時に呼ぶ）。
    pub(crate) fn reset(&self, chat_id: i64) {
        let mut chains = self.chains.lock().expect("bot chain state lock poisoned");
        chains.remove(&chat_id);
    }
}

// ---------------------------------------------------------------------------
// ルーティング判定
// ---------------------------------------------------------------------------

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
    /// チャット外などの理由で拒否。
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

// ---------------------------------------------------------------------------
// TelegramHandler — Discord の Handler 構造に対応
// ---------------------------------------------------------------------------

/// Telegram メッセージルーティングとボットチェーンガード。
///
/// 各 Bot は独自の `TelegramHandler` インスタンスを持ち、
/// 共有の `BotChainState` を通じて連鎖深さを追跡する。
struct TelegramHandler {
    app_state: Arc<AppState>,
    bot_id: String,
    bot_username: String,
    default_agent: String,
    channels: HashMap<i64, TelegramChatConfig>,
    chain_state: Arc<BotChainState>,
}

impl TelegramHandler {
    /// 指定エージェントがこの bot にバインドされているかを返す。
    fn agent_uses_this_bot(&self, agent_id: &crate::config::AgentId) -> bool {
        let bot_id = crate::config::BotId::new(&self.bot_id);
        self.app_state
            .config
            .agents
            .get(agent_id)
            .is_some_and(|agent| agent.telegram_bot.as_ref() == Some(&bot_id))
    }

    /// チャンネル設定内で、この bot にバインドされた最初のエージェントを返す。
    fn first_agent_for_this_bot(&self, channel_config: &TelegramChatConfig) -> Option<String> {
        channel_config
            .agents
            .iter()
            .find(|agent_id| self.agent_uses_this_bot(agent_id))
            .map(ToString::to_string)
    }

    /// Single-agent チャネルの先頭エージェントがこの bot にバインドされていれば返す。
    fn primary_agent_for_this_bot(&self, channel_config: &TelegramChatConfig) -> Option<String> {
        let agent_id = channel_config.agents.first()?;
        self.agent_uses_this_bot(agent_id)
            .then(|| agent_id.to_string())
    }

    /// Single-agent チャネルのルーティングを解決する。
    fn resolve_single_agent_channel(&self, channel_config: &TelegramChatConfig) -> RouteDecision {
        match self.primary_agent_for_this_bot(channel_config) {
            Some(agent_id) => RouteDecision::Respond { agent_id },
            None => RouteDecision::Reject,
        }
    }

    /// Multi-agent room のルーティングを解決する。
    fn resolve_multi_agent_room(
        &self,
        channel_config: &TelegramChatConfig,
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
    fn route_message(&self, raw_chat_id: i64, is_dm: bool, mentions_bot: bool) -> RouteDecision {
        if is_dm {
            return RouteDecision::Respond {
                agent_id: self.default_agent.clone(),
            };
        }

        let Some(channel_config) = self.channels.get(&raw_chat_id) else {
            return RouteDecision::Reject;
        };

        if channel_config.multi_agent {
            self.resolve_multi_agent_room(channel_config, mentions_bot)
        } else {
            self.resolve_single_agent_channel(channel_config)
        }
    }

    /// メッセージを処理すべきかを判定する。
    fn should_process_message(
        &self,
        is_bot: bool,
        is_dm: bool,
        raw_chat_id: i64,
        mentions_bot: bool,
    ) -> ReceiveDecision {
        // Telegram Bot API は自身のメッセージを Dispatcher に渡さないため、
        // self-message チェックは不要 (Discord とは異なる)。

        if is_bot {
            if !mentions_bot {
                return ReceiveDecision::Reject;
            }
            if !self.chain_state.check_and_increment(raw_chat_id) {
                return ReceiveDecision::Reject;
            }
            return ReceiveDecision::Accept { reset_chain: false };
        }

        // 人間のメッセージ: require_mention 設定に従う
        if !is_dm {
            if let Some(config) = self.channels.get(&raw_chat_id) {
                if config.require_mention && !config.multi_agent && !mentions_bot {
                    return ReceiveDecision::Reject;
                }
            }
        }

        ReceiveDecision::Accept { reset_chain: true }
    }

    /// [`SurfaceContext`] を構築する。
    fn make_context(&self, user: &str, thread: &str, agent_id: &str) -> SurfaceContext {
        SurfaceContext::new(
            "telegram".to_string(),
            user.to_string(),
            thread.to_string(),
            "telegram".to_string(),
            agent_id.to_string(),
        )
    }

    /// テキスト内に @username メンションが含まれているかを判定する。
    fn is_bot_mentioned_in_text(
        &self,
        text: &str,
        entities: &[teloxide::types::MessageEntity],
    ) -> bool {
        let username = &self.bot_username;
        if username.is_empty() {
            return false;
        }

        // MessageEntityRef::parse converts UTF-16 offsets to UTF-8 byte offsets
        let refs = teloxide::types::MessageEntityRef::parse(text, entities);

        // 1) /command@bot_username 形式
        let is_own_command = refs
            .iter()
            .filter(|e| matches!(e.kind(), MessageEntityKind::BotCommand))
            .any(|e| {
                let cmd_text = e.text();
                if let Some(at_pos) = cmd_text.find('@') {
                    let mention = &cmd_text[at_pos + 1..];
                    mention.eq_ignore_ascii_case(username)
                } else {
                    false
                }
            });

        if is_own_command {
            return true;
        }

        // 2) @mention エンティティ
        refs.iter()
            .filter(|e| matches!(e.kind(), MessageEntityKind::Mention))
            .any(|e| {
                e.text()
                    .strip_prefix('@')
                    .is_some_and(|m| m.eq_ignore_ascii_case(username))
            })
    }

    /// Channel Log 用のチャット ID を解決し、人間のメッセージを保存する。
    async fn store_human_channel_log_message(
        &self,
        raw_chat_id: i64,
        sender_name: &str,
        msg_id: i32,
        text: &str,
    ) -> Option<i64> {
        match crate::storage::call_blocking(std::sync::Arc::clone(&self.app_state.db), {
            let db = std::sync::Arc::clone(&self.app_state.db);
            move |_| db.resolve_telegram_channel_log_chat_id(raw_chat_id)
        })
        .await
        {
            Ok(chat_id) => {
                let stored = crate::storage::StoredMessage {
                    id: format!("cl-tg-{raw_chat_id}-{msg_id}"),
                    chat_id,
                    sender_name: sender_name.to_string(),
                    content: text.to_string(),
                    is_from_bot: false,
                    timestamp: Utc::now().to_rfc3339(),
                    message_kind: crate::storage::MessageKind::Message,
                    sender_agent_id: None,
                    recipient_agent_id: None,
                };
                let db = std::sync::Arc::clone(&self.app_state.db);
                if let Err(e) = crate::storage::call_blocking(db, move |db| {
                    let conn = db.lock_conn()?;
                    conn.execute(
                        "INSERT OR REPLACE INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp, message_kind)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![
                            stored.id,
                            stored.chat_id,
                            stored.sender_name,
                            stored.content,
                            stored.is_from_bot as i32,
                            stored.timestamp,
                            stored.message_kind.to_string(),
                        ],
                    )?;
                    Ok::<_, crate::error::StorageError>(())
                })
                .await
                {
                    warn!(
                        chat_id = raw_chat_id,
                        error = %e,
                        "failed to store Telegram message in Channel Log"
                    );
                }
                Some(chat_id)
            }
            Err(e) => {
                warn!(error = %e, "failed to resolve Channel Log chat_id");
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TelegramAdapter — アウトバウンド送信のみ
// ---------------------------------------------------------------------------

/// Telegram チャネルアダプター。
///
/// アウトバウンドメッセージ送信用。Bot API 経由で Telegram にメッセージを送信する。
pub(crate) struct TelegramAdapter {
    /// デフォルトの Bot クライアント (単一ボットまたはフォールバック用)。
    default_bot: Bot,
    /// `bot_id → token` のマップ。マルチボット構成で使用。
    #[allow(dead_code)]
    bot_tokens: std::collections::HashMap<String, String>,
    /// `agent_id → bot_id` のマップ。
    #[allow(dead_code)]
    agent_bot_map: std::collections::HashMap<String, String>,
}

impl TelegramAdapter {
    /// Creates a Telegram adapter backed by the provided bot client.
    #[allow(dead_code)]
    pub(crate) fn new(bot: Bot) -> Self {
        Self {
            default_bot: bot,
            bot_tokens: std::collections::HashMap::new(),
            agent_bot_map: std::collections::HashMap::new(),
        }
    }

    /// Creates a multi-bot Telegram adapter with token routing.
    pub(crate) fn new_multi(
        default_bot: Bot,
        bot_tokens: std::collections::HashMap<String, String>,
        agent_bot_map: std::collections::HashMap<String, String>,
    ) -> Self {
        Self {
            default_bot,
            bot_tokens,
            agent_bot_map,
        }
    }

    /// Get the appropriate bot for sending to the given external_chat_id.
    ///
    /// Resolves agent_id from the `:agent:` segment, then looks up
    /// agent → bot_id → token to find the right Bot.
    /// Falls back to `default_bot` if no mapping exists.
    fn bot_for(&self, external_chat_id: &str) -> &Bot {
        // Currently teloxide::Bot is tied to a single token at construction time.
        // Multi-bot outbound routing requires per-token Bot instances.
        // For now, return default_bot as the token routing is not yet
        // implemented at the adapter level.
        //
        // TODO: Create Bot instances per-token when outbound routing is needed.
        let _ = external_chat_id;
        &self.default_bot
    }
}

#[async_trait]
impl ChannelAdapter for TelegramAdapter {
    fn name(&self) -> &str {
        "telegram"
    }

    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
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
                match self
                    .bot_for(external_chat_id)
                    .send_message(ChatId(chat_id), &chunk)
                    .await
                {
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

    async fn send_attachment(
        &self,
        external_chat_id: &str,
        text: Option<&str>,
        file_path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let chat_id = parse_telegram_chat_id(external_chat_id)?;

        let extension = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let is_image = matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp");

        let caption_text = caption.or(text).unwrap_or("");
        let caption_value = if caption_text.len() > 1024 {
            let mut end = 1024;
            while end > 0 && !caption_text.is_char_boundary(end) {
                end -= 1;
            }
            &caption_text[..end]
        } else {
            caption_text
        };

        if is_image {
            let input_file = InputFile::file(file_path);
            let mut req = self
                .bot_for(external_chat_id)
                .send_photo(ChatId(chat_id), input_file);
            if !caption_value.is_empty() {
                req = req.caption(caption_value);
            }
            req.await
                .map_err(|e| format!("Telegram send_photo failed: {e}"))?;
        } else {
            let input_file = InputFile::file(file_path);
            let mut req = self
                .bot_for(external_chat_id)
                .send_document(ChatId(chat_id), input_file);
            if !caption_value.is_empty() {
                req = req.caption(caption_value);
            }
            req.await
                .map_err(|e| format!("Telegram send_document failed: {e}"))?;
        }

        if let Some(t) = text {
            if caption.is_some() && !t.is_empty() {
                for chunk in split_text(t, TELEGRAM_MAX_MESSAGE_LEN) {
                    self.bot_for(external_chat_id)
                        .send_message(ChatId(chat_id), &chunk)
                        .await
                        .map_err(|e| format!("Telegram send_message failed: {e}"))?;
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ヘルパー関数
// ---------------------------------------------------------------------------

/// Telegram からファイルをダウンロードし、`workspace/media/inbound/` に保存する。
async fn download_and_save(
    bot: &Bot,
    file_id: FileId,
    filename: &str,
    workspace_dir: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let tg_file = bot.get_file(file_id).await?;

    let temp_path = std::env::temp_dir().join(format!(
        "egopulse-tg-{}",
        Utc::now().format("%Y%m%d%H%M%S%3f")
    ));
    {
        let mut dst = tokio::fs::File::create(&temp_path).await?;
        bot.download_file(&tg_file.path, &mut dst).await?;
    }
    let bytes = tokio::fs::read(&temp_path).await?;
    let _ = tokio::fs::remove_file(&temp_path).await;

    let saved = crate::channels::utils::media::save_inbound_file(workspace_dir, filename, &bytes)?;
    Ok(saved)
}

/// Telegram にメッセージを送信 (4096文字制限で自動分割)。
async fn send_telegram_response(bot: &Bot, chat_id: ChatId, text: &str) {
    if let Err(error) =
        crate::channels::utils::text::send_chunked(text, TELEGRAM_MAX_MESSAGE_LEN, |chunk| {
            let bot = bot.clone();
            let chunk = chunk.to_string();
            Box::pin(async move {
                match bot.send_message(chat_id, &chunk).await {
                    Ok(_) => {}
                    Err(teloxide::RequestError::RetryAfter(seconds)) => {
                        warn!(
                            retry_after = seconds.duration().as_secs(),
                            "Telegram: rate limited while sending message chunk"
                        );
                        tokio::time::sleep(seconds.duration()).await;
                        bot.send_message(chat_id, &chunk).await.map_err(|e| {
                            format!("Telegram: failed to send message chunk after retry: {e}")
                        })?;
                    }
                    Err(e) => {
                        return Err(format!("Telegram: failed to send message chunk: {e}"));
                    }
                }
                Ok(())
            })
        })
        .await
    {
        error!(
            chat_id = chat_id.0,
            error = %error,
            "Telegram: failed to send chunked response"
        );
    }
}

fn parse_telegram_chat_id(external_chat_id: &str) -> Result<i64, String> {
    let raw = external_chat_id
        .strip_prefix("telegram:")
        .unwrap_or(external_chat_id);
    // Strip `:agent:` suffix from session_key format (telegram:<chat_id>:agent:<agent_id>)
    let thread = raw.split(':').next().unwrap_or(raw);
    thread
        .parse::<i64>()
        .map_err(|_| format!("invalid Telegram external_chat_id: '{external_chat_id}'"))
}

// ---------------------------------------------------------------------------
// メッセージハンドラ (Dispatcher endpoint)
// ---------------------------------------------------------------------------

/// Telegram メッセージハンドラ。
async fn handle_message(
    bot: Bot,
    msg: teloxide::types::Message,
    handler: Arc<TelegramHandler>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let text = msg.text().map(str::to_string).unwrap_or_default();
    let raw_chat_id = msg.chat.id.0;

    // チャット種別判定
    let is_dm = matches!(&msg.chat.kind, teloxide::types::ChatKind::Private(_));
    let _chat_type = match &msg.chat.kind {
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

    // メンション判定
    let entities = msg.entities().unwrap_or_default();
    let mentions_bot = handler.is_bot_mentioned_in_text(&text, entities);

    // ルーティング
    let route = handler.route_message(raw_chat_id, is_dm, mentions_bot);
    if route.is_rejected() {
        return Ok(());
    }

    let Some(route_agent_id) = route.agent_id().map(ToString::to_string) else {
        return Ok(());
    };

    // 送信者名
    let sender_name = msg
        .from
        .as_ref()
        .map(|u| u.username.clone().unwrap_or_else(|| u.first_name.clone()))
        .unwrap_or_else(|| "unknown".to_string());

    let thread = raw_chat_id.to_string();
    let context = handler.make_context(&sender_name, &thread, &route_agent_id);

    // スラッシュコマンドインターセプト
    if msg.text().is_some()
        && msg.photo().is_none()
        && msg.document().is_none()
        && msg.voice().is_none()
    {
        if !mentions_bot && !is_dm && slash_commands::is_slash_command(&text) {
            debug!(
                chat_id = raw_chat_id,
                "Telegram: skipping non-mentioned slash command in group"
            );
            return Ok(());
        }

        let sender_id = msg.from.as_ref().map(|u| u.id.0.to_string());
        match process_slash_command(&handler.app_state, &context, &text, sender_id.as_deref()).await
        {
            SlashCommandOutcome::Respond(response) => {
                send_telegram_response(&bot, msg.chat.id, &response).await;
                return Ok(());
            }
            SlashCommandOutcome::Error(response) => {
                send_telegram_response(&bot, msg.chat.id, &response).await;
                return Ok(());
            }
            SlashCommandOutcome::NotHandled => {}
        }
    }

    // メッセージ受信可否判定
    let author_is_bot = msg.from.as_ref().is_some_and(|u| u.is_bot);
    let decision = handler.should_process_message(author_is_bot, is_dm, raw_chat_id, mentions_bot);
    match decision {
        ReceiveDecision::Accept { reset_chain: true } => {
            handler.chain_state.reset(raw_chat_id);
        }
        ReceiveDecision::Accept { reset_chain: false } => {}
        ReceiveDecision::Reject => return Ok(()),
    }

    // Multi-Agent Room で Channel Log に保存
    let is_multi_agent = handler
        .channels
        .get(&raw_chat_id)
        .is_some_and(|c| c.multi_agent);

    let channel_log_chat_id = if is_multi_agent && !is_dm {
        handler
            .store_human_channel_log_message(raw_chat_id, &sender_name, msg.id.0, &text)
            .await
    } else {
        None
    };

    // ObserveOnly: Channel Log に保存済み、応答はしない
    let Some(agent_id) = route.response_agent_id().map(ToString::to_string) else {
        return Ok(());
    };

    // 添付ファイル処理
    let mut attachment_paths: Vec<PathBuf> = Vec::new();
    let workspace_dir = match handler.app_state.config.workspace_dir() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "Telegram: failed to resolve workspace_dir");
            return Ok(());
        }
    };

    if let Some(photos) = msg.photo() {
        if let Some(largest) = photos.last() {
            match download_and_save(&bot, largest.file.id.clone(), "photo.jpg", &workspace_dir)
                .await
            {
                Ok(path) => attachment_paths.push(path),
                Err(e) => tracing::warn!(error = %e, "Telegram: failed to download photo"),
            }
        }
    }

    if let Some(doc) = msg.document() {
        let filename = doc.file_name.as_deref().unwrap_or("document");
        match download_and_save(&bot, doc.file.id.clone(), filename, &workspace_dir).await {
            Ok(path) => attachment_paths.push(path),
            Err(e) => tracing::warn!(error = %e, "Telegram: failed to download document"),
        }
    }

    if let Some(voice) = msg.voice() {
        match download_and_save(&bot, voice.file.id.clone(), "voice.ogg", &workspace_dir).await {
            Ok(path) => attachment_paths.push(path),
            Err(e) => tracing::warn!(error = %e, "Telegram: failed to download voice"),
        }
    }

    let combined_text =
        crate::channels::utils::media::format_attachment_text(&attachment_paths, &text);

    if combined_text.is_empty() {
        return Ok(());
    }

    let mut context = handler.make_context(&sender_name, &thread, &agent_id);
    context.channel_log_chat_id = channel_log_chat_id;
    context.origin_id = uuid::Uuid::new_v4().to_string();

    info!(
        chat_id = raw_chat_id,
        agent = %agent_id,
        bot = %handler.bot_id,
        sender = %context.surface_user,
        text_length = text.len(),
        attachments = attachment_paths.len(),
        "Telegram message received"
    );

    // TurnScheduler 経由でターンを実行
    let scheduled = crate::agent_loop::ScheduledTurn {
        context: context.clone(),
        input: combined_text,
        origin_id: context.origin_id.clone(),
    };

    if let Some(turn) = handler.app_state.turn_scheduler.submit(scheduled) {
        let state = Arc::clone(&handler.app_state);
        tokio::spawn(async move {
            crate::runtime::execute_scheduled_turn(&state, turn).await;
        });
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Bot 起動
// ---------------------------------------------------------------------------

/// 指定した Bot ID で Telegram bot を起動する (multi-bot 用)。
#[allow(dead_code)]
#[allow(private_interfaces)]
pub(crate) async fn start_telegram_bot_for_bot(
    state: Arc<AppState>,
    token: &str,
    bot_id: &crate::config::BotId,
    bot_username: &str,
    default_agent: &crate::config::AgentId,
    channels: &HashMap<i64, TelegramChatConfig>,
    chain_state: Arc<BotChainState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bot = Bot::new(token);

    bot.delete_webhook().await.inspect_err(|e| {
        error!("Telegram: failed to delete webhook: {e}");
    })?;

    {
        use teloxide::types::BotCommand;

        let commands: Vec<BotCommand> = slash_commands::all_commands()
            .iter()
            .map(|c| BotCommand::new(c.name, c.description))
            .collect();
        if let Err(e) = bot.set_my_commands(commands).await {
            warn!("Telegram: failed to set bot commands: {e}");
        }
    }

    let telegram_handler = Arc::new(TelegramHandler {
        app_state: state,
        bot_id: bot_id.to_string(),
        bot_username: bot_username.to_string(),
        default_agent: default_agent.to_string(),
        channels: channels.clone(),
        chain_state,
    });

    info!(
        "Starting Telegram bot '{}' (agent {default_agent}) as @{bot_username}...",
        bot_id.as_str(),
    );

    let handler = Update::filter_message().endpoint(handle_message);

    let listener = teloxide::update_listeners::polling_default(bot.clone()).await;
    let listener_error_handler = teloxide::error_handlers::LoggingErrorHandler::with_custom_text(
        "An error from the Telegram update listener".to_string(),
    );

    let mut dispatcher = Dispatcher::builder(bot, handler)
        .default_handler(|_| async {})
        .dependencies(dptree::deps![telegram_handler])
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_handler(channels: HashMap<i64, TelegramChatConfig>) -> TelegramHandler {
        TelegramHandler {
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
            bot_username: "my_bot".to_string(),
            default_agent: "developer".to_string(),
            channels,
            chain_state: Arc::new(BotChainState::new()),
        }
    }

    fn test_handler_with_agents(
        channels: HashMap<i64, TelegramChatConfig>,
        bot_id: &str,
        bot_username: &str,
        default_agent: &str,
        agents: std::collections::HashMap<crate::config::AgentId, crate::config::AgentConfig>,
    ) -> TelegramHandler {
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
        TelegramHandler {
            app_state: Arc::new(state),
            bot_id: bot_id.to_string(),
            bot_username: bot_username.to_string(),
            default_agent: default_agent.to_string(),
            channels,
            chain_state: Arc::new(BotChainState::new()),
        }
    }

    fn agent_id(id: &str) -> crate::config::AgentId {
        crate::config::AgentId::new(id)
    }

    fn channel(agent_ids: &[&str], multi_agent: bool, require_mention: bool) -> TelegramChatConfig {
        TelegramChatConfig {
            require_mention,
            agents: agent_ids.iter().map(|id| agent_id(id)).collect(),
            multi_agent,
        }
    }

    fn channels(entries: &[(i64, &[&str], bool, bool)]) -> HashMap<i64, TelegramChatConfig> {
        entries
            .iter()
            .map(|(chat_id, agent_ids, multi_agent, require_mention)| {
                (*chat_id, channel(agent_ids, *multi_agent, *require_mention))
            })
            .collect()
    }

    fn agent_cfg(
        label: &str,
        telegram_bot: Option<&str>,
    ) -> (crate::config::AgentId, crate::config::AgentConfig) {
        let id = crate::config::AgentId::new(label);
        let cfg = crate::config::AgentConfig {
            label: label.to_string(),
            telegram_bot: telegram_bot.map(crate::config::BotId::new),
            ..Default::default()
        };
        (id, cfg)
    }

    fn agents(
        entries: &[(&str, Option<&str>)],
    ) -> std::collections::HashMap<crate::config::AgentId, crate::config::AgentConfig> {
        entries
            .iter()
            .map(|(label, telegram_bot)| agent_cfg(label, *telegram_bot))
            .collect()
    }

    fn route_accepts(handler: &TelegramHandler, chat_id: i64, mentions_bot: bool) -> bool {
        !handler
            .route_message(chat_id, false, mentions_bot)
            .is_rejected()
    }

    fn route_agent_id(
        handler: &TelegramHandler,
        chat_id: i64,
        is_dm: bool,
        mentions_bot: bool,
    ) -> Option<String> {
        handler
            .route_message(chat_id, is_dm, mentions_bot)
            .agent_id()
            .map(ToString::to_string)
    }

    fn route_responder_agent_id(
        handler: &TelegramHandler,
        chat_id: i64,
        is_dm: bool,
        mentions_bot: bool,
    ) -> Option<String> {
        handler
            .route_message(chat_id, is_dm, mentions_bot)
            .response_agent_id()
            .map(ToString::to_string)
    }

    // --- Adapter tests ---

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
        assert_eq!(
            parse_telegram_chat_id("telegram:-100123:agent:default").expect("agent-scoped chat id"),
            -100123
        );
        assert!(parse_telegram_chat_id("telegram:not-a-number").is_err());
    }

    /// Telegram BotCommand リストが all_commands() レジストリと整合することを確認。
    #[test]
    fn telegram_commands_match_registry() {
        use teloxide::types::BotCommand;

        let registry = crate::slash_commands::all_commands();
        let bot_commands: Vec<BotCommand> = registry
            .iter()
            .map(|c| BotCommand::new(c.name, c.description))
            .collect();

        assert_eq!(bot_commands.len(), registry.len());

        for (bot_cmd, reg) in bot_commands.iter().zip(registry.iter()) {
            assert_eq!(bot_cmd.command, reg.name);
            assert_eq!(bot_cmd.description, reg.description);
        }
    }

    // --- Media tests ---

    #[test]
    fn telegram_media_builds_combined_text() {
        let paths = vec![PathBuf::from("/workspace/media/inbound/photo.jpg")];
        let text = "check this";
        let combined = crate::channels::utils::media::format_attachment_text(&paths, text);
        assert_eq!(
            combined,
            "[attachment: /workspace/media/inbound/photo.jpg]\ncheck this"
        );
    }

    #[test]
    fn telegram_text_only_no_regression() {
        let paths: Vec<PathBuf> = vec![];
        let text = "hello world";
        let combined = crate::channels::utils::media::format_attachment_text(&paths, text);
        assert_eq!(combined, "hello world");
    }

    #[test]
    fn telegram_media_without_user_text() {
        let paths = vec![PathBuf::from("/workspace/media/inbound/voice.ogg")];
        let text = "";
        let combined = crate::channels::utils::media::format_attachment_text(&paths, text);
        assert_eq!(combined, "[attachment: /workspace/media/inbound/voice.ogg]");
    }

    #[test]
    fn telegram_media_multiple_attachments() {
        let paths = vec![
            PathBuf::from("/workspace/media/inbound/photo.jpg"),
            PathBuf::from("/workspace/media/inbound/doc.pdf"),
        ];
        let text = "see attached";
        let combined = crate::channels::utils::media::format_attachment_text(&paths, text);
        assert_eq!(
            combined,
            "[attachment: /workspace/media/inbound/photo.jpg]\n\
             [attachment: /workspace/media/inbound/doc.pdf]\n\
             see attached"
        );
    }

    #[test]
    fn telegram_empty_text_and_no_attachments_yields_empty() {
        let paths: Vec<PathBuf> = vec![];
        let text = "";
        let combined = crate::channels::utils::media::format_attachment_text(&paths, text);
        assert!(combined.is_empty());
    }

    // --- Routing tests (Step 6) ---

    #[test]
    fn route_accepts_dm_with_default_agent() {
        let handler = test_handler(HashMap::new());
        let result = handler.route_message(999, true, false);
        assert_eq!(
            result,
            RouteDecision::Respond {
                agent_id: "developer".to_string()
            }
        );
    }

    #[test]
    fn route_rejects_unauthorized_group() {
        let handler = test_handler(HashMap::new());
        assert!(handler.route_message(-100123, false, false).is_rejected());
    }

    #[test]
    fn route_responds_with_bound_agent_in_single_channel() {
        let handler = test_handler_with_agents(
            channels(&[(-100, &["reviewer"], false, false)]),
            "main",
            "my_bot",
            "developer",
            agents(&[("reviewer", Some("main"))]),
        );
        assert_eq!(
            route_responder_agent_id(&handler, -100, false, false),
            Some("reviewer".to_string())
        );
    }

    #[test]
    fn route_rejects_single_channel_for_unbound_bot() {
        let handler = test_handler_with_agents(
            channels(&[(-100, &["reviewer"], false, false)]),
            "other_bot",
            "other_bot",
            "developer",
            agents(&[("reviewer", Some("main"))]),
        );
        assert_eq!(route_responder_agent_id(&handler, -100, false, false), None);
    }

    #[test]
    fn route_responds_in_multi_agent_room_with_mention() {
        let handler = test_handler_with_agents(
            channels(&[(-100, &["lyre", "vega"], true, false)]),
            "main",
            "lyre_bot",
            "developer",
            agents(&[("lyre", Some("main")), ("vega", Some("other"))]),
        );
        assert_eq!(
            route_responder_agent_id(&handler, -100, false, true),
            Some("lyre".to_string())
        );
    }

    #[test]
    fn route_observes_without_mention_in_multi_room() {
        let handler = test_handler_with_agents(
            channels(&[(-100, &["lyre"], true, false)]),
            "main",
            "lyre_bot",
            "developer",
            agents(&[("lyre", Some("main"))]),
        );
        // agent_id is present (for channel log), but no response agent
        assert_eq!(
            route_agent_id(&handler, -100, false, false),
            Some("lyre".to_string())
        );
        assert_eq!(route_responder_agent_id(&handler, -100, false, false), None);
    }

    // --- Bot chain state tests ---

    #[test]
    fn should_process_message_human_resets_chain() {
        let handler = test_handler(channels(&[(-100, &["dev"], false, false)]));
        assert_eq!(
            handler.should_process_message(false, false, -100, true),
            ReceiveDecision::Accept { reset_chain: true }
        );
    }

    #[test]
    fn should_process_message_bot_within_depth() {
        let handler = test_handler(HashMap::new());
        assert_eq!(
            handler.should_process_message(true, false, -200, true),
            ReceiveDecision::Accept { reset_chain: false }
        );
    }

    #[test]
    fn should_process_message_bot_exceeds_depth() {
        let handler = test_handler(HashMap::new());
        for _ in 0..BOT_CHAIN_MAX_DEPTH {
            assert_eq!(
                handler.should_process_message(true, false, -200, true),
                ReceiveDecision::Accept { reset_chain: false }
            );
        }
        assert_eq!(
            handler.should_process_message(true, false, -200, true),
            ReceiveDecision::Reject
        );
    }

    #[test]
    fn should_process_message_bot_without_mention_rejected() {
        let handler = test_handler(HashMap::new());
        assert_eq!(
            handler.should_process_message(true, false, -200, false),
            ReceiveDecision::Reject
        );
    }

    // --- BotChainState tests ---

    #[test]
    fn bot_chain_starts_at_one() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        assert!(state.check_and_increment(-100));
    }

    #[test]
    fn bot_chain_allows_at_max_depth() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        for _ in 0..BOT_CHAIN_MAX_DEPTH {
            assert!(state.check_and_increment(-100));
        }
    }

    #[test]
    fn bot_chain_rejects_after_max_depth() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        for _ in 0..BOT_CHAIN_MAX_DEPTH {
            assert!(state.check_and_increment(-100));
        }
        assert!(!state.check_and_increment(-100));
    }

    #[test]
    fn bot_chain_resets_on_human_message() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        state.check_and_increment(-100);
        state.check_and_increment(-100);
        state.reset(-100);
        assert!(state.check_and_increment(-100));
    }

    #[test]
    fn bot_chain_scopes_by_chat_id() {
        let state = BotChainState::with_ttl(Duration::from_secs(BOT_CHAIN_TTL_SECS));
        for _ in 0..BOT_CHAIN_MAX_DEPTH {
            assert!(state.check_and_increment(-100));
        }
        assert!(
            state.check_and_increment(-200),
            "different chat_id is independent"
        );
        assert!(
            !state.check_and_increment(-100),
            "original chat still at max"
        );
    }

    #[test]
    fn bot_chain_ttl_expiry_restarts_at_one() {
        let state = BotChainState::with_ttl(Duration::from_millis(1));
        assert!(state.check_and_increment(-100));
        std::thread::sleep(Duration::from_millis(5));
        assert!(state.check_and_increment(-100));
    }

    // --- Mention detection ---

    #[test]
    fn mention_detects_at_username() {
        let handler = test_handler(HashMap::new());
        let text = "@my_bot hello";
        let entity =
            teloxide::types::MessageEntity::new(teloxide::types::MessageEntityKind::Mention, 0, 7);
        assert!(handler.is_bot_mentioned_in_text(text, &[entity]));
    }

    #[test]
    fn mention_detects_command_with_bot_suffix() {
        let handler = test_handler(HashMap::new());
        let text = "/status@my_bot";
        let entity = teloxide::types::MessageEntity::new(
            teloxide::types::MessageEntityKind::BotCommand,
            0,
            14,
        );
        assert!(handler.is_bot_mentioned_in_text(text, &[entity]));
    }

    #[test]
    fn mention_rejects_different_username() {
        let handler = test_handler(HashMap::new());
        let text = "@other_bot hello";
        let entity =
            teloxide::types::MessageEntity::new(teloxide::types::MessageEntityKind::Mention, 0, 10);
        assert!(!handler.is_bot_mentioned_in_text(text, &[entity]));
    }

    #[test]
    fn mention_rejects_empty_username() {
        let mut handler = test_handler(HashMap::new());
        handler.bot_username = String::new();
        let text = "@my_bot hello";
        let entity =
            teloxide::types::MessageEntity::new(teloxide::types::MessageEntityKind::Mention, 0, 7);
        assert!(!handler.is_bot_mentioned_in_text(text, &[entity]));
    }

    // --- Additional routing edge cases ---

    #[test]
    fn route_rejects_when_channels_empty() {
        let handler = test_handler(HashMap::new());
        assert!(!route_accepts(&handler, -100, true));
    }

    #[test]
    fn route_dm_always_uses_default_agent() {
        let handler = test_handler(channels(&[(-100, &["reviewer"], false, false)]));
        let result = handler.route_message(999, true, false);
        assert_eq!(
            result,
            RouteDecision::Respond {
                agent_id: "developer".to_string()
            }
        );
    }

    #[test]
    fn route_single_channel_responds_without_mention() {
        let handler = test_handler_with_agents(
            channels(&[(-100, &["lyre"], false, false)]),
            "main",
            "lyre_bot",
            "developer",
            agents(&[("lyre", Some("main"))]),
        );
        // Single-agent channel always responds (mention doesn't matter)
        assert_eq!(
            route_responder_agent_id(&handler, -100, false, false),
            Some("lyre".to_string())
        );
    }

    #[test]
    fn route_multi_agent_rejects_mention_for_wrong_bot() {
        let handler = test_handler_with_agents(
            channels(&[(-100, &["lyre", "vega"], true, false)]),
            "musa",
            "musa_bot",
            "developer",
            agents(&[("lyre", Some("lyre")), ("vega", Some("vega"))]),
        );
        assert_eq!(route_responder_agent_id(&handler, -100, false, true), None);
    }

    #[test]
    fn route_multi_agent_rejects_empty_room_without_mention() {
        let handler = test_handler_with_agents(
            channels(&[(-100, &[], true, false)]),
            "main",
            "my_bot",
            "developer",
            agents(&[]),
        );
        assert_eq!(route_agent_id(&handler, -100, false, false), None);
    }

    #[test]
    fn make_context_includes_agent_id_in_session_key() {
        let handler = test_handler(HashMap::new());
        let ctx = handler.make_context("user", "-100123", "alice");
        assert_eq!(ctx.session_key(), "telegram:-100123:agent:alice");
    }

    #[test]
    fn require_mention_true_single_agent_rejects_without_mention() {
        let handler = test_handler_with_agents(
            channels(&[(-100, &["developer"], false, true)]),
            "main",
            "my_bot",
            "developer",
            agents(&[("developer", Some("main"))]),
        );
        // Route accepts (bound bot)
        assert!(route_accepts(&handler, -100, false));
        // But receive rejects (no mention, single-agent, require_mention=true)
        assert_eq!(
            handler.should_process_message(false, false, -100, false),
            ReceiveDecision::Reject
        );
        // With mention, it's accepted
        assert_eq!(
            handler.should_process_message(false, false, -100, true),
            ReceiveDecision::Accept { reset_chain: true }
        );
    }

    #[test]
    fn require_mention_true_multi_agent_allows_without_mention_for_channel_log() {
        let handler = test_handler_with_agents(
            channels(&[(-100, &["lyre", "vega"], true, true)]),
            "main",
            "my_bot",
            "developer",
            agents(&[("lyre", Some("main")), ("vega", Some("other"))]),
        );
        // In multi-agent rooms, human messages are always accepted for channel log
        assert_eq!(
            handler.should_process_message(false, false, -100, false),
            ReceiveDecision::Accept { reset_chain: true }
        );
    }
}

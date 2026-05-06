//! スラッシュコマンドの検出・ディスパッチ。
//!
//! 公開 API を提供し、各チャネルから渡されたコマンドテキストを対応するハンドラに振り分ける。
//! `is_slash_command` で判定し、`handle_slash_command` で実行結果のメッセージを返す。

use std::path::Path;
use std::sync::Arc;

use crate::agent_loop::SurfaceContext;
use crate::agent_loop::compaction::force_compact;
use crate::agent_loop::session::{load_messages_for_turn, resolve_chat_id};
use crate::config::{AgentId, ChannelName, Config, ProviderId};
use crate::error::EgoPulseError;
use crate::runtime::AppState;
use crate::storage::call_blocking;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// 入力テキストがスラッシュコマンドかどうかを判定する。
///
/// 先頭のメンションをループで除去した後、残りのテキストが `/` で始まる場合に
/// `true` を返す。`//` (二重スラッシュ) や単独の `/` はコマンドとは見なさない。
pub(crate) fn is_slash_command(text: &str) -> bool {
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

/// [`process_slash_command`] の戻り値。
///
/// 各チャネルはこの結果に基づいてチャネル固有の方法で応答を送信する。
#[derive(Debug)]
pub(crate) enum SlashCommandOutcome {
    /// コマンドが正常に処理され、応答メッセージが生成された。
    Respond(String),
    /// chat_id の解決に失敗した等の内部的エラー。
    Error(String),
    /// テキストがスラッシュコマンドではなかった。
    NotHandled,
}

/// スラッシュコマンドの判定・chat_id 解決・実行を一括で行う。
///
/// 各チャネルのスラッシュコマンド処理ブロックを共通化するためのエントリポイント。
/// チャネル側は戻り値の [`SlashCommandOutcome`] に従って応答を送信すればよい。
///
/// # Arguments
///
/// * `state` — アプリケーション状態
/// * `context` — チャネルごとのサーフェスコンテキスト
/// * `text` — ユーザー入力テキスト
/// * `sender_id` — 送信者 ID（`/status` で表示）
pub(crate) async fn process_slash_command(
    state: &AppState,
    context: &SurfaceContext,
    text: &str,
    sender_id: Option<&str>,
) -> SlashCommandOutcome {
    if !is_slash_command(text) {
        return SlashCommandOutcome::NotHandled;
    }

    let command_name = extract_command_name(text);
    let needs_chat_id = matches!(command_name, Some("/new" | "/compact" | "/status"));

    if !needs_chat_id {
        let response = handle_slash_command(state, 0, context, text, sender_id)
            .await
            .unwrap_or_else(unknown_command_response);
        return SlashCommandOutcome::Respond(response);
    }

    match resolve_chat_id(state, context).await {
        Ok(chat_id) => {
            let response = handle_slash_command(state, chat_id, context, text, sender_id)
                .await
                .unwrap_or_else(unknown_command_response);
            SlashCommandOutcome::Respond(response)
        }
        Err(e) => {
            tracing::warn!("failed to resolve chat_id for slash command: {e}");
            SlashCommandOutcome::Error("An error occurred processing the command.".to_string())
        }
    }
}

fn extract_command_name(text: &str) -> Option<&str> {
    let normalized = normalized_slash_command(text)?;
    let bare = normalized
        .split_once('@')
        .map(|(cmd, _)| cmd)
        .unwrap_or(normalized);
    let lower = bare.split_whitespace().next()?;
    Some(lower)
}

/// スラッシュコマンドを実行し、結果メッセージを返す。
///
/// コマンドが未知または空の場合は `None` を返す。
pub(crate) async fn handle_slash_command(
    state: &AppState,
    chat_id: i64,
    context: &SurfaceContext,
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
        "/compact" => handle_compact(state, chat_id, context).await,
        "/status" => handle_status(state, chat_id, context, sender_id).await,
        "/skills" => Some(handle_skills(state)),
        "/restart" => Some(handle_restart()),
        "/providers" | "/provider" | "/models" | "/model" => {
            handle_llm_profile(state, context, normalized).await
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
pub(crate) fn unknown_command_response() -> String {
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

async fn handle_compact(
    state: &AppState,
    chat_id: i64,
    context: &SurfaceContext,
) -> Option<String> {
    let loaded = match load_messages_for_turn(state, chat_id).await {
        Ok(loaded) => loaded,
        Err(e) => return Some(format!("Failed to load session: {e}")),
    };
    if loaded.messages.is_empty() {
        return Some("Session is empty.".to_string());
    }

    let count = loaded.messages.len();
    let llm = match state.llm_for_context(context) {
        Ok(llm) => llm,
        Err(e) => return Some(format!("Failed to get LLM provider: {e}")),
    };

    match force_compact(state, context, chat_id, &loaded.messages, &llm).await {
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
    context: &SurfaceContext,
    sender_id: Option<&str>,
) -> Option<String> {
    let config = match state.try_current_config() {
        Ok(config) => config,
        Err(e) => return Some(format!("Failed to load config: {e}")),
    };
    let agent_id = crate::config::AgentId::new(&context.agent_id);
    let resolved = match config.resolve_llm_for_agent_channel(&agent_id, &context.channel) {
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
         Channel: {}\n\
         Provider: {}\n\
         Model: {}\n\
         {session_line}",
        context.channel, resolved.provider, resolved.model,
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

async fn handle_llm_profile(
    state: &AppState,
    context: &SurfaceContext,
    input: &str,
) -> Option<String> {
    match handle_command(state, context, input).await {
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
pub(crate) struct CommandDef {
    /// コマンド名（`/` なし）。
    pub name: &'static str,
    /// コマンドの短い説明。
    pub description: &'static str,
    /// 使用例（`/` で始まる）。
    pub usage: &'static str,
}

/// 登録済みコマンド一覧を返す。
pub(crate) const fn all_commands() -> &'static [CommandDef] {
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

// ---------------------------------------------------------------------------
// LLM profile commands (merged from llm_profile.rs)
// ---------------------------------------------------------------------------

const GLOBAL_SCOPE: &str = "global";

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProfileScope {
    Global,
    Channel(ChannelName),
    Agent {
        agent_id: AgentId,
        channel: ChannelName,
    },
}

impl std::fmt::Display for ProfileScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Global => f.write_str(GLOBAL_SCOPE),
            Self::Channel(channel) => write!(f, "{channel}"),
            Self::Agent { agent_id, .. } => write!(f, "agent:{agent_id}"),
        }
    }
}

async fn handle_command(
    state: &AppState,
    context: &SurfaceContext,
    input: &str,
) -> Result<Option<String>, EgoPulseError> {
    if !input.starts_with('/') {
        return Ok(None);
    }

    let parts = input.split_whitespace().collect::<Vec<_>>();
    let Some(raw) = parts.first().copied() else {
        return Ok(None);
    };
    // Strip @bot suffix (e.g. "/providers@mybot" → "/providers")
    let command = raw.split_once('@').map(|(c, _)| c).unwrap_or(raw);

    match command {
        "/providers" => {
            let config = state.try_current_config()?;
            let scope = command_scope(context);
            let effective = resolved_for_scope(&config, &scope)?;
            let lines = config
                .providers
                .iter()
                .map(|(id, provider)| {
                    let marker = if id.as_str() == effective.provider {
                        "*"
                    } else {
                        "-"
                    };
                    format!(
                        "{marker} {id} ({}) default_model={}",
                        provider.label, provider.default_model
                    )
                })
                .collect::<Vec<_>>();
            Ok(Some(lines.join("\n")))
        }
        "/provider" => handle_provider_command(state, context, &parts)
            .await
            .map(Some),
        "/models" => {
            let config = state.try_current_config()?;
            let scope = parse_scope(&parts[1..], command_scope(context), &config)?;
            let resolved = resolved_for_scope(&config, &scope)?;
            let provider = config
                .providers
                .get(resolved.provider.as_str())
                .ok_or_else(|| EgoPulseError::Internal("provider not found".to_string()))?;
            let lines = provider
                .models
                .keys()
                .map(|model| {
                    let marker = if model == &resolved.model { "*" } else { "-" };
                    format!("{marker} {model}")
                })
                .collect::<Vec<_>>();
            Ok(Some(lines.join("\n")))
        }
        "/model" => handle_model_command(state, context, &parts).await.map(Some),
        _ => Ok(None),
    }
}

async fn handle_provider_command(
    state: &AppState,
    context: &SurfaceContext,
    parts: &[&str],
) -> Result<String, EgoPulseError> {
    let config = state.try_current_config()?;
    let scope = parse_scope(&parts[1..], command_scope(context), &config)?;

    if parts.len() == 1 {
        let resolved = resolved_for_scope(&config, &scope)?;
        return Ok(format!(
            "scope={scope} provider={} model={}",
            resolved.provider, resolved.model
        ));
    }

    let value = first_non_scope_arg(&parts[1..]).unwrap_or_default();
    if value == "reset" {
        if scope == ProfileScope::Global {
            return Ok("global scope uses default_provider and cannot reset".to_string());
        }
        let path = config_path(state)?;
        let mut config = Config::load_allow_missing_api_key(Some(path))?;
        match &scope {
            ProfileScope::Global => unreachable!("global reset is returned above"),
            ProfileScope::Channel(channel_name) => {
                if let Some(channel) = config.channels.get_mut(channel_name.as_str()) {
                    channel.provider = None;
                    channel.model = None;
                }
            }
            ProfileScope::Agent { agent_id, .. } => {
                let agent = config.agents.get_mut(agent_id).ok_or_else(|| {
                    crate::error::ConfigError::AgentNotFound {
                        agent_id: agent_id.to_string(),
                    }
                })?;
                agent.provider = None;
                agent.model = None;
            }
        }
        config.save_config_with_secrets(path)?;
        let updated = Config::load_allow_missing_api_key(Some(path))?;
        let resolved = resolved_for_scope(&updated, &scope)?;
        return Ok(format!(
            "scope={scope} provider reset -> {}",
            resolved.provider
        ));
    }

    let provider_id = ProviderId::new(value);
    if !config.providers.contains_key(&provider_id) {
        return Ok(format!("unknown provider: {value}"));
    }
    let path = config_path(state)?;
    let mut config = Config::load_allow_missing_api_key(Some(path))?;
    match &scope {
        ProfileScope::Global => {
            config.default_provider = provider_id;
            config.default_model = None;
        }
        ProfileScope::Channel(channel_name) => {
            let channel = config.channels.entry(channel_name.clone()).or_default();
            channel.provider = Some(value.to_string());
            channel.model = None;
        }
        ProfileScope::Agent { agent_id, .. } => {
            let agent = config.agents.get_mut(agent_id).ok_or_else(|| {
                crate::error::ConfigError::AgentNotFound {
                    agent_id: agent_id.to_string(),
                }
            })?;
            agent.provider = Some(value.to_string());
            agent.model = None;
        }
    }
    config.save_config_with_secrets(path)?;
    let updated = Config::load_allow_missing_api_key(Some(path))?;
    let resolved = resolved_for_scope(&updated, &scope)?;
    Ok(format!(
        "scope={scope} provider={} model={}",
        resolved.provider, resolved.model
    ))
}

async fn handle_model_command(
    state: &AppState,
    context: &SurfaceContext,
    parts: &[&str],
) -> Result<String, EgoPulseError> {
    let config = state.try_current_config()?;
    let scope = parse_scope(&parts[1..], command_scope(context), &config)?;
    let resolved = resolved_for_scope(&config, &scope)?;

    if parts.len() == 1 {
        return Ok(format!(
            "scope={scope} provider={} model={}",
            resolved.provider, resolved.model
        ));
    }

    let value = first_non_scope_arg(&parts[1..]).unwrap_or_default();
    if value == "reset" {
        if scope == ProfileScope::Global {
            let mut config = Config::load_allow_missing_api_key(Some(config_path(state)?))?;
            config.default_model = None;
            config.save_config_with_secrets(config_path(state)?)?;
            return Ok(format!(
                "scope={scope} model reset -> {}",
                config.global_provider().default_model
            ));
        }
        let path = config_path(state)?;
        let mut config = Config::load_allow_missing_api_key(Some(path))?;
        match &scope {
            ProfileScope::Global => unreachable!("global reset is returned above"),
            ProfileScope::Channel(channel_name) => {
                if let Some(channel) = config.channels.get_mut(channel_name.as_str()) {
                    channel.model = None;
                }
            }
            ProfileScope::Agent { agent_id, .. } => {
                let agent = config.agents.get_mut(agent_id).ok_or_else(|| {
                    crate::error::ConfigError::AgentNotFound {
                        agent_id: agent_id.to_string(),
                    }
                })?;
                agent.model = None;
            }
        }
        config.save_config_with_secrets(path)?;
        let updated = Config::load_allow_missing_api_key(Some(path))?;
        let effective = resolved_for_scope(&updated, &scope)?;
        return Ok(format!("scope={scope} model reset -> {}", effective.model));
    }

    let path = config_path(state)?;
    let mut config = Config::load_allow_missing_api_key(Some(path))?;
    match &scope {
        ProfileScope::Global => {
            config.default_model = Some(value.to_string());
            let default_provider = config.default_provider.clone();
            if let Some(provider) = config.providers.get_mut(&default_provider)
                && !provider.models.contains_key(value)
            {
                provider
                    .models
                    .insert(value.to_string(), crate::config::ModelConfig::default());
            }
        }
        ProfileScope::Channel(channel_name) => {
            let channel = config.channels.entry(channel_name.clone()).or_default();
            channel.model = Some(value.to_string());
        }
        ProfileScope::Agent { agent_id, channel } => {
            let provider_name = config
                .resolve_llm_for_agent_channel(agent_id, channel.as_str())?
                .provider;
            let agent = config.agents.get_mut(agent_id).ok_or_else(|| {
                crate::error::ConfigError::AgentNotFound {
                    agent_id: agent_id.to_string(),
                }
            })?;
            agent.model = Some(value.to_string());
            if let Some(provider) = config.providers.get_mut(provider_name.as_str())
                && !provider.models.contains_key(value)
            {
                provider
                    .models
                    .insert(value.to_string(), crate::config::ModelConfig::default());
            }
        }
    }
    config.save_config_with_secrets(path)?;
    let updated = Config::load_allow_missing_api_key(Some(path))?;
    let effective = resolved_for_scope(&updated, &scope)?;
    Ok(format!(
        "scope={scope} provider={} model={}",
        effective.provider, effective.model
    ))
}

fn config_path(state: &AppState) -> Result<&Path, EgoPulseError> {
    state
        .config_path
        .as_deref()
        .ok_or_else(|| EgoPulseError::Internal("config path is unavailable".to_string()))
}

fn command_scope(context: &SurfaceContext) -> ProfileScope {
    ProfileScope::Agent {
        agent_id: AgentId::new(&context.agent_id),
        channel: ChannelName::new(&context.channel),
    }
}

fn parse_scope(
    args: &[&str],
    fallback: ProfileScope,
    config: &Config,
) -> Result<ProfileScope, EgoPulseError> {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if arg == "--scope" {
            let value = iter
                .next()
                .ok_or_else(|| EgoPulseError::Internal("missing scope value".to_string()))?;
            return normalize_scope(value, fallback, config);
        }
    }
    Ok(fallback)
}

fn first_non_scope_arg<'a>(args: &'a [&str]) -> Option<&'a str> {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if *arg == "--scope" {
            skip_next = true;
            continue;
        }
        return Some(*arg);
    }
    None
}

fn normalize_scope(
    scope: &str,
    fallback: ProfileScope,
    config: &Config,
) -> Result<ProfileScope, EgoPulseError> {
    let normalized = scope.trim().to_ascii_lowercase();
    if normalized == GLOBAL_SCOPE {
        return Ok(ProfileScope::Global);
    }
    if config.channels.contains_key(&ChannelName::new(&normalized)) {
        return Ok(ProfileScope::Channel(ChannelName::new(&normalized)));
    }
    if let Some(agent_id) = normalized.strip_prefix("agent:") {
        let channel = match fallback {
            ProfileScope::Agent { channel, .. } | ProfileScope::Channel(channel) => channel,
            ProfileScope::Global => config
                .channels
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| ChannelName::new("web")),
        };
        return Ok(ProfileScope::Agent {
            agent_id: AgentId::new(agent_id),
            channel,
        });
    }
    Err(EgoPulseError::Internal(format!("invalid scope: {scope}")))
}

fn resolved_for_scope(
    config: &Config,
    scope: &ProfileScope,
) -> Result<crate::config::ResolvedLlmConfig, EgoPulseError> {
    match scope {
        ProfileScope::Global => Ok(config.resolve_global_llm()),
        ProfileScope::Channel(channel) => Ok(config.resolve_llm_for_channel(channel.as_str())?),
        ProfileScope::Agent { agent_id, channel } => {
            Ok(config.resolve_llm_for_agent_channel(agent_id, channel.as_str())?)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::agent_loop::SurfaceContext;
    use crate::agent_loop::turn::{build_state, test_config};
    use crate::config::{AgentId, Config};
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::runtime::AppState;
    use crate::storage::{StoredMessage, call_blocking};

    use super::{
        SlashCommandOutcome, all_commands, handle_slash_command, is_slash_command,
        process_slash_command,
    };

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
        fn provider_name(&self) -> &str {
            "test"
        }

        fn model_name(&self) -> &str {
            "test-model"
        }

        async fn send_message(
            &self,
            _system: &str,
            _messages: Vec<Message>,
            _tools: Option<Vec<crate::llm::ToolDefinition>>,
        ) -> Result<MessagesResponse, LlmError> {
            Ok(MessagesResponse {
                content: "summary".to_string(),
                tool_calls: Vec::new(),
                usage: None,
            })
        }
    }

    fn build_test_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8").to_string());
        let state = build_state(config, Box::new(NoOpProvider));
        (state, dir)
    }

    async fn create_test_chat(state: &AppState, key: &str) -> i64 {
        let session_key = format!("cli:{key}");
        let key = key.to_string();
        call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id("cli", &session_key, Some(&key), "cli", "default")
        })
        .await
        .expect("chat_id")
    }

    fn test_context() -> SurfaceContext {
        SurfaceContext {
            channel: "cli".to_string(),
            surface_user: "local_user".to_string(),
            surface_thread: "test".to_string(),
            chat_type: "cli".to_string(),
            agent_id: "default".to_string(),
        }
    }

    // -- handle_slash_command tests -----------------------------------------------

    #[tokio::test]
    async fn handle_new_clears_session() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = create_test_chat(&state, "test-new").await;

        call_blocking(Arc::clone(&state.db), {
            move |db| {
                db.store_message_with_session(
                    &StoredMessage {
                        id: "msg-1".to_string(),
                        chat_id,
                        sender_name: "user".to_string(),
                        content: "hello".to_string(),
                        is_from_bot: false,
                        timestamp: "2024-01-01T00:00:00Z".to_string(),
                    },
                    r#"[{"role":"user","content":"hello"}]"#,
                    None,
                )
            }
        })
        .await
        .expect("store message");

        // Act
        let result = handle_slash_command(&state, chat_id, &test_context(), "/new", None).await;

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
        let chat_id = create_test_chat(&state, "test-compact").await;

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
        let result = handle_slash_command(&state, chat_id, &test_context(), "/compact", None).await;

        // Assert
        let response = result.expect("response");
        assert!(response.contains("Compacted"), "response: {response}");
        assert!(response.contains("2 messages"), "response: {response}");
    }

    #[tokio::test]
    async fn handle_status_shows_info() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = create_test_chat(&state, "test-status").await;

        // Act
        let result = handle_slash_command(
            &state,
            chat_id,
            &test_context(),
            "/status",
            Some("user-123"),
        )
        .await;

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
        let result = handle_slash_command(&state, chat_id, &test_context(), "/skills", None).await;

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
        let result =
            handle_slash_command(&state, chat_id, &test_context(), "/providers", None).await;

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
        let result = handle_slash_command(&state, 1, &test_context(), "/foo", None).await;

        // Assert
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn handle_restart_returns_message() {
        // Arrange
        let (state, _dir) = build_test_state();

        // Act
        let result = handle_slash_command(&state, 1, &test_context(), "/restart", None).await;

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
        let result = handle_slash_command(&state, chat_id, &test_context(), "/status", None).await;

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
        let result = handle_slash_command(&state, chat_id, &test_context(), "/compact", None).await;

        // Assert
        // load_messages_for_turn は chat_id を直接受け取るため、
        // チャット行が存在しなくても空セッションとして返す
        let response = result.expect("response");
        assert!(
            response.contains("Session is empty"),
            "response: {response}"
        );
    }

    // -- Step 2: Agent LLM Resolution tests -----------------------------------------

    #[tokio::test]
    async fn status_uses_agent_llm_resolution() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = create_test_chat(&state, "test-status-agent").await;

        // Act
        let result = handle_slash_command(&state, chat_id, &test_context(), "/status", None).await;

        // Assert
        let response = result.expect("response");
        assert!(
            response.contains("Provider: openai"),
            "expected agent-resolved provider, got: {response}"
        );
        assert!(
            response.contains("Model: gpt-4o-mini"),
            "expected agent-resolved model, got: {response}"
        );
    }

    #[tokio::test]
    async fn compact_uses_agent_llm_resolution() {
        // Arrange
        let (state, _dir) = build_test_state();
        let chat_id = create_test_chat(&state, "test-compact-agent").await;

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
        let result = handle_slash_command(&state, chat_id, &test_context(), "/compact", None).await;

        // Assert
        let response = result.expect("response");
        assert!(response.contains("Compacted"), "response: {response}");
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

    // -- Step 4: SurfaceContext agent_id propagation tests --------------------------

    #[tokio::test]
    async fn handle_status_receives_surface_context() {
        let (state, _dir) = build_test_state();
        let chat_id = create_test_chat(&state, "test-status-surface").await;

        let context = SurfaceContext {
            channel: "cli".to_string(),
            surface_user: "local_user".to_string(),
            surface_thread: "test".to_string(),
            chat_type: "cli".to_string(),
            agent_id: "default".to_string(),
        };

        let result = handle_slash_command(&state, chat_id, &context, "/status", None).await;
        let response = result.expect("response");
        assert!(
            response.contains("Provider: openai"),
            "expected provider resolved via context agent_id: {response}"
        );
        assert!(
            response.contains("Model: gpt-4o-mini"),
            "expected model resolved via context agent_id: {response}"
        );
    }

    #[tokio::test]
    async fn handle_compact_receives_surface_context() {
        let (state, _dir) = build_test_state();
        let chat_id = create_test_chat(&state, "test-compact-surface").await;

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

        let context = SurfaceContext {
            channel: "cli".to_string(),
            surface_user: "local_user".to_string(),
            surface_thread: "test".to_string(),
            chat_type: "cli".to_string(),
            agent_id: "default".to_string(),
        };

        let result = handle_slash_command(&state, chat_id, &context, "/compact", None).await;
        let response = result.expect("response");
        assert!(
            response.contains("Compacted"),
            "LLM should be resolved via context agent_id: {response}"
        );
    }

    #[test]
    fn slash_command_callers_pass_default_agent_context() {
        let (state, _dir) = build_test_state();
        assert_eq!(
            state.config.default_agent.to_string(),
            "default",
            "test config must use default agent"
        );

        let ctx = test_context();
        assert_eq!(
            ctx.agent_id, "default",
            "test_context must carry the default agent_id (matches channel caller pattern)"
        );
    }

    #[tokio::test]
    async fn llm_profile_resolved_for_scope_keeps_channel_scope() {
        let (state, _dir) = build_test_state();
        let ctx = test_context();

        let result = handle_slash_command(&state, 1, &ctx, "/providers", None).await;
        let response = result.expect("response");
        assert!(
            response.contains("openai"),
            "providers listing should contain openai: {response}"
        );

        let result = handle_slash_command(&state, 1, &ctx, "/model", None).await;
        let response = result.expect("response");
        assert!(
            response.contains("gpt-4o-mini"),
            "/model should show current model for the current agent: {response}"
        );
    }

    #[tokio::test]
    async fn provider_command_updates_current_agent_by_default() {
        let (state, _dir, path) = build_state_from_yaml(
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
  local:
    label: Local
    base_url: http://127.0.0.1:1234/v1
    default_model: qwen2.5
channels:
  discord:
    provider: openai
default_agent: alice
agents:
  alice:
    label: Alice
  bob:
    label: Bob"#,
        );
        let context = SurfaceContext {
            channel: "discord".to_string(),
            surface_user: "user".to_string(),
            surface_thread: "thread".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "bob".to_string(),
        };

        let result = handle_slash_command(&state, 1, &context, "/provider local", None).await;
        let response = result.expect("response");
        assert!(response.contains("scope=agent:bob"), "response: {response}");

        let updated = Config::load_allow_missing_api_key(Some(&path)).expect("reload");
        assert_eq!(
            updated
                .agents
                .get(&AgentId::new("bob"))
                .and_then(|agent| agent.provider.as_deref()),
            Some("local")
        );
        assert_eq!(
            updated
                .channels
                .get("discord")
                .and_then(|channel| channel.provider.as_deref()),
            Some("openai")
        );
    }

    #[tokio::test]
    async fn model_command_updates_current_agent_by_default() {
        let (state, _dir, path) = build_state_from_yaml(
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  discord:
    model: channel-model
default_agent: alice
agents:
  alice:
    label: Alice
  bob:
    label: Bob"#,
        );
        let context = SurfaceContext {
            channel: "discord".to_string(),
            surface_user: "user".to_string(),
            surface_thread: "thread".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "bob".to_string(),
        };

        let result = handle_slash_command(&state, 1, &context, "/model agent-model", None).await;
        let response = result.expect("response");
        assert!(response.contains("scope=agent:bob"), "response: {response}");

        let updated = Config::load_allow_missing_api_key(Some(&path)).expect("reload");
        assert_eq!(
            updated
                .agents
                .get(&AgentId::new("bob"))
                .and_then(|agent| agent.model.as_deref()),
            Some("agent-model")
        );
        assert_eq!(
            updated
                .channels
                .get("discord")
                .and_then(|channel| channel.model.as_deref()),
            Some("channel-model")
        );
    }

    fn build_state_from_yaml(yaml: &str) -> (AppState, tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("egopulse.config.yaml");
        std::fs::write(&path, yaml).expect("write config");
        let mut config = Config::load_allow_missing_api_key(Some(&path)).expect("load config");
        config.state_root = dir.path().to_string_lossy().into_owned();
        let mut state = build_state(config, Box::new(NoOpProvider));
        state.config_path = Some(path.clone());
        (state, dir, path)
    }

    // -- process_slash_command tests -------------------------------------------

    #[tokio::test]
    async fn process_slash_command_responds_to_known_command() {
        // Arrange
        let (state, _dir) = build_test_state();
        let context = test_context();

        // Act
        let outcome = process_slash_command(&state, &context, "/skills", None).await;

        // Assert
        match outcome {
            SlashCommandOutcome::Respond(response) => {
                assert!(
                    response.contains("No skills loaded."),
                    "expected skills response, got: {response}"
                );
            }
            other => panic!("expected Respond, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn process_slash_command_returns_not_handled_for_plain_text() {
        // Arrange
        let (state, _dir) = build_test_state();
        let context = test_context();

        // Act
        let outcome = process_slash_command(&state, &context, "hello world", None).await;

        // Assert
        assert!(
            matches!(outcome, SlashCommandOutcome::NotHandled),
            "plain text should not be handled as slash command"
        );
    }

    #[tokio::test]
    async fn process_slash_command_returns_respond_for_unknown_command() {
        // Arrange
        let (state, _dir) = build_test_state();
        let context = test_context();

        // Act
        let outcome = process_slash_command(&state, &context, "/foobar", None).await;

        // Assert
        match outcome {
            SlashCommandOutcome::Respond(response) => {
                assert_eq!(response, "Unknown command.");
            }
            other => panic!("expected Respond with unknown fallback, got {other:?}"),
        }
    }
}

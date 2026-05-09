use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use url::Url;

use super::ModelConfig;
use super::secret_ref::{
    ResolvedValue, StringOrRef, TELEGRAM_BOT_TOKEN_ENV_NAME, WEB_AUTH_TOKEN_ENV_NAME, dotenv_path,
    read_dotenv, resolve_string_or_ref,
};
use super::{
    AgentConfig, AgentId, BotId, ChannelConfig, ChannelName, Config, DiscordBotConfig,
    DiscordChannelConfig, ProviderConfig, ProviderId, SleepBatchConfig, TelegramChatConfig,
};
use crate::error::ConfigError;

fn deserialize_null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

/// Deserialization helper that accepts both old list format and new map format for models.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FileModels {
    List(Vec<String>),
    Map(HashMap<String, Option<ModelConfig>>),
}

#[derive(Debug, Deserialize, Default)]
struct FileProviderConfig {
    label: Option<String>,
    base_url: Option<String>,
    api_key: Option<StringOrRef>,
    default_model: Option<String>,
    models: Option<FileModels>,
}

#[derive(Debug, Deserialize, Default)]
struct FileChannelConfig {
    enabled: Option<bool>,
    port: Option<u16>,
    host: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    auth_token: Option<StringOrRef>,
    allowed_origins: Option<Vec<String>>,
    bot_token: Option<StringOrRef>,
    bot_username: Option<String>,
    chats: Option<HashMap<String, FileTelegramChatConfig>>,
    soul_path: Option<String>,
    bots: Option<HashMap<String, FileDiscordBotConfig>>,
    channels: Option<HashMap<String, FileDiscordChannelConfig>>,
}

#[derive(Debug, Deserialize, Default)]
struct FileDiscordBotConfig {
    token: Option<StringOrRef>,
}

#[derive(Debug, Deserialize, Default)]
struct FileDiscordChannelConfig {
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    require_mention: bool,
    #[serde(default)]
    agents: Option<Vec<String>>,
    #[serde(default)]
    multi_agent: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
struct FileTelegramChatConfig {
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    require_mention: bool,
}

#[derive(Debug, Deserialize, Default)]
struct FileAgentConfig {
    label: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    discord_bot: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FileSleepBatchConfig {
    provider: Option<String>,
    model: Option<String>,
    enabled: Option<bool>,
    schedule: Option<String>,
    timezone: Option<String>,
    agents: Option<Vec<String>>,
    retry: Option<FileRetryConfig>,
}

#[derive(Debug, Deserialize, Default)]
struct FileRetryConfig {
    max_attempts: Option<u32>,
    interval_minutes: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    default_provider: Option<String>,
    default_model: Option<String>,
    providers: Option<HashMap<String, FileProviderConfig>>,
    log_level: Option<String>,
    compaction_timeout_secs: Option<u64>,
    max_history_messages: Option<usize>,
    compact_keep_recent: Option<usize>,
    default_context_window_tokens: Option<usize>,
    compaction_threshold_ratio: Option<f64>,
    compaction_target_ratio: Option<f64>,
    channels: Option<HashMap<String, FileChannelConfig>>,
    default_agent: Option<String>,
    agents: Option<HashMap<String, FileAgentConfig>>,
    sleep_batch: Option<FileSleepBatchConfig>,
}

pub(super) fn build_config(
    config_path: Option<&Path>,
    allow_missing_api_key: bool,
) -> Result<Config, ConfigError> {
    let resolved_config_path = match config_path {
        Some(path) => Some(PathBuf::from(path)),
        None => Config::resolve_config_path()?,
    };

    let dotenv = load_dotenv(resolved_config_path.as_deref());

    let FileConfig {
        default_provider: file_default_provider,
        default_model: file_default_model,
        providers: file_providers,
        log_level: file_log_level,
        compaction_timeout_secs: file_compaction_timeout_secs,
        max_history_messages: file_max_history_messages,
        compact_keep_recent: file_compact_keep_recent,
        default_context_window_tokens: file_default_context_window_tokens,
        compaction_threshold_ratio: file_compaction_threshold_ratio,
        compaction_target_ratio: file_compaction_target_ratio,
        channels: file_channels,
        default_agent: file_default_agent,
        agents: file_agents,
        sleep_batch: file_sleep_batch,
    } = read_file_config(resolved_config_path.as_deref())?;

    let default_provider =
        normalize_string(file_default_provider).ok_or(ConfigError::MissingDefaultProvider)?;
    let default_provider = ProviderId::new(&default_provider);
    let providers = normalize_provider_map(
        file_providers.ok_or(ConfigError::MissingProviders)?,
        &dotenv,
        allow_missing_api_key,
    )?;
    if !providers.contains_key(&default_provider) {
        return Err(ConfigError::InvalidProviderReference {
            provider: default_provider.to_string(),
        });
    }

    let default_model = normalize_string(file_default_model);

    let state_root = super::resolve::default_state_root()?
        .to_string_lossy()
        .into_owned();

    let log_level = first_non_empty([env_var("LOG_LEVEL"), file_log_level])
        .unwrap_or_else(|| "info".to_string());

    let compaction_timeout_secs = file_compaction_timeout_secs
        .unwrap_or_else(super::resolve::default_compaction_timeout_secs);
    let max_history_messages =
        file_max_history_messages.unwrap_or_else(super::resolve::default_max_history_messages);
    let compact_keep_recent =
        file_compact_keep_recent.unwrap_or_else(super::resolve::default_compact_keep_recent);
    let default_context_window_tokens = file_default_context_window_tokens
        .unwrap_or(super::resolve::default_context_window_tokens());
    let compaction_threshold_ratio = file_compaction_threshold_ratio
        .unwrap_or(super::resolve::default_compaction_threshold_ratio());
    let compaction_target_ratio =
        file_compaction_target_ratio.unwrap_or(super::resolve::default_compaction_target_ratio());

    let agents = normalize_agents(file_agents.unwrap_or_default(), &dotenv)?;
    validate_agent_provider_references(&providers, &agents)?;

    let default_agent =
        normalize_string(file_default_agent).unwrap_or_else(|| "default".to_string());
    let default_agent = AgentId::new(&default_agent);
    validate_agent_id(&default_agent)?;
    if !agents.contains_key(&default_agent) {
        return Err(ConfigError::DefaultAgentNotFound {
            agent_id: default_agent.to_string(),
        });
    }

    let mut channels =
        normalize_channels(file_channels.unwrap_or_default(), &dotenv, &default_agent)?;
    apply_web_channel_env_overrides(&mut channels);
    apply_channel_bot_token_env_override(&mut channels, "telegram", TELEGRAM_BOT_TOKEN_ENV_NAME);

    validate_channel_provider_references(&providers, &channels)?;

    let sleep_batch = normalize_sleep_batch(file_sleep_batch, &providers, &agents, &default_agent)?;

    let config = Config {
        default_provider,
        default_model,
        providers,
        state_root,
        log_level,
        compaction_timeout_secs,
        max_history_messages,
        compact_keep_recent,
        default_context_window_tokens,
        compaction_threshold_ratio,
        compaction_target_ratio,
        channels,
        default_agent,
        agents,
        sleep_batch,
    };

    validate_compaction_config(&config)?;

    validate_discord_bot_references(&config)?;

    if config.web_enabled() && config.web_auth_token().is_none() {
        return Err(ConfigError::MissingWebAuthToken);
    }

    Ok(config)
}

fn load_dotenv(config_path: Option<&Path>) -> HashMap<String, String> {
    let Some(path) = config_path else {
        return HashMap::new();
    };
    let Some(parent) = path.parent() else {
        return HashMap::new();
    };
    read_dotenv(&dotenv_path(parent))
}

fn read_file_config(path: Option<&Path>) -> Result<FileConfig, ConfigError> {
    let Some(path) = path else {
        return Ok(FileConfig::default());
    };

    if !path.exists() {
        return Err(ConfigError::ConfigNotFound {
            path: PathBuf::from(path),
        });
    }

    let contents = fs::read_to_string(path).map_err(|source| ConfigError::ConfigReadFailed {
        path: PathBuf::from(path),
        source,
    })?;
    yaml_serde::from_str(&contents).map_err(|source| ConfigError::ConfigParseFailed {
        path: PathBuf::from(path),
        detail: source.to_string(),
    })
}

fn normalize_channels(
    channels: HashMap<String, FileChannelConfig>,
    dotenv: &HashMap<String, String>,
    default_agent: &AgentId,
) -> Result<HashMap<ChannelName, ChannelConfig>, ConfigError> {
    let mut normalized = HashMap::new();
    for (name, fc) in channels {
        let key = ChannelName::new(&name);
        if key.as_str().is_empty() {
            continue;
        }

        let resolved_auth = resolve_string_or_ref(fc.auth_token, dotenv)?;
        let resolved_bot = resolve_string_or_ref(fc.bot_token, dotenv)?;

        let file_auth_token = resolved_auth.as_ref().map(|rv| {
            if matches!(rv, ResolvedValue::Literal(_)) {
                yaml_serde::Value::String(rv.value().to_string())
            } else {
                rv.to_yaml_value()
            }
        });
        let file_bot_token = resolved_bot.as_ref().map(|rv| {
            if matches!(rv, ResolvedValue::Literal(_)) {
                yaml_serde::Value::String(rv.value().to_string())
            } else {
                rv.to_yaml_value()
            }
        });

        let chats = fc
            .chats
            .map(|map| {
                let mut result = HashMap::new();
                for (k, v) in map {
                    let chat_id: i64 = k
                        .parse::<i64>()
                        .map_err(|_| ConfigError::InvalidChatsKey { key: k.clone() })?;
                    result.insert(
                        chat_id,
                        TelegramChatConfig {
                            require_mention: v.require_mention,
                        },
                    );
                }
                Ok(result)
            })
            .transpose()?
            .filter(|m| !m.is_empty());

        let config = ChannelConfig {
            enabled: fc.enabled,
            port: fc.port,
            host: fc.host,
            provider: normalize_string(fc.provider),
            model: normalize_string(fc.model),
            auth_token: resolved_auth,
            file_auth_token,
            allowed_origins: fc.allowed_origins,
            bot_token: resolved_bot,
            file_bot_token,
            bot_username: fc.bot_username,
            chats,
            soul_path: fc.soul_path,
            discord_bots: None,
            discord_channels: None,
        };
        let was_discord = key.as_str() == "discord";
        normalized.insert(key, config);

        if was_discord {
            if let Some(file_bots) = fc.bots {
                let bots = normalize_discord_bots(file_bots, dotenv, default_agent)?;
                let discord_channel = normalized.get_mut("discord").expect("just inserted");
                discord_channel.discord_bots = Some(bots);
            }
            let shared_channels = normalize_discord_channels(fc.channels, default_agent)?;
            let discord_channel = normalized.get_mut("discord").expect("just inserted");
            discord_channel.discord_channels = shared_channels;
        }
    }

    if let Some(web) = normalized.get_mut("web") {
        if web.host.is_none() {
            web.host = Some(super::resolve::default_web_host().to_string());
        }
        if web.port.is_none() {
            web.port = Some(super::resolve::default_web_port());
        }
    }

    Ok(normalized)
}

fn validate_agent_id(id: &AgentId) -> Result<(), ConfigError> {
    let s = id.as_str();
    if s.is_empty() || s.trim().is_empty() {
        return Err(ConfigError::InvalidAgentId { id: id.to_string() });
    }
    if s.contains("..") || s.contains('/') || s.contains('\\') || s.contains(':') {
        return Err(ConfigError::InvalidAgentId { id: id.to_string() });
    }
    Ok(())
}

fn validate_bot_id(id: &BotId) -> Result<(), ConfigError> {
    let s = id.as_str();
    if s.is_empty() || s.trim().is_empty() {
        return Err(ConfigError::InvalidBotId { id: id.to_string() });
    }
    if s.contains("..") || s.contains('/') || s.contains('\\') || s.contains(':') {
        return Err(ConfigError::InvalidBotId { id: id.to_string() });
    }
    Ok(())
}

fn normalize_discord_bots(
    file_bots: HashMap<String, FileDiscordBotConfig>,
    dotenv: &HashMap<String, String>,
    _default_agent: &AgentId,
) -> Result<HashMap<BotId, DiscordBotConfig>, ConfigError> {
    let mut bots = HashMap::new();
    for (name, fb) in file_bots {
        let bot_id = BotId::new(&name);
        validate_bot_id(&bot_id)?;

        if bots.contains_key(&bot_id) {
            return Err(ConfigError::DuplicateBotId {
                bot_id: bot_id.to_string(),
                original_key: name,
            });
        }

        let resolved_token = resolve_string_or_ref(fb.token, dotenv)?;
        let file_token = resolved_token.as_ref().map(|rv| {
            if matches!(rv, ResolvedValue::Literal(_)) {
                yaml_serde::Value::String(rv.value().to_string())
            } else {
                rv.to_yaml_value()
            }
        });

        bots.insert(
            bot_id,
            DiscordBotConfig {
                token: resolved_token,
                file_token,
            },
        );
    }
    Ok(bots)
}

fn normalize_discord_channels(
    file_channels: Option<HashMap<String, FileDiscordChannelConfig>>,
    default_agent: &AgentId,
) -> Result<Option<HashMap<u64, DiscordChannelConfig>>, ConfigError> {
    let Some(map) = file_channels else {
        return Ok(None);
    };
    let mut result = HashMap::new();
    for (k, v) in map {
        let channel_id: u64 = k
            .parse::<u64>()
            .map_err(|_| ConfigError::InvalidChannelsKey { key: k })?;
        let mut agents: Vec<AgentId> = v
            .agents
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| normalize_string(Some(s)))
            .map(|s| AgentId::new(&s))
            .collect();
        if agents.is_empty() {
            agents.push(default_agent.clone());
        }
        let multi_agent = v.multi_agent.unwrap_or(false);
        result.insert(
            channel_id,
            DiscordChannelConfig {
                require_mention: v.require_mention,
                agents,
                multi_agent,
            },
        );
    }
    Ok(Some(result).filter(|m| !m.is_empty()))
}

fn normalize_agents(
    agents: HashMap<String, FileAgentConfig>,
    _dotenv: &HashMap<String, String>,
) -> Result<HashMap<AgentId, AgentConfig>, ConfigError> {
    let mut normalized = HashMap::new();
    for (name, fa) in agents {
        let key = AgentId::new(&name);
        validate_agent_id(&key)?;

        let config = AgentConfig {
            label: normalize_string(fa.label).unwrap_or_else(|| key.to_string()),
            provider: normalize_string(fa.provider),
            model: normalize_string(fa.model),
            discord_bot: normalize_string(fa.discord_bot).map(|s| BotId::new(&s)),
        };
        normalized.insert(key, config);
    }

    if normalized.is_empty() {
        normalized.insert(
            AgentId::new("default"),
            AgentConfig {
                label: "Default Agent".to_string(),
                ..Default::default()
            },
        );
    }

    Ok(normalized)
}

fn normalize_sleep_batch(
    file: Option<FileSleepBatchConfig>,
    providers: &HashMap<ProviderId, ProviderConfig>,
    agents: &HashMap<AgentId, AgentConfig>,
    default_agent: &AgentId,
) -> Result<SleepBatchConfig, ConfigError> {
    let Some(fb) = file else {
        return Ok(SleepBatchConfig::default());
    };

    let provider = normalize_string(fb.provider).map(|p| ProviderId::new(&p));
    if let Some(ref pid) = provider {
        if !providers.contains_key(pid) {
            return Err(ConfigError::InvalidProviderReference {
                provider: pid.to_string(),
            });
        }
    }

    let enabled = fb.enabled.unwrap_or(false);
    let schedule = normalize_string(fb.schedule);
    let timezone = normalize_string(fb.timezone);

    if enabled && schedule.is_none() {
        return Err(ConfigError::SleepBatchEnabledRequiresSchedule);
    }
    if enabled && timezone.is_none() {
        return Err(ConfigError::SleepBatchEnabledRequiresTimezone);
    }

    if let Some(ref sched) = schedule {
        validate_schedule(sched)?;
    }
    if let Some(ref tz) = timezone {
        validate_timezone(tz)?;
    }

    let mut resolved_agents = normalize_agent_list(fb.agents, agents)?;
    if let Some(ref mut list) = resolved_agents {
        sort_agent_list(list, default_agent);
    }

    let retry = fb.retry.unwrap_or_default();
    let retry_max_attempts = retry.max_attempts.unwrap_or(3);
    let retry_interval_minutes = retry.interval_minutes.unwrap_or(5);
    if retry_max_attempts == 0 {
        return Err(ConfigError::SleepBatchInvalidRetry {
            detail: "max_attempts must be at least 1".to_string(),
        });
    }

    Ok(SleepBatchConfig {
        provider,
        model: normalize_string(fb.model),
        enabled,
        schedule,
        timezone,
        agents: resolved_agents,
        retry_max_attempts,
        retry_interval_minutes,
    })
}

fn validate_schedule(schedule: &str) -> Result<(), ConfigError> {
    let parts: Vec<&str> = schedule.split(':').collect();
    if parts.len() != 2 {
        return Err(ConfigError::SleepBatchInvalidSchedule {
            schedule: schedule.to_string(),
        });
    }
    let hour: u32 = parts[0]
        .parse()
        .map_err(|_| ConfigError::SleepBatchInvalidSchedule {
            schedule: schedule.to_string(),
        })?;
    let minute: u32 = parts[1]
        .parse()
        .map_err(|_| ConfigError::SleepBatchInvalidSchedule {
            schedule: schedule.to_string(),
        })?;
    if hour > 23 || minute > 59 {
        return Err(ConfigError::SleepBatchInvalidSchedule {
            schedule: schedule.to_string(),
        });
    }
    Ok(())
}

fn validate_timezone(tz: &str) -> Result<(), ConfigError> {
    tz.parse::<chrono_tz::Tz>()
        .map_err(|_| ConfigError::SleepBatchInvalidTimezone {
            timezone: tz.to_string(),
        })?;
    Ok(())
}

fn normalize_agent_list(
    raw: Option<Vec<String>>,
    agents: &HashMap<AgentId, AgentConfig>,
) -> Result<Option<Vec<AgentId>>, ConfigError> {
    let Some(names) = raw else {
        return Ok(None);
    };
    let mut resolved = Vec::with_capacity(names.len());
    for name in &names {
        let id = AgentId::new(name);
        if !agents.contains_key(&id) {
            return Err(ConfigError::SleepBatchUnknownAgent {
                agent_id: id.to_string(),
            });
        }
        if !resolved.contains(&id) {
            resolved.push(id);
        }
    }
    Ok(Some(resolved))
}

fn sort_agent_list(agents: &mut [AgentId], default_agent: &AgentId) {
    agents.sort_by(|a, b| {
        let a_is_default = a == default_agent;
        let b_is_default = b == default_agent;
        b_is_default.cmp(&a_is_default).then_with(|| a.cmp(b))
    });
}

fn normalize_provider_map(
    providers: HashMap<String, FileProviderConfig>,
    dotenv: &HashMap<String, String>,
    allow_missing_api_key: bool,
) -> Result<HashMap<ProviderId, ProviderConfig>, ConfigError> {
    let mut normalized = HashMap::new();

    for (name, file_provider) in providers {
        let key =
            ProviderId::new(&normalize_string(Some(name)).ok_or(ConfigError::MissingProvider)?);
        let label = normalize_string(file_provider.label).unwrap_or_else(|| key.to_string());
        let base_url = normalize_string(file_provider.base_url)
            .or_else(|| {
                crate::llm::codex_auth::is_codex_provider(key.as_str())
                    .then_some("https://chatgpt.com/backend-api/codex".to_string())
            })
            .ok_or_else(|| ConfigError::MissingProviderBaseUrl {
                provider: key.to_string(),
            })?;
        validate_base_url(&base_url)?;

        let default_model = normalize_string(file_provider.default_model).ok_or_else(|| {
            ConfigError::MissingProviderDefaultModel {
                provider: key.to_string(),
            }
        })?;

        let models = match file_provider.models {
            Some(FileModels::Map(map)) => map
                .into_iter()
                .filter_map(|(k, v)| {
                    let model = normalize_string(Some(k))?;
                    Some((model, v.unwrap_or_default()))
                })
                .collect(),
            Some(FileModels::List(list)) => list
                .into_iter()
                .filter_map(|model| normalize_string(Some(model)))
                .map(|m| (m, ModelConfig::default()))
                .collect(),
            None => HashMap::new(),
        };
        let mut models = models;
        if !models.contains_key(&default_model) {
            models.insert(default_model.clone(), ModelConfig::default());
        }

        let api_key = resolve_string_or_ref(file_provider.api_key, dotenv)?;
        if !allow_missing_api_key
            && api_key.is_none()
            && !crate::llm::codex_auth::provider_allows_empty_api_key(key.as_str(), &base_url)
        {
            return Err(ConfigError::MissingProviderApiKey {
                provider: key.to_string(),
            });
        }

        normalized.insert(
            key,
            ProviderConfig {
                label,
                base_url,
                api_key,
                default_model,
                models,
            },
        );
    }

    Ok(normalized)
}

fn validate_base_url(value: &str) -> Result<(), ConfigError> {
    Url::parse(value)
        .map(|_| ())
        .map_err(|_| ConfigError::InvalidBaseUrl)
}

fn validate_compaction_config(config: &Config) -> Result<(), ConfigError> {
    if config.compaction_timeout_secs == 0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "compaction_timeout_secs must be at least 1".to_string(),
        ));
    }
    if config.max_history_messages == 0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "max_history_messages must be at least 1".to_string(),
        ));
    }
    if config.compact_keep_recent == 0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "compact_keep_recent must be at least 1".to_string(),
        ));
    }
    if config.compaction_threshold_ratio <= 0.0 || config.compaction_threshold_ratio > 1.0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "compaction_threshold_ratio must be between 0 (exclusive) and 1.0 (inclusive)"
                .to_string(),
        ));
    }
    if config.compaction_target_ratio <= 0.0 || config.compaction_target_ratio > 1.0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "compaction_target_ratio must be between 0 (exclusive) and 1.0 (inclusive)".to_string(),
        ));
    }
    if config.compaction_target_ratio >= config.compaction_threshold_ratio {
        return Err(ConfigError::InvalidCompactionConfig(
            "compaction_target_ratio must be less than compaction_threshold_ratio".to_string(),
        ));
    }
    if config.default_context_window_tokens == 0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "default_context_window_tokens must be at least 1".to_string(),
        ));
    }
    const MAX_DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 1_000_000;
    if config.default_context_window_tokens > MAX_DEFAULT_CONTEXT_WINDOW_TOKENS {
        return Err(ConfigError::InvalidCompactionConfig(format!(
            "default_context_window_tokens must not exceed {MAX_DEFAULT_CONTEXT_WINDOW_TOKENS}"
        )));
    }
    for (provider_id, provider) in &config.providers {
        for (model_name, model_config) in &provider.models {
            if let Some(tokens) = model_config.context_window_tokens {
                if tokens == 0 {
                    return Err(ConfigError::InvalidCompactionConfig(format!(
                        "context_window_tokens for {provider_id}/{model_name} must be at least 1"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_discord_bot_references(config: &Config) -> Result<(), ConfigError> {
    let empty_bots = HashMap::new();
    let bots = config
        .channels
        .get("discord")
        .and_then(|ch| ch.discord_bots.as_ref())
        .unwrap_or(&empty_bots);

    for bot_id in bots.keys() {
        validate_bot_id(bot_id)?;
    }

    let discord = config.channels.get("discord");
    if let Some(channels) = discord.and_then(|ch| ch.discord_channels.as_ref()) {
        for (channel_id, channel_config) in channels {
            for agent_id in &channel_config.agents {
                if !config.agents.contains_key(agent_id) {
                    return Err(ConfigError::DiscordBotChannelAgentNotFound {
                        bot_id: "discord".to_string(),
                        channel_id: *channel_id,
                        agent_id: agent_id.to_string(),
                    });
                }
            }
            let agent_count = channel_config.agents.len();
            let multi = channel_config.multi_agent;
            if multi && agent_count < 2 {
                return Err(ConfigError::DiscordBotChannelMultiAgentMismatch {
                    bot_id: "discord".to_string(),
                    channel_id: *channel_id,
                    reason: "multi_agent is true but fewer than 2 agents specified".to_string(),
                });
            }
            if !multi && agent_count > 1 {
                return Err(ConfigError::DiscordBotChannelMultiAgentMismatch {
                    bot_id: "discord".to_string(),
                    channel_id: *channel_id,
                    reason: "multi_agent is false but multiple agents specified".to_string(),
                });
            }
        }
    }

    for (agent_id, agent_config) in &config.agents {
        if let Some(ref bot_id) = agent_config.discord_bot {
            if !bots.contains_key(bot_id) {
                return Err(ConfigError::AgentDiscordBotNotFound {
                    agent_id: agent_id.to_string(),
                    bot_id: bot_id.to_string(),
                });
            }
        }
    }

    Ok(())
}

fn validate_provider_references<'a>(
    providers: &HashMap<ProviderId, ProviderConfig>,
    references: impl IntoIterator<Item = Option<&'a String>>,
) -> Result<(), ConfigError> {
    for provider in references.into_iter().flatten() {
        let provider_id = ProviderId::new(provider);
        if !providers.contains_key(&provider_id) {
            return Err(ConfigError::InvalidProviderReference {
                provider: provider.clone(),
            });
        }
    }
    Ok(())
}

fn validate_channel_provider_references(
    providers: &HashMap<ProviderId, ProviderConfig>,
    channels: &HashMap<ChannelName, ChannelConfig>,
) -> Result<(), ConfigError> {
    validate_provider_references(providers, channels.values().map(|c| c.provider.as_ref()))
}

fn validate_agent_provider_references(
    providers: &HashMap<ProviderId, ProviderConfig>,
    agents: &HashMap<AgentId, AgentConfig>,
) -> Result<(), ConfigError> {
    validate_provider_references(providers, agents.values().map(|a| a.provider.as_ref()))
}

fn apply_web_channel_env_overrides(channels: &mut HashMap<ChannelName, ChannelConfig>) {
    let web_host = env_var("WEB_HOST");
    let web_port = env_var("WEB_PORT").and_then(|value| value.parse::<u16>().ok());
    let web_enabled = env_var("WEB_ENABLED").and_then(|value| parse_bool(&value));
    let web_auth_token = env_var(WEB_AUTH_TOKEN_ENV_NAME);
    let web_allowed_origins = env_var("WEB_ALLOWED_ORIGINS").map(|value| {
        value
            .split(',')
            .filter_map(|origin| normalize_string(Some(origin.to_string())))
            .collect::<Vec<_>>()
    });

    if web_host.is_none()
        && web_port.is_none()
        && web_enabled.is_none()
        && web_auth_token.is_none()
        && web_allowed_origins.is_none()
    {
        return;
    }

    let web = channels.entry(ChannelName::new("web")).or_default();
    if let Some(enabled) = web_enabled {
        web.enabled = Some(enabled);
    }
    if let Some(host) = web_host {
        web.host = Some(host);
    }
    if let Some(port) = web_port {
        web.port = Some(port);
    }
    if let Some(token) = web_auth_token {
        web.auth_token = Some(ResolvedValue::Literal(token));
    }
    if let Some(origins) = web_allowed_origins {
        web.allowed_origins = Some(origins);
    }

    if web.host.is_none() {
        web.host = Some(super::resolve::default_web_host().to_string());
    }
    if web.port.is_none() {
        web.port = Some(super::resolve::default_web_port());
    }
}

fn apply_channel_bot_token_env_override(
    channels: &mut HashMap<ChannelName, ChannelConfig>,
    channel_name: &str,
    env_key: &str,
) {
    if let Some(token) = env_var(env_key) {
        let channel = channels.entry(ChannelName::new(channel_name)).or_default();
        channel.bot_token = Some(ResolvedValue::Literal(token));
    }
}

fn env_var(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .and_then(|value| normalize_string(Some(value)))
}

fn first_non_empty<const N: usize>(values: [Option<String>; N]) -> Option<String> {
    values.into_iter().find_map(normalize_string)
}

pub(super) fn normalize_string(value: Option<String>) -> Option<String> {
    value.and_then(|candidate| {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

/// URL として有効か検証する。setup wizard からも使用。
pub(crate) fn is_valid_base_url(url: &str) -> bool {
    Url::parse(url).is_ok()
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

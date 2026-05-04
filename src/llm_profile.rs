//! LLM provider/model 切替コマンドと設定更新を扱うモジュール。

use std::path::Path;

use crate::agent_loop::SurfaceContext;
use crate::config::{AgentId, ChannelName, Config, ProviderId};
use crate::error::EgoPulseError;
use crate::runtime::AppState;

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

pub async fn handle_command(
    state: &AppState,
    context: &SurfaceContext,
    input: &str,
) -> Result<Option<String>, EgoPulseError> {
    if !input.starts_with('/') {
        return Ok(None);
    }

    let parts = input.split_whitespace().collect::<Vec<_>>();
    let Some(command) = parts.first().copied() else {
        return Ok(None);
    };

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

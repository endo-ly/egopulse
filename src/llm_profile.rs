//! LLM provider/model 切替コマンドと設定更新を扱うモジュール。

use std::path::Path;

use crate::agent_loop::SurfaceContext;
use crate::config::Config;
use crate::error::EgoPulseError;
use crate::runtime::AppState;

const GLOBAL_SCOPE: &str = "global";
const AVAILABLE_SCOPES: &[&str] = &[GLOBAL_SCOPE, "web", "discord", "telegram"];

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
                    let marker = if id.as_str() == effective.provider { "*" } else { "-" };
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
            let scope = parse_scope(&parts[1..], command_scope(context))?;
            let resolved = resolved_for_scope(&config, &scope)?;
            let provider = config
                .providers
                .get(resolved.provider.as_str())
                .ok_or_else(|| EgoPulseError::Internal("provider not found".to_string()))?;
            let lines = provider
                .models
                .iter()
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
    let scope = parse_scope(&parts[1..], command_scope(context))?;

    if parts.len() == 1 {
        let resolved = resolved_for_scope(&config, &scope)?;
        return Ok(format!(
            "scope={scope} provider={} model={}",
            resolved.provider, resolved.model
        ));
    }

    let value = first_non_scope_arg(&parts[1..]).unwrap_or_default();
    if value == "reset" {
        if scope == GLOBAL_SCOPE {
            return Ok("global scope uses default_provider and cannot reset".to_string());
        }
        let path = config_path(state)?;
        let mut config = Config::load_allow_missing_api_key(Some(path))?;
        if let Some(channel) = config.channels.get_mut(scope.as_str()) {
            channel.provider = None;
            channel.model = None;
        }
        config.save_yaml(path)?;
        let updated = Config::load_allow_missing_api_key(Some(path))?;
        let resolved = resolved_for_scope(&updated, &scope)?;
        return Ok(format!(
            "scope={scope} provider reset -> {}",
            resolved.provider
        ));
    }

    if !config.providers.contains_key(value) {
        return Ok(format!("unknown provider: {value}"));
    }
    let path = config_path(state)?;
    let mut config = Config::load_allow_missing_api_key(Some(path))?;
    if scope == GLOBAL_SCOPE {
        config.default_provider = crate::config::ProviderId::new(value);
        config.default_model = None;
    } else {
        let channel = config.channels.entry(crate::config::ChannelName::new(&scope)).or_default();
        channel.provider = Some(value.to_string());
        channel.model = None;
    }
    config.save_yaml(path)?;
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
    let scope = parse_scope(&parts[1..], command_scope(context))?;
    let resolved = resolved_for_scope(&config, &scope)?;

    if parts.len() == 1 {
        return Ok(format!(
            "scope={scope} provider={} model={}",
            resolved.provider, resolved.model
        ));
    }

    let value = first_non_scope_arg(&parts[1..]).unwrap_or_default();
    if value == "reset" {
        if scope == GLOBAL_SCOPE {
            let mut config = Config::load_allow_missing_api_key(Some(config_path(state)?))?;
            config.default_model = None;
            config.save_yaml(config_path(state)?)?;
            return Ok(format!(
                "scope={scope} model reset -> {}",
                config.global_provider().default_model
            ));
        }
        let path = config_path(state)?;
        let mut config = Config::load_allow_missing_api_key(Some(path))?;
        if let Some(channel) = config.channels.get_mut(scope.as_str()) {
            channel.model = None;
        }
        config.save_yaml(path)?;
        let updated = Config::load_allow_missing_api_key(Some(path))?;
        let effective = resolved_for_scope(&updated, &scope)?;
        return Ok(format!("scope={scope} model reset -> {}", effective.model));
    }

    let path = config_path(state)?;
    let mut config = Config::load_allow_missing_api_key(Some(path))?;
    if scope == GLOBAL_SCOPE {
        config.default_model = Some(value.to_string());
        if let Some(provider) = config.providers.get_mut(&config.default_provider) {
            if !provider.models.iter().any(|m| m == value) {
                provider.models.push(value.to_string());
            }
        }
    } else {
        let channel = config.channels.entry(crate::config::ChannelName::new(&scope)).or_default();
        channel.model = Some(value.to_string());
    }
    config.save_yaml(path)?;
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

fn command_scope(context: &SurfaceContext) -> String {
    match context.channel.as_str() {
        "web" | "discord" | "telegram" => context.channel.clone(),
        _ => GLOBAL_SCOPE.to_string(),
    }
}

fn parse_scope(args: &[&str], fallback: String) -> Result<String, EgoPulseError> {
    let mut iter = args.iter().copied();
    while let Some(arg) = iter.next() {
        if arg == "--scope" {
            let value = iter
                .next()
                .ok_or_else(|| EgoPulseError::Internal("missing scope value".to_string()))?;
            return normalize_scope(value);
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

fn normalize_scope(scope: &str) -> Result<String, EgoPulseError> {
    let normalized = scope.trim().to_ascii_lowercase();
    if AVAILABLE_SCOPES
        .iter()
        .any(|candidate| *candidate == normalized)
    {
        Ok(normalized)
    } else {
        Err(EgoPulseError::Internal(format!("invalid scope: {scope}")))
    }
}

fn resolved_for_scope(
    config: &Config,
    scope: &str,
) -> Result<crate::config::ResolvedLlmConfig, EgoPulseError> {
    match scope {
        GLOBAL_SCOPE => Ok(config.resolve_global_llm()),
        channel => Ok(config.resolve_llm_for_channel(channel)?),
    }
}

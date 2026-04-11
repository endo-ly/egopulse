//! LLM provider/model 切替コマンドと設定更新を扱うモジュール。

use std::path::Path;

use serde_yml::{Mapping, Value};

use crate::agent_loop::SurfaceContext;
use crate::config::Config;
use crate::config_store::update_yaml;
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
            let config = state.current_config()?;
            let scope = command_scope(context);
            let effective = resolved_for_scope(&config, &scope)?;
            let lines = config
                .providers
                .iter()
                .map(|(id, provider)| {
                    let marker = if id == &effective.provider { "*" } else { "-" };
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
            let config = state.current_config()?;
            let scope = parse_scope(&parts[1..], command_scope(context))?;
            let resolved = resolved_for_scope(&config, &scope)?;
            let provider = config
                .providers
                .get(&resolved.provider)
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
    let config = state.current_config()?;
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
        save_scope_provider(path, &scope, None)?;
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
    save_scope_provider(path, &scope, Some(value))?;
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
    let config = state.current_config()?;
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
            return Ok(
                "global scope model is stored on the provider default and cannot reset".to_string(),
            );
        }
        let path = config_path(state)?;
        save_scope_model(path, &scope, None, &resolved.provider)?;
        let updated = Config::load_allow_missing_api_key(Some(path))?;
        let effective = resolved_for_scope(&updated, &scope)?;
        return Ok(format!("scope={scope} model reset -> {}", effective.model));
    }

    let path = config_path(state)?;
    save_scope_model(path, &scope, Some(value), &resolved.provider)?;
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

fn save_scope_provider(
    path: &Path,
    scope: &str,
    provider: Option<&str>,
) -> Result<(), EgoPulseError> {
    update_yaml(path, |root| {
        if scope == GLOBAL_SCOPE {
            let provider = provider
                .ok_or_else(|| EgoPulseError::Internal("provider is required".to_string()))?;
            set_string(root, "default_provider", provider);
        } else {
            let channel = ensure_channel_mapping(root, scope);
            match provider {
                Some(provider) => {
                    channel.insert(
                        Value::String("provider".to_string()),
                        Value::String(provider.to_string()),
                    );
                }
                None => {
                    channel.remove(Value::String("provider".to_string()));
                }
            }
            channel.remove(Value::String("model".to_string()));
        }
        Ok(())
    })
}

fn save_scope_model(
    path: &Path,
    scope: &str,
    model: Option<&str>,
    provider: &str,
) -> Result<(), EgoPulseError> {
    update_yaml(path, |root| {
        if scope == GLOBAL_SCOPE {
            let model =
                model.ok_or_else(|| EgoPulseError::Internal("model is required".to_string()))?;
            let provider_mapping = ensure_provider_mapping(root, provider);
            provider_mapping.insert(
                Value::String("default_model".to_string()),
                Value::String(model.to_string()),
            );
            let mut models = provider_mapping
                .get(Value::String("models".to_string()))
                .and_then(value_as_string_vec)
                .unwrap_or_default();
            if !models.iter().any(|candidate| candidate == model) {
                models.push(model.to_string());
            }
            provider_mapping.insert(
                Value::String("models".to_string()),
                serde_yml::to_value(models)
                    .map_err(|error| EgoPulseError::Internal(error.to_string()))?,
            );
        } else {
            let channel = ensure_channel_mapping(root, scope);
            match model {
                Some(model) => {
                    channel.insert(
                        Value::String("model".to_string()),
                        Value::String(model.to_string()),
                    );
                }
                None => {
                    channel.remove(Value::String("model".to_string()));
                }
            }
        }
        Ok(())
    })
}

fn ensure_provider_mapping<'a>(root: &'a mut Value, provider: &str) -> &'a mut Mapping {
    let providers = ensure_mapping(entry_mut(root, "providers"));
    ensure_mapping(
        providers
            .entry(Value::String(provider.to_string()))
            .or_insert(Value::Mapping(Mapping::new())),
    )
}

fn ensure_channel_mapping<'a>(root: &'a mut Value, channel: &str) -> &'a mut Mapping {
    let channels = ensure_mapping(entry_mut(root, "channels"));
    ensure_mapping(
        channels
            .entry(Value::String(channel.to_string()))
            .or_insert(Value::Mapping(Mapping::new())),
    )
}

fn set_string(root: &mut Value, key: &str, value: &str) {
    let mapping = ensure_mapping(root);
    mapping.insert(
        Value::String(key.to_string()),
        Value::String(value.to_string()),
    );
}

fn entry_mut<'a>(root: &'a mut Value, key: &str) -> &'a mut Value {
    ensure_mapping(root)
        .entry(Value::String(key.to_string()))
        .or_insert(Value::Mapping(Mapping::new()))
}

fn ensure_mapping(value: &mut Value) -> &mut Mapping {
    if !matches!(value, Value::Mapping(_)) {
        *value = Value::Mapping(Mapping::new());
    }
    match value {
        Value::Mapping(mapping) => mapping,
        _ => unreachable!(),
    }
}

fn value_as_string_vec(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Sequence(values) => Some(
            values
                .iter()
                .filter_map(|value| match value {
                    Value::String(value) => {
                        Some(value.trim().to_string()).filter(|value| !value.is_empty())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
        ),
        _ => None,
    }
}

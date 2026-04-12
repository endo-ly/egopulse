//! Web 設定 API を扱うモジュール。
//!
//! グローバル設定（provider/model）とチャネル別overrideの参照・保存をHTTPとして公開する。

use std::collections::HashMap;
use std::path::PathBuf;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::config::{Config, ProviderConfig, default_config_path};
use crate::error::ConfigError;

use super::WebState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ProviderPayload {
    id: String,
    label: String,
    base_url: String,
    default_model: String,
    models: Vec<String>,
    has_api_key: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ChannelOverridePayload {
    provider: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ConfigPayload {
    default_provider: String,
    default_model: Option<String>,
    effective_model: String,
    data_dir: String,
    workspace_dir: String,
    web_enabled: bool,
    web_host: String,
    web_port: u16,
    web_auth_enabled: bool,
    has_api_key: bool,
    config_path: String,
    providers: Vec<ProviderPayload>,
    channel_overrides: HashMap<String, ChannelOverridePayload>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ProviderUpdatePayload {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    models: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ChannelOverrideUpdatePayload {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct ConfigUpdateRequest {
    default_provider: String,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    providers: Option<HashMap<String, ProviderUpdatePayload>>,
    web_enabled: bool,
    web_host: String,
    web_port: u16,
    #[serde(default)]
    channel_overrides: Option<HashMap<String, ChannelOverrideUpdatePayload>>,
}

/// Returns the persisted configuration visible to the web UI.
pub(super) async fn api_get_config(
    State(state): State<WebState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = config_path_for_save(&state).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let display = match Config::load_allow_missing_api_key(Some(&path)) {
        Ok(config) => Some(config),
        Err(ConfigError::ConfigNotFound { .. }) => None,
        Err(error) => return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string())),
    };

    Ok(Json(serde_json::json!({
        "ok": true,
        "config": match display.as_ref() {
            Some(display) => payload_from_config(display, &path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
            None => default_payload(&path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        },
    })))
}

/// Persists a config update from the web UI.
pub(super) async fn api_put_config(
    State(state): State<WebState>,
    Json(request): Json<ConfigUpdateRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let default_provider = request.default_provider.trim();
    if default_provider.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "default_provider is required".to_string(),
        ));
    }
    if request.web_host.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "web_host is required".to_string()));
    }
    if request.web_port == 0 {
        return Err((
            StatusCode::BAD_REQUEST,
            "web_port must be greater than 0".to_string(),
        ));
    }

    let path = config_path_for_save(&state).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let mut config = match Config::load_allow_missing_api_key(Some(&path)) {
        Ok(config) => config,
        Err(ConfigError::ConfigNotFound { .. }) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "config file not found; run 'egopulse setup' first".to_string(),
            ));
        }
        Err(error) => return Err((StatusCode::BAD_REQUEST, error.to_string())),
    };

    config.default_model = request.default_model.as_ref().and_then(|m| {
        let trimmed = m.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    if let Some(provider_updates) = request.providers {
        apply_provider_updates(&mut config, provider_updates);
    }

    if config.providers.contains_key(default_provider) {
        config.default_provider = default_provider.to_string();
    }

    let web_enabled = request.web_enabled;
    let web_host = request.web_host.trim().to_string();
    let web_port = request.web_port;

    {
        let web = config.channels.entry("web".to_string()).or_default();
        web.enabled = Some(web_enabled);
        web.host = Some(web_host);
        web.port = Some(web_port);
    }

    if let Some(overrides) = request.channel_overrides {
        apply_channel_overrides(&mut config, overrides)
            .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    }

    config
        .save_yaml(&path)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    let display = Config::load_allow_missing_api_key(Some(&path))
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "config": payload_from_config(&display, &path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    })))
}

fn apply_provider_updates(config: &mut Config, updates: HashMap<String, ProviderUpdatePayload>) {
    for (id, update) in updates {
        let id = id.trim().to_ascii_lowercase();
        if id.is_empty() {
            continue;
        }

        if let Some(existing) = config.providers.get_mut(&id) {
            if let Some(label) = update.label {
                let trimmed = label.trim();
                if !trimmed.is_empty() {
                    existing.label = trimmed.to_string();
                }
            }
            if let Some(base_url) = update.base_url {
                let trimmed = base_url.trim();
                if !trimmed.is_empty() {
                    existing.base_url = trimmed.to_string();
                }
            }
            if let Some(model) = update.default_model {
                let trimmed = model.trim();
                if !trimmed.is_empty() {
                    existing.default_model = trimmed.to_string();
                }
            }
            if let Some(models) = update.models {
                let final_default = existing.default_model.clone();
                existing.models = models
                    .into_iter()
                    .filter_map(|m| {
                        let trimmed = m.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        }
                    })
                    .collect();
                if !existing.models.contains(&final_default) {
                    existing.models.push(final_default);
                }
            }
            apply_api_key_update(&mut existing.api_key, update.api_key);
        } else {
            let label = update
                .label
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| infer_provider_label(&id));

            let base_url = match update.base_url {
                Some(url) if !url.trim().is_empty() => url.trim().to_string(),
                _ => continue,
            };

            let default_model = match update.default_model {
                Some(model) if !model.trim().is_empty() => model.trim().to_string(),
                _ => continue,
            };

            let mut models = update
                .models
                .unwrap_or_default()
                .into_iter()
                .filter_map(|m| {
                    let trimmed = m.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                })
                .collect::<Vec<_>>();

            if !models.contains(&default_model) {
                models.push(default_model.clone());
            }

            let mut api_key = None;
            apply_api_key_update(&mut api_key, update.api_key);

            config.providers.insert(
                id,
                ProviderConfig {
                    label,
                    base_url,
                    api_key,
                    default_model,
                    models,
                },
            );
        }
    }
}

fn apply_api_key_update(current: &mut Option<SecretString>, raw_update: Option<String>) {
    let Some(value) = raw_update else { return };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        // empty string → keep existing
        return;
    }
    if trimmed == "*CLEAR*" {
        *current = None;
    } else {
        *current = Some(SecretString::new(trimmed.to_string().into_boxed_str()));
    }
}

fn apply_channel_overrides(
    config: &mut Config,
    overrides: HashMap<String, ChannelOverrideUpdatePayload>,
) -> Result<(), ConfigError> {
    for (channel, update) in overrides {
        let key = channel.trim().to_ascii_lowercase();
        if key.is_empty() || key == "web" {
            continue;
        }

        let entry = config.channels.entry(key.clone()).or_default();

        let provider_name = match update.provider {
            Some(provider) => {
                let trimmed = provider.trim();
                let channel_name = key.clone();
                if trimmed.is_empty() {
                    None
                } else if config.providers.contains_key(trimmed) {
                    Some(trimmed.to_string())
                } else {
                    return Err(ConfigError::InvalidProviderReference {
                        provider: format!("{} for channel {}", trimmed, channel_name),
                    });
                }
            }
            None => entry.provider.clone(),
        };
        entry.provider = provider_name;

        match update.model {
            Some(model) if !model.trim().is_empty() => {
                entry.model = Some(model.trim().to_string());
            }
            Some(_) => {
                entry.model = None;
            }
            None => {}
        }
    }
    Ok(())
}

fn default_payload(path: &std::path::Path) -> Result<ConfigPayload, ConfigError> {
    Ok(ConfigPayload {
        default_provider: String::new(),
        default_model: None,
        effective_model: String::new(),
        data_dir: crate::config::default_data_dir()?
            .to_string_lossy()
            .into_owned(),
        workspace_dir: crate::config::default_workspace_dir()?
            .to_string_lossy()
            .into_owned(),
        web_enabled: false,
        web_host: "127.0.0.1".to_string(),
        web_port: 10961,
        web_auth_enabled: false,
        has_api_key: false,
        config_path: path.display().to_string(),
        providers: Vec::new(),
        channel_overrides: HashMap::new(),
    })
}

fn config_path_for_save(state: &WebState) -> Result<PathBuf, ConfigError> {
    match &state.config_path {
        Some(path) => Ok(path.clone()),
        None => default_config_path(),
    }
}

fn payload_from_config(config: &Config, path: &std::path::Path) -> Result<ConfigPayload, ConfigError> {
    let resolved = config.resolve_global_llm();

    let providers = config
        .providers
        .iter()
        .map(|(id, provider)| ProviderPayload {
            id: id.clone(),
            label: provider.label.clone(),
            base_url: provider.base_url.clone(),
            default_model: provider.default_model.clone(),
            models: provider.models.clone(),
            has_api_key: provider.api_key.is_some(),
        })
        .collect::<Vec<_>>();

    let channel_overrides = config
        .channels
        .iter()
        .filter_map(|(id, channel)| {
            if id == "web" {
                return None;
            }
            Some((
                id.clone(),
                ChannelOverridePayload {
                    provider: channel.provider.clone(),
                    model: channel.model.clone(),
                },
            ))
        })
        .collect();

    Ok(ConfigPayload {
        default_provider: resolved.provider,
        default_model: config.default_model.clone(),
        effective_model: resolved.model,
        data_dir: config.data_dir.clone(),
        workspace_dir: crate::config::default_workspace_dir()?
            .to_string_lossy()
            .into_owned(),
        web_enabled: config.web_enabled(),
        web_host: config.web_host(),
        web_port: config.web_port(),
        web_auth_enabled: config.web_auth_token().is_some(),
        has_api_key: resolved.api_key.is_some(),
        config_path: path.display().to_string(),
        providers,
        channel_overrides,
    })
}

fn infer_provider_label(provider: &str) -> String {
    match provider {
        "openai" => "OpenAI".to_string(),
        "openrouter" => "OpenRouter".to_string(),
        "local" => "Local OpenAI-compatible".to_string(),
        "custom" => "Custom OpenAI-compatible".to_string(),
        other => other.to_string(),
    }
}

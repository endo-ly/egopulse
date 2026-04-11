//! Web 設定 API を扱うモジュール。
//!
//! provider / model / scope を中心とした設定の参照と YAML 保存を HTTP として公開する。

use std::path::PathBuf;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_yml::{Mapping, Value};

use crate::config::{Config, default_config_path};
use crate::config_store::update_yaml;
use crate::error::ConfigError;

use super::WebState;

const GLOBAL_SCOPE: &str = "global";
const SUPPORTED_SCOPES: &[&str] = &[GLOBAL_SCOPE, "web", "discord", "telegram"];

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
struct ConfigPayload {
    scope: String,
    provider: String,
    model: String,
    base_url: String,
    data_dir: String,
    workspace_dir: String,
    web_enabled: bool,
    web_host: String,
    web_port: u16,
    web_auth_enabled: bool,
    has_api_key: bool,
    config_path: String,
    scopes: Vec<String>,
    providers: Vec<ProviderPayload>,
}

#[derive(Debug, Deserialize)]
/// Accepts mutable config values from the browser client.
pub(super) struct ConfigUpdateRequest {
    scope: String,
    provider: String,
    model: String,
    #[serde(default)]
    base_url: Option<String>,
    web_enabled: bool,
    web_host: String,
    web_port: u16,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    clear_api_key: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct ConfigQuery {
    #[serde(default)]
    scope: Option<String>,
}

/// Returns the persisted configuration visible to the web UI.
pub(super) async fn api_get_config(
    State(state): State<WebState>,
    Query(query): Query<ConfigQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = config_path_for_save(&state);
    let display = match Config::load_allow_missing_api_key(Some(&path)) {
        Ok(config) => Some(config),
        Err(ConfigError::ConfigNotFound { .. }) => None,
        Err(error) => return Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string())),
    };
    let scope = query
        .scope
        .as_deref()
        .map(normalize_scope)
        .transpose()?
        .unwrap_or_else(|| GLOBAL_SCOPE.to_string());

    Ok(Json(serde_json::json!({
        "ok": true,
        "config": match display.as_ref() {
            Some(display) => payload_from_config(display, &path, &scope)?,
            None => default_payload(&path, &scope),
        },
    })))
}

/// Persists a config update from the web UI.
pub(super) async fn api_put_config(
    State(state): State<WebState>,
    Json(request): Json<ConfigUpdateRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let scope = normalize_scope(&request.scope)?;
    let provider = request.provider.trim();
    let model = request.model.trim();
    if provider.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "provider is required".to_string()));
    }
    if model.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "model is required".to_string()));
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

    let path = config_path_for_save(&state);
    let current_config = match Config::load_allow_missing_api_key(Some(&path)) {
        Ok(config) => Some(config),
        Err(ConfigError::ConfigNotFound { .. }) => None,
        Err(error) => return Err((StatusCode::BAD_REQUEST, error.to_string())),
    };
    update_yaml(&path, |root| {
        if scope == GLOBAL_SCOPE {
            upsert_provider(
                root,
                provider,
                model,
                request
                    .base_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty()),
                normalized_api_key_update(request.api_key.as_deref(), request.clear_api_key),
            )
            .map_err(|(_, error)| crate::error::EgoPulseError::Internal(error))?;
            set_string(root, "default_provider", provider);
        } else {
            if current_config
                .as_ref()
                .is_none_or(|config| !config.providers.contains_key(provider))
            {
                return Err(crate::error::EgoPulseError::Internal(format!(
                    "unknown provider for scope {scope}: {provider}"
                )));
            }
            let channel = ensure_channel_mapping(root, &scope);
            channel.insert(
                Value::String("provider".to_string()),
                Value::String(provider.to_string()),
            );
            channel.insert(
                Value::String("model".to_string()),
                Value::String(model.to_string()),
            );
        }

        let web = ensure_channel_mapping(root, "web");
        web.insert(
            Value::String("enabled".to_string()),
            Value::Bool(request.web_enabled),
        );
        web.insert(
            Value::String("host".to_string()),
            Value::String(request.web_host.trim().to_string()),
        );
        web.insert(
            Value::String("port".to_string()),
            serde_yml::to_value(request.web_port)
                .map_err(|error| crate::error::EgoPulseError::Internal(error.to_string()))?,
        );

        Ok(())
    })
    .map_err(internal_error)?;

    let display = Config::load_allow_missing_api_key(Some(&path))
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "config": payload_from_config(&display, &path, &scope)?,
    })))
}

fn default_payload(path: &std::path::Path, scope: &str) -> ConfigPayload {
    ConfigPayload {
        scope: scope.to_string(),
        provider: String::new(),
        model: String::new(),
        base_url: String::new(),
        data_dir: crate::config::default_data_dir()
            .to_string_lossy()
            .into_owned(),
        workspace_dir: crate::config::default_workspace_dir()
            .to_string_lossy()
            .into_owned(),
        web_enabled: false,
        web_host: "127.0.0.1".to_string(),
        web_port: 10961,
        web_auth_enabled: false,
        has_api_key: false,
        config_path: path.display().to_string(),
        scopes: SUPPORTED_SCOPES
            .iter()
            .map(|scope| (*scope).to_string())
            .collect(),
        providers: Vec::new(),
    }
}

fn config_path_for_save(state: &WebState) -> PathBuf {
    state
        .config_path
        .clone()
        .unwrap_or_else(default_config_path)
}

fn payload_from_config(
    config: &Config,
    path: &std::path::Path,
    scope: &str,
) -> Result<ConfigPayload, (StatusCode, String)> {
    let resolved = match scope {
        GLOBAL_SCOPE => config.resolve_global_llm(),
        channel => config
            .resolve_llm_for_channel(channel)
            .map_err(internal_error)?,
    };

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

    Ok(ConfigPayload {
        scope: scope.to_string(),
        provider: resolved.provider,
        model: resolved.model,
        base_url: resolved.base_url,
        data_dir: config.data_dir.clone(),
        workspace_dir: crate::config::default_workspace_dir()
            .to_string_lossy()
            .into_owned(),
        web_enabled: config.web_enabled(),
        web_host: config.web_host(),
        web_port: config.web_port(),
        web_auth_enabled: config.web_auth_token().is_some(),
        has_api_key: resolved.api_key.is_some(),
        config_path: path.display().to_string(),
        scopes: SUPPORTED_SCOPES
            .iter()
            .map(|scope| (*scope).to_string())
            .collect(),
        providers,
    })
}

fn normalize_scope(scope: &str) -> Result<String, (StatusCode, String)> {
    let normalized = scope.trim().to_ascii_lowercase();
    if SUPPORTED_SCOPES
        .iter()
        .any(|candidate| *candidate == normalized)
    {
        return Ok(normalized);
    }
    Err((StatusCode::BAD_REQUEST, "invalid scope".to_string()))
}

fn upsert_provider(
    root: &mut Value,
    provider: &str,
    model: &str,
    base_url: Option<&str>,
    api_key_update: ApiKeyUpdate<'_>,
) -> Result<(), (StatusCode, String)> {
    let providers = ensure_mapping(entry_mut(root, "providers"));
    let provider_value = providers
        .entry(Value::String(provider.to_string()))
        .or_insert(Value::Mapping(Mapping::new()));
    let provider_mapping = ensure_mapping(provider_value);

    let current_base_url = provider_mapping
        .get(Value::String("base_url".to_string()))
        .and_then(value_as_trimmed_str);
    let effective_base_url = base_url.or(current_base_url.as_deref()).ok_or((
        StatusCode::BAD_REQUEST,
        "base_url is required when creating a provider".to_string(),
    ))?;

    if !provider_mapping.contains_key(Value::String("label".to_string())) {
        provider_mapping.insert(
            Value::String("label".to_string()),
            Value::String(infer_provider_label(provider).to_string()),
        );
    }
    provider_mapping.insert(
        Value::String("base_url".to_string()),
        Value::String(effective_base_url.to_string()),
    );
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
        serde_yml::to_value(models).map_err(internal_error)?,
    );

    match api_key_update {
        ApiKeyUpdate::Replace(value) => {
            provider_mapping.insert(
                Value::String("api_key".to_string()),
                Value::String(value.to_string()),
            );
        }
        ApiKeyUpdate::Clear => {
            provider_mapping.remove(Value::String("api_key".to_string()));
        }
        ApiKeyUpdate::NoChange => {}
    }

    Ok(())
}

enum ApiKeyUpdate<'a> {
    NoChange,
    Replace(&'a str),
    Clear,
}

fn normalized_api_key_update(api_key: Option<&str>, clear_api_key: bool) -> ApiKeyUpdate<'_> {
    if clear_api_key {
        return ApiKeyUpdate::Clear;
    }
    match api_key.map(str::trim) {
        Some(value) if !value.is_empty() => ApiKeyUpdate::Replace(value),
        _ => ApiKeyUpdate::NoChange,
    }
}

fn ensure_channel_mapping<'a>(root: &'a mut Value, channel: &str) -> &'a mut Mapping {
    let channels = ensure_mapping(entry_mut(root, "channels"));
    ensure_mapping(
        channels
            .entry(Value::String(channel.to_string()))
            .or_insert(Value::Mapping(Mapping::new())),
    )
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

fn value_as_trimmed_str(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

fn value_as_string_vec(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Sequence(values) => Some(
            values
                .iter()
                .filter_map(value_as_trimmed_str)
                .collect::<Vec<_>>(),
        ),
        _ => None,
    }
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

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

use std::path::PathBuf;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_yml::{Mapping, Value};

use crate::config::{ChannelConfig, Config};

use super::WebState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ConfigPayload {
    model: String,
    base_url: String,
    data_dir: String,
    web_enabled: bool,
    web_host: String,
    web_port: u16,
    has_api_key: bool,
    config_path: String,
    requires_restart: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct ConfigUpdateRequest {
    model: String,
    base_url: String,
    data_dir: String,
    web_enabled: bool,
    web_host: String,
    web_port: u16,
    #[serde(default)]
    api_key: Option<String>,
}

pub(super) async fn api_get_config(
    State(state): State<WebState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = config_path_for_save(&state);
    let display = Config::load(Some(&path)).unwrap_or_else(|_| state.app_state.config.clone());

    Ok(Json(serde_json::json!({
        "ok": true,
        "config": payload_from_config(&display, &path),
        "requires_restart": true,
    })))
}

pub(super) async fn api_put_config(
    State(state): State<WebState>,
    Json(request): Json<ConfigUpdateRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if request.model.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "model is required".to_string()));
    }
    if request.base_url.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "base_url is required".to_string()));
    }
    if request.data_dir.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "data_dir is required".to_string()));
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
    let mut root = read_existing_yaml(&path)?;

    set_string(&mut root, "model", request.model.trim());
    set_string(&mut root, "base_url", request.base_url.trim());
    set_string(&mut root, "data_dir", request.data_dir.trim());
    remove_key(&mut root, "web_enabled");
    remove_key(&mut root, "web_host");
    remove_key(&mut root, "web_port");

    match request.api_key.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => set_string(&mut root, "api_key", value),
        Some(_) => remove_key(&mut root, "api_key"),
        None => {}
    }

    let channels = ensure_mapping(entry_mut(&mut root, "channels"));
    let web = ensure_mapping(
        channels
            .entry(Value::String("web".to_string()))
            .or_insert(Value::Mapping(Mapping::new())),
    );
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
        serde_yml::to_value(request.web_port).map_err(internal_error)?,
    );

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(internal_error)?;
    }
    let yaml = serde_yml::to_string(&root).map_err(internal_error)?;
    std::fs::write(&path, yaml).map_err(internal_error)?;

    let display = match Config::load_allow_missing_api_key(Some(&path)) {
        Ok(config) => config,
        Err(_) => Config {
            model: request.model.trim().to_string(),
            api_key: state.app_state.config.api_key.clone(),
            llm_base_url: request.base_url.trim().to_string(),
            data_dir: request.data_dir.trim().to_string(),
            log_level: state.app_state.config.log_level.clone(),
            channels: std::collections::HashMap::from([(
                "web".to_string(),
                ChannelConfig {
                    enabled: Some(request.web_enabled),
                    host: Some(request.web_host.trim().to_string()),
                    port: Some(request.web_port),
                },
            )]),
        },
    };

    Ok(Json(serde_json::json!({
        "ok": true,
        "config": payload_from_config(&display, &path),
        "requires_restart": true,
    })))
}

fn config_path_for_save(state: &WebState) -> PathBuf {
    state
        .config_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("./egopulse.config.yaml"))
}

fn payload_from_config(config: &Config, path: &std::path::Path) -> ConfigPayload {
    ConfigPayload {
        model: config.model.clone(),
        base_url: config.llm_base_url.clone(),
        data_dir: config.data_dir.clone(),
        web_enabled: config.web_enabled(),
        web_host: config.web_host(),
        web_port: config.web_port(),
        has_api_key: config.api_key.is_some(),
        config_path: path.display().to_string(),
        requires_restart: true,
    }
}

fn read_existing_yaml(path: &std::path::Path) -> Result<Value, (StatusCode, String)> {
    if !path.exists() {
        return Ok(Value::Mapping(Mapping::new()));
    }

    let raw = std::fs::read_to_string(path).map_err(internal_error)?;
    let parsed: Value = serde_yml::from_str(&raw).map_err(internal_error)?;
    Ok(match parsed {
        Value::Mapping(_) => parsed,
        _ => Value::Mapping(Mapping::new()),
    })
}

fn set_string(root: &mut Value, key: &str, value: &str) {
    let mapping = ensure_mapping(root);
    mapping.insert(
        Value::String(key.to_string()),
        Value::String(value.to_string()),
    );
}

fn remove_key(root: &mut Value, key: &str) {
    let mapping = ensure_mapping(root);
    mapping.remove(Value::String(key.to_string()));
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

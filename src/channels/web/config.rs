//! Web config API: the structured view of the runtime configuration, the
//! masked YAML source, and targeted updates.
//!
//! Every update copies the current snapshot, replaces only the fields the
//! request carries, and hands the candidate to `ConfigManager::apply_candidate`,
//! which validates, persists, and enforces `expected_fingerprint`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::config::loader::active_env_overrides;
use crate::config::manager::ConfigSnapshot;
use crate::config::resolve as defaults;
use crate::config::secret_ref::{
    EnvOrigin, ResolvedValue, env_resolved_value, provider_api_key_env_name,
};
use crate::config::{
    AgentId, BotId, Config, LlmSettingSource, ModelConfig, ProviderId, default_config_path,
    parse_duration,
};
use crate::error::{ConfigError, EgoPulseError};
use crate::tools::mcp::{McpServerConfig, configured_server_definitions};

use super::{ApiError, ApiResult, WebState, api_error, bad_request, internal_error, not_found};

/// Replacement shown for every secret value in the masked YAML.
const MASK: &str = "********";

// ---------------------------------------------------------------------------
// GET /api/config
// ---------------------------------------------------------------------------

/// Returns the configuration grouped by screen section.
///
/// Secrets are reported by kind and reference name only: no value, no `exec`
/// command, and only the names of extra provider headers.
///
/// # Errors
///
/// Returns `500` when the config path cannot be determined.
pub(super) async fn api_get_config(State(state): State<WebState>) -> ApiResult {
    let path = config_path(&state)?;
    let snapshot = state.app_state.config_manager.current_blocking();
    Ok(Json(config_payload(&snapshot, &path)))
}

fn config_payload(snapshot: &ConfigSnapshot, path: &std::path::Path) -> Value {
    let config = &snapshot.config;
    let env_overrides: BTreeMap<&str, &str> = active_env_overrides().into_iter().collect();
    json!({
        "ok": true,
        "revision": snapshot.revision,
        "fingerprint": snapshot.fingerprint,
        "config_path": path.display().to_string(),
        "env_overrides": env_overrides,
        "general": general_payload(config),
        "agents": agents_payload(config),
        "providers": providers_payload(config),
        "channels": channels_payload(config),
        "sleep": sleep_payload(config),
        "pulse": {
            "tick_interval": format_duration(config.pulse.tick_interval_secs),
            "default_tick_interval": format_duration(crate::config::PulseConfig::default().tick_interval_secs),
        },
        "webhooks": webhooks_payload(config),
        "backup": {
            "enabled": config.db.backup.enabled,
            "interval_days": config.db.backup.interval_days,
            "time": config.db.backup.time,
            "max_generations": config.db.backup.max_generations,
        },
        "web_fetch": web_fetch_payload(config),
    })
}

fn secret_payload(value: Option<&ResolvedValue>) -> Value {
    match value {
        None => json!({ "kind": null, "ref": null, "origin": null, "configured": false }),
        Some(ResolvedValue::Literal(v)) => {
            json!({ "kind": "literal", "ref": null, "origin": null, "configured": !v.trim().is_empty() })
        }
        Some(ResolvedValue::EnvRef { id, value, origin }) => {
            json!({
                "kind": "env",
                "ref": id,
                "origin": match origin {
                    EnvOrigin::Process => "process",
                    EnvOrigin::Dotenv => "dotenv",
                },
                "configured": !value.trim().is_empty(),
            })
        }
        Some(ResolvedValue::ExecRef { value, .. }) => {
            json!({ "kind": "exec", "ref": null, "origin": null, "configured": !value.trim().is_empty() })
        }
    }
}

fn general_payload(config: &Config) -> Value {
    json!({
        "default_agent": config.default_agent.as_str(),
        "default_provider": config.default_provider.as_str(),
        "default_model": config.default_model,
        "timezone": config.timezone,
        "max_history_messages": config.max_history_messages,
        "compaction_timeout_secs": config.compaction_timeout_secs,
        "default_context_window_tokens": config.default_context_window_tokens,
        "compaction_threshold_ratio": config.compaction_threshold_ratio,
        "compaction_target_ratio": config.compaction_target_ratio,
        "compact_keep_recent": config.compact_keep_recent,
        "log_level": config.log_level,
        "state_root": config.state_root,
        "defaults": {
            "max_history_messages": defaults::default_max_history_messages(),
            "compaction_timeout_secs": defaults::default_compaction_timeout_secs(),
            "default_context_window_tokens": defaults::default_context_window_tokens(),
            "compaction_threshold_ratio": defaults::default_compaction_threshold_ratio(),
            "compaction_target_ratio": defaults::default_compaction_target_ratio(),
            "compact_keep_recent": defaults::default_compact_keep_recent(),
            "timezone": "UTC",
            "log_level": "info",
        },
    })
}

fn source_name(source: LlmSettingSource) -> &'static str {
    match source {
        LlmSettingSource::Profile => "profile",
        LlmSettingSource::Agent => "agent",
        LlmSettingSource::GlobalDefault => "global_default",
        LlmSettingSource::ProviderDefault => "provider_default",
    }
}

fn agents_payload(config: &Config) -> Vec<Value> {
    let mut ids: Vec<&AgentId> = config.agents.keys().collect();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let agent = &config.agents[id];
            let effective = match config.resolve_llm_origin_for_agent(id, None) {
                Ok((resolved, origin)) => json!({
                    "provider": resolved.provider,
                    "model": resolved.model,
                    "provider_source": source_name(origin.provider),
                    "model_source": source_name(origin.model),
                }),
                Err(_) => Value::Null,
            };
            let mut profiles: Vec<Value> = agent
                .profiles
                .iter()
                .map(|(channel, profile)| {
                    let resolved = config
                        .resolve_llm_origin_for_agent(id, Some(channel))
                        .ok()
                        .map(|(r, _)| json!({ "provider": r.provider, "model": r.model }));
                    json!({
                        "channel": channel,
                        "provider": profile.provider,
                        "model": profile.model,
                        "effective": resolved,
                    })
                })
                .collect();
            profiles.sort_by(|a, b| a["channel"].as_str().cmp(&b["channel"].as_str()));
            json!({
                "id": id.as_str(),
                "label": agent.label,
                "is_default": *id == config.default_agent,
                "provider": agent.provider,
                "model": agent.model,
                "reasoning_effort": agent.reasoning_effort,
                "discord_bot": agent.discord_bot.as_ref().map(BotId::as_str),
                "telegram_bot": agent.telegram_bot.as_ref().map(BotId::as_str),
                "effective": effective,
                "profiles": profiles,
            })
        })
        .collect()
}

fn providers_payload(config: &Config) -> Vec<Value> {
    let mut ids: Vec<&ProviderId> = config.providers.keys().collect();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let provider = &config.providers[id];
            let mut models: Vec<Value> = provider
                .models
                .iter()
                .map(|(name, model)| {
                    json!({
                        "name": name,
                        "context_window_tokens": model.context_window_tokens,
                        "has_instructions": model.model_instructions.is_some() || model.model_instructions_file.is_some(),
                    })
                })
                .collect();
            models.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            let mut used_by: Vec<&str> = config
                .agents
                .keys()
                .filter(|agent| {
                    config
                        .resolve_llm_origin_for_agent(agent, None)
                        .is_ok_and(|(resolved, _)| resolved.provider == id.as_str())
                })
                .map(AgentId::as_str)
                .collect();
            used_by.sort_unstable();
            json!({
                "id": id.as_str(),
                "label": provider.label,
                "base_url": provider.base_url,
                "default_model": provider.default_model,
                "models": models,
                "api_key": secret_payload(provider.api_key.as_ref()),
                "api_key_optional": crate::llm::codex_auth::provider_allows_empty_api_key(id.as_str(), &provider.base_url),
                "header_names": provider.headers.keys().collect::<Vec<_>>(),
                "used_by": used_by,
            })
        })
        .collect()
}

fn bound_agents(config: &Config, bot: &str, telegram: bool) -> Vec<String> {
    let mut agents: Vec<String> = config
        .agents
        .iter()
        .filter(|(_, agent)| {
            let binding = if telegram {
                &agent.telegram_bot
            } else {
                &agent.discord_bot
            };
            binding.as_ref().is_some_and(|b| b.as_str() == bot)
        })
        .map(|(id, _)| id.to_string())
        .collect();
    agents.sort();
    agents
}

fn channels_payload(config: &Config) -> Value {
    let channel = |name: &str| config.channels.get(name);
    let web = channel("web");
    let voice = channel("voice");

    let mut discord_bots: Vec<Value> = channel("discord")
        .and_then(|c| c.discord_bots.as_ref())
        .map(|bots| {
            bots.iter()
                .map(|(id, bot)| {
                    json!({
                        "id": id.as_str(),
                        "token": secret_payload(bot.token.as_ref()),
                        "agents": bound_agents(config, id.as_str(), false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    discord_bots.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut discord_channels: Vec<Value> = config
        .discord_channels()
        .into_iter()
        .map(|(id, c)| {
            json!({
                "id": id.to_string(),
                "agents": c.agents.iter().map(AgentId::as_str).collect::<Vec<_>>(),
                "require_mention": c.require_mention,
                "multi_agent": c.multi_agent,
                "secret": c.secret,
                "tool_progress": c.tool_progress,
            })
        })
        .collect();
    discord_channels.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));

    let mut telegram_bots: Vec<Value> = channel("telegram")
        .and_then(|c| c.telegram_bots.as_ref())
        .map(|bots| {
            bots.iter()
                .map(|(id, bot)| {
                    json!({
                        "id": id.as_str(),
                        "token": secret_payload(bot.token.as_ref()),
                        "agents": bound_agents(config, id.as_str(), true),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    telegram_bots.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut telegram_channels: Vec<Value> = config
        .telegram_channels()
        .into_iter()
        .map(|(id, c)| {
            json!({
                "id": id.to_string(),
                "agents": c.agents.iter().map(AgentId::as_str).collect::<Vec<_>>(),
                "require_mention": c.require_mention,
                "multi_agent": c.multi_agent,
                "secret": c.secret,
                "tool_progress": c.tool_progress,
            })
        })
        .collect();
    telegram_channels.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));

    json!({
        "web": {
            "enabled": config.web_enabled(),
            "host": config.web_host(),
            "port": config.web_port(),
            "auth_token": secret_payload(web.and_then(|c| c.auth_token.as_ref())),
            "allowed_origins": config.web_allowed_origins(),
        },
        "discord": {
            "enabled": config.channel_enabled("discord"),
            "bots": discord_bots,
            "channels": discord_channels,
        },
        "telegram": {
            "enabled": config.channel_enabled("telegram"),
            "bots": telegram_bots,
            "channels": telegram_channels,
        },
        "voice": {
            "enabled": config.voice_enabled(),
            "auth_token": secret_payload(voice.and_then(|c| c.auth_token.as_ref())),
            "default_surface": config.voice_default_surface(),
            "default_session": config.voice_default_session(),
            "allowed_surfaces": config.voice_allowed_surfaces(),
        },
    })
}

fn sleep_payload(config: &Config) -> Value {
    let sleep = &config.sleep_batch;
    let effective = config
        .resolve_sleep_batch_llm()
        .ok()
        .map(|r| json!({ "provider": r.provider, "model": r.model }));
    json!({
        "enabled": sleep.enabled,
        "schedule": sleep.schedule,
        "agents": sleep.agents.as_ref().map(|agents| agents.iter().map(AgentId::as_str).collect::<Vec<_>>()),
        "provider": sleep.provider.as_ref().map(ProviderId::as_str),
        "model": sleep.model,
        "reasoning_effort": sleep.reasoning_effort,
        "retry_max_attempts": sleep.retry_max_attempts,
        "retry_interval_minutes": sleep.retry_interval_minutes,
        "effective": effective,
    })
}

fn webhooks_payload(config: &Config) -> Vec<Value> {
    let mut receivers: Vec<Value> = config
        .webhook_receivers()
        .iter()
        .map(|(id, receiver)| {
            json!({
                "id": id.as_str(),
                "token": secret_payload(receiver.token.as_ref()),
                "channel": receiver.target.channel.as_str(),
                "thread": receiver.target.thread,
                "agent": receiver.target.agent.as_ref().map(AgentId::as_str),
            })
        })
        .collect();
    receivers.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    receivers
}

fn web_fetch_payload(config: &Config) -> Value {
    let fetch = &config.web_fetch;
    json!({
        "allowed_schemes": fetch.allowed_schemes,
        "timeout_secs": fetch.timeout_secs,
        "max_fetch_bytes": fetch.max_fetch_bytes,
        "max_output_bytes": fetch.max_output_bytes,
        "allow_private_ips": fetch.allow_private_ips,
        "denylist": fetch.denylist,
        "allowlist": fetch.allowlist,
        "content_validation": {
            "enabled": fetch.content_validation.enabled,
            "strict_mode": fetch.content_validation.strict_mode,
            "max_scan_bytes": fetch.content_validation.max_scan_bytes,
        },
    })
}

/// `scheme://host[:port]` of an endpoint. The path, query, and credentials
/// are dropped: hosted MCP servers often embed an access key in the path.
fn endpoint_origin(endpoint: &str) -> String {
    match url::Url::parse(endpoint) {
        Ok(url) => {
            let host = url.host_str().unwrap_or_default();
            let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
            format!("{}://{host}{port}", url.scheme())
        }
        Err(_) => MASK.to_string(),
    }
}

/// The program of a stdio command, without its arguments.
fn command_program(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .map(|program| {
            std::path::Path::new(program)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| program.to_string())
        })
        .unwrap_or_default()
}

fn mcp_server_payload(name: &str, server: &McpServerConfig) -> Value {
    let mut env_keys: Vec<&String> = server.env.keys().collect();
    env_keys.sort();
    let mut header_keys: Vec<&String> = server.headers.keys().collect();
    header_keys.sort();
    json!({
        "name": name,
        "transport": server.transport.to_string(),
        "command": server.command.as_deref().map(command_program),
        "endpoint": server.endpoint.as_deref().map(endpoint_origin),
        "env_keys": env_keys,
        "header_keys": header_keys,
        "request_timeout_secs": server.request_timeout_secs,
    })
}

/// Returns the MCP server definitions found in `mcp.json` / `mcp.d/`, without
/// arguments, environment or header values, or endpoint paths.
///
/// Read on request rather than with `GET /api/config`, which is polled.
///
/// # Errors
///
/// Returns `500` when the definitions could not be read.
pub(super) async fn api_get_config_mcp(State(state): State<WebState>) -> ApiResult {
    let snapshot = state.app_state.config_manager.current_blocking();
    let payload = tokio::task::spawn_blocking(move || mcp_payload(&snapshot.config))
        .await
        .map_err(|_| internal_error())?;
    Ok(Json(payload))
}

fn mcp_payload(config: &Config) -> Value {
    let Ok(workspace) = config.workspace_dir() else {
        return json!({ "ok": true, "files": [], "servers": [], "error": "workspace directory is unavailable" });
    };
    let files: Vec<String> = crate::tools::mcp::mcp_config_paths(&workspace)
        .map(|paths| paths.iter().map(|p| p.display().to_string()).collect())
        .unwrap_or_default();
    match configured_server_definitions(&workspace) {
        Ok(servers) => json!({
            "ok": true,
            "files": files,
            "servers": servers.iter().map(|(name, server)| mcp_server_payload(name, server)).collect::<Vec<_>>(),
            "error": null,
        }),
        Err(error) => {
            json!({ "ok": true, "files": files, "servers": [], "error": error.to_string() })
        }
    }
}

fn format_duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out
}

// ---------------------------------------------------------------------------
// GET /api/config/yaml
// ---------------------------------------------------------------------------

/// Returns the YAML file with every secret value masked: literal tokens and
/// API keys, `exec` commands, and all provider header values.
///
/// # Errors
///
/// Returns `500` when the file cannot be read or parsed.
pub(super) async fn api_get_config_yaml(State(state): State<WebState>) -> ApiResult {
    let path = config_path(&state)?;
    let snapshot = state.app_state.config_manager.current_blocking();
    let raw = tokio::fs::read_to_string(&path).await.map_err(|error| {
        tracing::warn!(%error, "failed to read config file");
        internal_error()
    })?;
    let content = masked_yaml(&raw).map_err(|error| {
        tracing::warn!(%error, "failed to mask config yaml");
        internal_error()
    })?;
    Ok(Json(json!({
        "ok": true,
        "path": path.display().to_string(),
        "revision": snapshot.revision,
        "fingerprint": snapshot.fingerprint,
        "content": content,
    })))
}

fn masked_yaml(raw: &str) -> Result<String, yaml_serde::Error> {
    let mut root: yaml_serde::Value = yaml_serde::from_str(raw)?;
    mask_secrets(&mut root);
    yaml_serde::to_string(&root)
}

/// Keys whose value is a secret: a literal string, or a SecretRef mapping.
const SECRET_KEYS: &[&str] = &["api_key", "auth_token", "token"];

fn mask_secret_value(value: &mut yaml_serde::Value) {
    match value {
        yaml_serde::Value::String(s) if !s.is_empty() => *s = MASK.to_string(),
        yaml_serde::Value::Mapping(map) => {
            if let Some(command) = map.get_mut("command") {
                *command = yaml_serde::Value::String(MASK.to_string());
            }
            if let Some(literal) = map.get_mut("value") {
                *literal = yaml_serde::Value::String(MASK.to_string());
            }
        }
        _ => {}
    }
}

fn mask_secrets(value: &mut yaml_serde::Value) {
    match value {
        yaml_serde::Value::Mapping(map) => {
            for (key, child) in map.iter_mut() {
                match key.as_str() {
                    Some(k) if SECRET_KEYS.contains(&k) => mask_secret_value(child),
                    Some("headers") => {
                        if let yaml_serde::Value::Mapping(headers) = child {
                            for (_, header) in headers.iter_mut() {
                                *header = yaml_serde::Value::String(MASK.to_string());
                            }
                        }
                    }
                    _ => mask_secrets(child),
                }
            }
        }
        yaml_serde::Value::Sequence(items) => items.iter_mut().for_each(mask_secrets),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// PATCH endpoints
// ---------------------------------------------------------------------------

/// Distinguishes an absent field (`None`) from an explicit `null`
/// (`Some(None)`).
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Trims an optional string; blank means unset.
fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[derive(Deserialize)]
pub(super) struct GeneralPatch {
    expected_fingerprint: String,
    default_agent: Option<String>,
    default_provider: Option<String>,
    #[serde(default, deserialize_with = "present")]
    default_model: Option<Option<String>>,
    timezone: Option<String>,
    max_history_messages: Option<usize>,
    compaction_timeout_secs: Option<u64>,
    default_context_window_tokens: Option<usize>,
    compaction_threshold_ratio: Option<f64>,
    compaction_target_ratio: Option<f64>,
    compact_keep_recent: Option<usize>,
}

/// Updates global defaults and conversation tuning.
///
/// # Errors
///
/// `409` on a fingerprint mismatch and `400` when the result is invalid.
pub(super) async fn api_patch_general(
    State(state): State<WebState>,
    Json(patch): Json<GeneralPatch>,
) -> ApiResult {
    apply(&state, &patch.expected_fingerprint, |config| {
        if let Some(agent) = patch.default_agent {
            config.default_agent = AgentId::new(&agent);
        }
        if let Some(provider) = patch.default_provider {
            config.default_provider = ProviderId::new(&provider);
        }
        if let Some(model) = patch.default_model {
            config.default_model = clean(model);
        }
        if let Some(timezone) = patch.timezone {
            config.timezone = timezone.trim().to_string();
        }
        if let Some(v) = patch.max_history_messages {
            config.max_history_messages = v;
        }
        if let Some(v) = patch.compaction_timeout_secs {
            config.compaction_timeout_secs = v;
        }
        if let Some(v) = patch.default_context_window_tokens {
            config.default_context_window_tokens = v;
        }
        if let Some(v) = patch.compaction_threshold_ratio {
            config.compaction_threshold_ratio = v;
        }
        if let Some(v) = patch.compaction_target_ratio {
            config.compaction_target_ratio = v;
        }
        if let Some(v) = patch.compact_keep_recent {
            config.compact_keep_recent = v;
        }
        Ok(Vec::new())
    })
}

#[derive(Deserialize)]
pub(super) struct AgentPatch {
    expected_fingerprint: String,
    label: Option<String>,
    #[serde(default, deserialize_with = "present")]
    provider: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    model: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    reasoning_effort: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    discord_bot: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    telegram_bot: Option<Option<String>>,
}

/// Updates one agent's model, behavior, and bot bindings.
///
/// # Errors
///
/// `404` for an unknown agent, `409` on a fingerprint mismatch, and `400` when
/// the result is invalid.
pub(super) async fn api_patch_agent(
    State(state): State<WebState>,
    Path(agent_id): Path<String>,
    Json(patch): Json<AgentPatch>,
) -> ApiResult {
    let id = AgentId::new(&agent_id);
    apply(&state, &patch.expected_fingerprint, |config| {
        let agent = config
            .agents
            .get_mut(&id)
            .ok_or_else(|| not_found("agent_not_found"))?;
        if let Some(label) = clean(patch.label) {
            agent.label = label;
        }
        if let Some(provider) = patch.provider {
            agent.provider = clean(provider).map(|p| p.to_ascii_lowercase());
        }
        if let Some(model) = patch.model {
            agent.model = clean(model);
        }
        if let Some(effort) = patch.reasoning_effort {
            agent.reasoning_effort = clean(effort);
        }
        if let Some(bot) = patch.discord_bot {
            agent.discord_bot = clean(bot).map(|b| BotId::new(&b));
        }
        if let Some(bot) = patch.telegram_bot {
            agent.telegram_bot = clean(bot).map(|b| BotId::new(&b));
        }
        Ok(Vec::new())
    })
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(super) enum ApiKeyChange {
    Replace { value: String },
    Remove,
}

#[derive(Deserialize)]
pub(super) struct ProviderPatch {
    expected_fingerprint: String,
    label: Option<String>,
    base_url: Option<String>,
    default_model: Option<String>,
    models: Option<Vec<String>>,
    api_key: Option<ApiKeyChange>,
}

/// Updates one provider's endpoint, models, and API key. A replaced key is
/// written to `.env` and referenced from the YAML. Keys currently supplied by
/// the process environment are read-only here, because writing them to `.env`
/// would not change the effective value.
///
/// # Errors
///
/// `404` for an unknown provider, `409` on a fingerprint mismatch, and `400`
/// when the result is invalid or a process-managed key is changed.
pub(super) async fn api_patch_provider(
    State(state): State<WebState>,
    Path(provider_id): Path<String>,
    Json(patch): Json<ProviderPatch>,
) -> ApiResult {
    let id = ProviderId::new(&provider_id);
    apply(&state, &patch.expected_fingerprint, |config| {
        let provider = config
            .providers
            .get_mut(&id)
            .ok_or_else(|| not_found("provider_not_found"))?;
        if let Some(label) = clean(patch.label) {
            provider.label = label;
        }
        if let Some(base_url) = patch.base_url {
            provider.base_url = base_url.trim().to_string();
        }
        if let Some(models) = patch.models {
            let previous = std::mem::take(&mut provider.models);
            provider.models = models
                .into_iter()
                .filter_map(|name| clean(Some(name)))
                .map(|name| {
                    let model = previous.get(&name).cloned().unwrap_or_default();
                    (name, model)
                })
                .collect();
        }
        if let Some(model) = patch.default_model {
            provider.default_model = model.trim().to_string();
        }
        if !provider.default_model.is_empty() {
            provider
                .models
                .entry(provider.default_model.clone())
                .or_insert_with(ModelConfig::default);
        }
        let process_managed = matches!(
            &provider.api_key,
            Some(ResolvedValue::EnvRef {
                origin: EnvOrigin::Process,
                ..
            })
        );
        let mut removed_dotenv_keys = Vec::new();
        match patch.api_key {
            Some(ApiKeyChange::Replace { value }) => {
                if process_managed {
                    return Err(bad_request("api_key_managed_by_process_env"));
                }
                let value = value.trim();
                if value.is_empty() {
                    return Err(bad_request("empty_api_key"));
                }
                let env_name = match &provider.api_key {
                    Some(ResolvedValue::EnvRef {
                        id,
                        origin: EnvOrigin::Dotenv,
                        ..
                    }) => {
                        if env_is_process_override(id) {
                            return Err(bad_request("api_key_managed_by_process_env"));
                        }
                        id.clone()
                    }
                    _ => provider_api_key_env_name_for_write(id.as_str())?,
                };
                provider.api_key = Some(env_resolved_value(env_name, value));
            }
            Some(ApiKeyChange::Remove) => {
                if process_managed {
                    return Err(bad_request("api_key_managed_by_process_env"));
                }
                if let Some(ResolvedValue::EnvRef {
                    id,
                    origin: EnvOrigin::Dotenv,
                    ..
                }) = provider.api_key.take()
                {
                    removed_dotenv_keys.push(id);
                }
            }
            None => {}
        }
        Ok(removed_dotenv_keys)
    })
}

#[derive(Deserialize)]
pub(super) struct SleepPatch {
    expected_fingerprint: String,
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    agents: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "present")]
    provider: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    model: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    reasoning_effort: Option<Option<String>>,
    retry_max_attempts: Option<u32>,
    retry_interval_minutes: Option<u32>,
}

/// Updates the Sleep batch scheduler and its model.
///
/// # Errors
///
/// `409` on a fingerprint mismatch and `400` when the result is invalid.
pub(super) async fn api_patch_sleep(
    State(state): State<WebState>,
    Json(patch): Json<SleepPatch>,
) -> ApiResult {
    apply(&state, &patch.expected_fingerprint, |config| {
        let sleep = &mut config.sleep_batch;
        if let Some(enabled) = patch.enabled {
            sleep.enabled = enabled;
        }
        if let Some(schedule) = patch.schedule {
            sleep.schedule = clean(schedule);
        }
        if let Some(agents) = patch.agents {
            sleep.agents =
                agents.map(|agents| agents.iter().map(|a| AgentId::new(a)).collect::<Vec<_>>());
        }
        if let Some(provider) = patch.provider {
            sleep.provider = clean(provider).map(|p| ProviderId::new(&p));
        }
        if let Some(model) = patch.model {
            sleep.model = clean(model);
        }
        if let Some(effort) = patch.reasoning_effort {
            sleep.reasoning_effort = clean(effort);
        }
        if let Some(v) = patch.retry_max_attempts {
            sleep.retry_max_attempts = v;
        }
        if let Some(v) = patch.retry_interval_minutes {
            sleep.retry_interval_minutes = v;
        }
        Ok(Vec::new())
    })
}

#[derive(Deserialize)]
pub(super) struct PulsePatch {
    expected_fingerprint: String,
    tick_interval: Option<String>,
}

/// Updates the Pulse scan interval.
///
/// # Errors
///
/// `409` on a fingerprint mismatch and `400` for an invalid duration.
pub(super) async fn api_patch_pulse(
    State(state): State<WebState>,
    Json(patch): Json<PulsePatch>,
) -> ApiResult {
    apply(&state, &patch.expected_fingerprint, |config| {
        if let Some(interval) = patch.tick_interval {
            config.pulse.tick_interval_secs =
                parse_duration(&interval).map_err(|_| bad_request("invalid_tick_interval"))?;
        }
        Ok(Vec::new())
    })
}

/// Copies the current config, lets `edit` change it, and applies the result
/// through the single update boundary.
fn apply(
    state: &WebState,
    expected_fingerprint: &str,
    edit: impl FnOnce(&mut Config) -> Result<Vec<String>, ApiError>,
) -> ApiResult {
    let path = config_path(state)?;
    let manager = &state.app_state.config_manager;
    let mut candidate = manager.current_blocking().config.clone();
    let removed_dotenv_keys = edit(&mut candidate)?;
    let updated = manager
        .apply_candidate_with_removed_dotenv_keys(
            candidate,
            Some(expected_fingerprint),
            &removed_dotenv_keys,
        )
        .map_err(map_update_error)?;
    Ok(Json(config_payload(&updated, &path)))
}

/// Whether the process environment currently supplies a non-empty value for
/// `name`; such a value wins over the same key in `.env`.
fn env_is_process_override(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

/// Picks an env name whose `.env` value will actually take effect. The default
/// name is used unless the process environment already overrides it; in that
/// case the Egopulse-prefixed name is used instead.
fn provider_api_key_env_name_for_write(provider_id: &str) -> Result<String, ApiError> {
    let base = provider_api_key_env_name(provider_id);
    if !env_is_process_override(&base) {
        return Ok(base);
    }
    let fallback = format!("EGOPULSE_{base}");
    if !env_is_process_override(&fallback) {
        return Ok(fallback);
    }
    Err(bad_request("api_key_env_override_conflict"))
}

fn map_update_error(error: EgoPulseError) -> ApiError {
    match &error {
        EgoPulseError::Config(ConfigError::ConfigConflict { .. }) => {
            api_error(StatusCode::CONFLICT, "config_conflict")
        }
        EgoPulseError::Config(ConfigError::ConfigReloadForbidden { .. }) => (
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "ok": false, "error": "config_reload_forbidden", "message": error.to_string() }),
            ),
        ),
        EgoPulseError::Config(
            ConfigError::ConfigNotFound { .. }
            | ConfigError::AutoConfigNotFound { .. }
            | ConfigError::ConfigReadFailed { .. }
            | ConfigError::ConfigPathUnavailable,
        ) => {
            tracing::warn!(%error, "config update failed");
            internal_error()
        }
        EgoPulseError::Config(_) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "invalid_config", "message": error.to_string() })),
        ),
        _ => {
            tracing::warn!(%error, "config update failed");
            internal_error()
        }
    }
}

fn config_path(state: &WebState) -> Result<PathBuf, ApiError> {
    match &state.config_path {
        Some(path) => Ok(path.clone()),
        None => default_config_path().map_err(|_| internal_error()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvVarGuard;
    use serial_test::serial;

    #[test]
    fn secret_payload_reports_env_origin() {
        let process = ResolvedValue::EnvRef {
            value: "secret".into(),
            id: "PROCESS_KEY".into(),
            origin: EnvOrigin::Process,
        };
        let dotenv = ResolvedValue::EnvRef {
            value: "secret".into(),
            id: "DOTENV_KEY".into(),
            origin: EnvOrigin::Dotenv,
        };

        assert_eq!(
            secret_payload(Some(&process))["origin"].as_str(),
            Some("process")
        );
        assert_eq!(
            secret_payload(Some(&dotenv))["origin"].as_str(),
            Some("dotenv")
        );
    }

    #[test]
    #[serial]
    fn provider_key_env_name_uses_prefixed_name_when_default_is_overridden() {
        let _guard = EnvVarGuard::set("REVIEW_FALLBACK_PROVIDER_API_KEY", "from-process");

        let name = provider_api_key_env_name_for_write("review-fallback-provider").expect("name");

        assert_eq!(name, "EGOPULSE_REVIEW_FALLBACK_PROVIDER_API_KEY");
    }

    #[test]
    #[serial]
    fn provider_key_env_name_rejects_when_both_names_are_overridden() {
        let _guard = EnvVarGuard::set("REVIEW_FALLBACK_PROVIDER_API_KEY", "from-process").also_set(
            "EGOPULSE_REVIEW_FALLBACK_PROVIDER_API_KEY",
            "also-from-process",
        );

        let error = provider_api_key_env_name_for_write("review-fallback-provider")
            .expect_err("both names are overridden");

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.1.0["error"].as_str(),
            Some("api_key_env_override_conflict")
        );
    }

    #[test]
    fn masked_yaml_hides_literals_exec_commands_and_header_values() {
        let raw = r#"
providers:
  openrouter:
    api_key: sk-live-literal
    headers:
      authorization: Bearer abc
  local:
    api_key:
      source: env
      id: LOCAL_API_KEY
channels:
  discord:
    bots:
      main:
        token:
          source: exec
          command: pass show discord-token-xyz
webhooks:
  receivers:
    gh:
      token: hook-secret
"#;

        let masked = masked_yaml(raw).expect("mask");

        for secret in [
            "sk-live-literal",
            "Bearer abc",
            "discord-token-xyz",
            "hook-secret",
        ] {
            assert!(!masked.contains(secret), "{secret} leaked:\n{masked}");
        }
        assert!(masked.contains("LOCAL_API_KEY"));
        assert!(masked.contains("source: exec"));
    }

    #[test]
    fn mcp_payload_drops_args_paths_queries_and_values() {
        let server: McpServerConfig = serde_json::from_value(json!({
            "transport": "streamable_http",
            "endpoint": "https://user:pw@mcp.example.com:8443/s/path-key/mcp?key=secret",
            "headers": { "Authorization": "Bearer t" },
        }))
        .expect("server");

        let payload = mcp_server_payload("remote", &server).to_string();

        assert!(payload.contains("\"https://mcp.example.com:8443\""));
        for secret in ["secret", "pw", "path-key", "Bearer t"] {
            assert!(!payload.contains(secret), "{secret} leaked: {payload}");
        }
    }

    #[test]
    fn command_program_keeps_only_the_executable_name() {
        assert_eq!(command_program("/usr/bin/npx -y server --token abc"), "npx");
    }

    #[test]
    fn present_distinguishes_null_from_absent() {
        let absent: AgentPatch =
            serde_json::from_value(json!({ "expected_fingerprint": "f" })).expect("absent");
        let null: AgentPatch =
            serde_json::from_value(json!({ "expected_fingerprint": "f", "model": null }))
                .expect("null");

        assert!(absent.model.is_none());
        assert_eq!(null.model, Some(None));
    }
}

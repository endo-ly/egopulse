use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::agent_loop::{ConversationScope, SurfaceContext};
use crate::channels::web::WebState;
use crate::channels::web::auth::constant_time_eq;
use crate::config::{Config, WebhookReceiverId};
use crate::runtime::channel_scope_from_secret;

pub(super) const MAX_WEBHOOK_PAYLOAD_BYTES: usize = 64 * 1024;

pub(crate) async fn receive_webhook(
    State(state): State<WebState>,
    Path(raw_receiver_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let receiver_id = WebhookReceiverId::new(&raw_receiver_id);

    let Some(receiver) = state.app_state.config.webhook_receivers().get(&receiver_id) else {
        return super::error::webhook_error(
            StatusCode::NOT_FOUND,
            "webhook_receiver_not_found",
            "receiver is not configured",
        );
    };

    let Some(expected_token) = receiver.token.as_ref().map(|rv| rv.value()) else {
        return super::error::webhook_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "webhook_not_configured",
            "receiver token is not configured",
        );
    };

    let Some(token) = extract_bearer_token(&headers) else {
        return super::error::webhook_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or malformed authorization header",
        );
    };

    if !constant_time_eq(token, expected_token) {
        return super::error::webhook_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid receiver token",
        );
    }

    if body.len() > MAX_WEBHOOK_PAYLOAD_BYTES {
        return super::error::webhook_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "payload exceeds 64KB limit",
        );
    }

    let target_channel = receiver.target.channel.as_str();
    if target_channel == "voice" || state.app_state.channels.get(target_channel).is_none() {
        return super::error::webhook_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "target channel is not active or is voice",
        );
    }

    let agent_id = receiver
        .target
        .agent
        .as_ref()
        .unwrap_or(&state.app_state.config.default_agent);
    if !state.app_state.config.agents.contains_key(agent_id) {
        return super::error::webhook_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "target agent is not configured",
        );
    }

    if target_channel != "web" && receiver.target.thread.trim().is_empty() {
        return super::error::webhook_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "target thread is required for non-web channels",
        );
    }

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return super::error::webhook_error(
                StatusCode::BAD_REQUEST,
                "invalid_params",
                "invalid JSON payload",
            );
        }
    };

    let input = super::formatter::format_webhook_payload(&receiver_id.to_string(), &payload);

    let context = build_webhook_context(&state.app_state.config, &receiver_id, receiver);

    crate::runtime::channel_input::submit_agent_turn(&state.app_state, context, input);

    (
        StatusCode::ACCEPTED,
        axum::Json(json!({
            "ok": true,
            "receiver": receiver_id.to_string(),
            "status": "accepted",
        })),
    )
        .into_response()
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn resolve_target_scope(config: &Config, channel: &str, thread: &str) -> ConversationScope {
    match channel {
        "discord" => config
            .channels
            .get("discord")
            .and_then(|ch| ch.discord_channels.as_ref())
            .and_then(|channels| thread.parse::<u64>().ok().and_then(|id| channels.get(&id)))
            .map(|c| channel_scope_from_secret(c.secret))
            .unwrap_or(ConversationScope::Normal),
        "telegram" => config
            .channels
            .get("telegram")
            .and_then(|ch| ch.telegram_channels.as_ref())
            .and_then(|channels| thread.parse::<i64>().ok().and_then(|id| channels.get(&id)))
            .map(|c| channel_scope_from_secret(c.secret))
            .unwrap_or(ConversationScope::Normal),
        _ => ConversationScope::Normal,
    }
}

fn build_webhook_context(
    config: &Config,
    receiver_id: &WebhookReceiverId,
    receiver: &crate::config::WebhookReceiverConfig,
) -> SurfaceContext {
    let target_channel = receiver.target.channel.as_str();
    let agent_id = receiver
        .target
        .agent
        .as_ref()
        .unwrap_or(&config.default_agent);

    let surface_thread = if target_channel == "web" {
        crate::channels::web::web_session_key(&receiver.target.thread)
    } else {
        receiver.target.thread.trim().to_string()
    };

    let mut context = SurfaceContext::new(
        target_channel.to_string(),
        format!("webhook:{receiver_id}"),
        surface_thread,
        target_channel.to_string(),
        agent_id.to_string(),
    );
    context.origin_id = uuid::Uuid::new_v4().to_string();
    context.scope = resolve_target_scope(config, target_channel, &receiver.target.thread);
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::ConversationScope;
    use crate::config::{
        AgentId, ChannelConfig, ChannelName, Config, DiscordChannelConfig, ProviderConfig,
        ProviderId, WebhookReceiverConfig, WebhookReceiverId, WebhookTargetConfig,
    };

    fn test_config_with_discord_secret(thread: &str, secret: bool) -> Config {
        let channel_id: u64 = thread.parse().unwrap_or(0);
        Config {
            default_provider: ProviderId::new("openai"),
            default_model: None,
            providers: std::collections::HashMap::from([(
                ProviderId::new("openai"),
                ProviderConfig {
                    label: "OpenAI".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key: None,
                    default_model: "gpt-4o-mini".to_string(),
                    models: std::collections::HashMap::new(),
                },
            )]),
            state_root: "/tmp/test".to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            compact_keep_recent: 20,
            default_context_window_tokens: 32768,
            compaction_threshold_ratio: 0.8,
            compaction_target_ratio: 0.4,
            channels: std::collections::HashMap::from([(
                ChannelName::new("discord"),
                ChannelConfig {
                    enabled: Some(true),
                    discord_channels: Some(std::collections::HashMap::from([(
                        channel_id,
                        DiscordChannelConfig {
                            secret,
                            ..Default::default()
                        },
                    )])),
                    ..Default::default()
                },
            )]),
            default_agent: AgentId::new("default"),
            agents: std::collections::HashMap::from([(
                AgentId::new("default"),
                crate::config::AgentConfig {
                    label: "Default".to_string(),
                    ..Default::default()
                },
            )]),
            timezone: "UTC".to_string(),
            sleep_batch: crate::config::SleepBatchConfig::default(),
            pulse: crate::config::PulseConfig::default(),
            db: crate::config::DatabaseConfig::default(),
            web_fetch: crate::config::web_fetch::WebFetchConfig::default(),
            webhooks: crate::config::WebhooksConfig::default(),
        }
    }

    #[test]
    fn webhook_context_uses_target_channel_and_receiver_surface_user() {
        let config = test_config_with_discord_secret("123", false);
        let receiver = WebhookReceiverConfig {
            token: None,
            file_token: None,
            target: WebhookTargetConfig {
                channel: ChannelName::new("discord"),
                thread: "123".to_string(),
                agent: Some(AgentId::new("default")),
            },
        };
        let receiver_id = WebhookReceiverId::new("egograph");

        let context = build_webhook_context(&config, &receiver_id, &receiver);

        assert_eq!(context.channel, "discord");
        assert_eq!(context.surface_user, "webhook:egograph");
        assert_eq!(context.surface_thread, "123");
        assert_eq!(context.chat_type, "discord");
        assert_eq!(context.agent_id, "default");
        assert!(!context.origin_id.is_empty());
    }

    #[test]
    fn webhook_context_uses_secret_scope_for_secret_discord_or_telegram_target() {
        let secret_config = test_config_with_discord_secret("123", true);
        let receiver = WebhookReceiverConfig {
            token: None,
            file_token: None,
            target: WebhookTargetConfig {
                channel: ChannelName::new("discord"),
                thread: "123".to_string(),
                agent: Some(AgentId::new("default")),
            },
        };
        let receiver_id = WebhookReceiverId::new("egograph");

        let secret_context = build_webhook_context(&secret_config, &receiver_id, &receiver);
        assert_eq!(secret_context.scope, ConversationScope::Secret);

        let normal_config = test_config_with_discord_secret("123", false);
        let normal_context = build_webhook_context(&normal_config, &receiver_id, &receiver);
        assert_eq!(normal_context.scope, ConversationScope::Normal);
    }

    #[test]
    fn webhook_web_target_normalizes_thread_like_web_session_key() {
        let config = test_config_with_discord_secret("123", false);
        let receiver_id = WebhookReceiverId::new("egograph");

        for (input, expected) in [
            ("web:main", "main"),
            ("web:   ", "main"),
            ("", "main"),
            ("custom", "custom"),
            ("  web:foo  ", "foo"),
        ] {
            let receiver = WebhookReceiverConfig {
                token: None,
                file_token: None,
                target: WebhookTargetConfig {
                    channel: ChannelName::new("web"),
                    thread: input.to_string(),
                    agent: Some(AgentId::new("default")),
                },
            };
            let context = build_webhook_context(&config, &receiver_id, &receiver);
            assert_eq!(
                context.surface_thread, expected,
                "web target thread '{input}' should normalize to '{expected}'"
            );
        }
    }
}

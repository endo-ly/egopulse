use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rand::Rng;

use super::Field;

pub(crate) fn update_field_visibility(fields: &mut [Field]) {
    let discord_enabled = fields
        .iter()
        .find(|f| f.key == "DISCORD_ENABLED")
        .map(|f| super::parse_bool(&f.value).unwrap_or(false))
        .unwrap_or(false);

    let telegram_enabled = fields
        .iter()
        .find(|f| f.key == "TELEGRAM_ENABLED")
        .map(|f| super::parse_bool(&f.value).unwrap_or(false))
        .unwrap_or(false);

    for field in fields.iter_mut() {
        match field.key.as_str() {
            "DISCORD_BOT_TOKEN" => {
                field.required = discord_enabled;
            }
            "TELEGRAM_BOT_TOKEN" => {
                field.required = telegram_enabled;
            }
            _ => {}
        }
    }
}

pub(crate) fn load_channel_fields(
    channels: &serde_yml::Value,
    result: &mut HashMap<String, String>,
) {
    let Some(ch_map) = channels.as_mapping() else {
        return;
    };

    insert_channel_string(ch_map, "web", "auth_token", result, "WEB_AUTH_TOKEN");
    insert_channel_bool(ch_map, "discord", "enabled", result, "DISCORD_ENABLED");
    insert_channel_string(ch_map, "discord", "bot_token", result, "DISCORD_BOT_TOKEN");
    insert_channel_bool(ch_map, "telegram", "enabled", result, "TELEGRAM_ENABLED");
    insert_channel_string(
        ch_map,
        "telegram",
        "bot_token",
        result,
        "TELEGRAM_BOT_TOKEN",
    );
    insert_channel_string(
        ch_map,
        "telegram",
        "bot_username",
        result,
        "TELEGRAM_BOT_USERNAME",
    );
}

pub(crate) fn extract_existing_web_auth_token(
    original_yaml: &Option<serde_yml::Value>,
) -> Option<String> {
    original_yaml
        .as_ref()
        .and_then(|v| v.as_mapping())
        .and_then(|m| m.get(serde_yml::Value::String("channels".into())))
        .and_then(|c| c.as_mapping())
        .and_then(|m| m.get(serde_yml::Value::String("web".into())))
        .and_then(|w| w.as_mapping())
        .and_then(|m| m.get(serde_yml::Value::String("auth_token".into())))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn extract_existing_state_root(
    original_yaml: &Option<serde_yml::Value>,
) -> Option<String> {
    original_yaml
        .as_ref()
        .and_then(|v| v.as_mapping())
        .and_then(|m| m.get(serde_yml::Value::String("state_root".into())))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn build_channel_configs(
    auth_token: String,
    discord_enabled: bool,
    discord_bot_token: String,
    telegram_enabled: bool,
    telegram_bot_token: String,
    telegram_bot_username: String,
) -> HashMap<crate::config::ChannelName, crate::config::ChannelConfig> {
    use crate::config::{ChannelConfig, ChannelName};

    let mut channels = HashMap::new();

    channels.insert(
        ChannelName::new("web"),
        ChannelConfig {
            enabled: Some(true),
            host: Some("127.0.0.1".to_string()),
            port: Some(10961),
            auth_token: Some(auth_token),
            ..Default::default()
        },
    );

    if discord_enabled {
        channels.insert(
            ChannelName::new("discord"),
            ChannelConfig {
                enabled: Some(true),
                bot_token: Some(discord_bot_token),
                ..Default::default()
            },
        );
    }

    if telegram_enabled {
        channels.insert(
            ChannelName::new("telegram"),
            ChannelConfig {
                enabled: Some(true),
                bot_token: Some(telegram_bot_token),
                bot_username: (!telegram_bot_username.is_empty()).then_some(telegram_bot_username),
                ..Default::default()
            },
        );
    }

    channels
}

pub(crate) fn generate_auth_token() -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random::<u8>()).collect();
    STANDARD.encode(&bytes)
}

fn yaml_key(value: &str) -> serde_yml::Value {
    serde_yml::Value::String(value.into())
}

fn channel_mapping<'a>(
    channels: &'a serde_yml::Mapping,
    channel: &str,
) -> Option<&'a serde_yml::Mapping> {
    channels.get(yaml_key(channel))?.as_mapping()
}

fn insert_channel_bool(
    channels: &serde_yml::Mapping,
    channel: &str,
    key: &str,
    result: &mut HashMap<String, String>,
    result_key: &str,
) {
    let Some(value) = channel_mapping(channels, channel)
        .and_then(|mapping| mapping.get(yaml_key(key)))
        .and_then(|value| value.as_bool())
    else {
        return;
    };

    result.insert(result_key.into(), value.to_string());
}

fn insert_channel_string(
    channels: &serde_yml::Mapping,
    channel: &str,
    key: &str,
    result: &mut HashMap<String, String>,
    result_key: &str,
) {
    let Some(value) = channel_mapping(channels, channel)
        .and_then(|mapping| mapping.get(yaml_key(key)))
        .and_then(|value| value.as_str())
    else {
        return;
    };

    result.insert(result_key.into(), value.to_string());
}

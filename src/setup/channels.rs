use std::collections::HashMap;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
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

    if let Some(web) = ch_map.get(serde_yml::Value::String("web".into())) {
        if let Some(web_map) = web.as_mapping() {
            if let Some(token) = web_map.get(serde_yml::Value::String("auth_token".into())) {
                if let Some(token_str) = token.as_str() {
                    result.insert("WEB_AUTH_TOKEN".into(), token_str.to_string());
                }
            }
        }
    }

    if let Some(discord) = ch_map.get(serde_yml::Value::String("discord".into())) {
        if let Some(d_map) = discord.as_mapping() {
            if let Some(enabled) = d_map.get(serde_yml::Value::String("enabled".into())) {
                if let Some(b) = enabled.as_bool() {
                    result.insert("DISCORD_ENABLED".into(), b.to_string());
                }
            }
            if let Some(token) = d_map.get(serde_yml::Value::String("bot_token".into())) {
                if let Some(t) = token.as_str() {
                    result.insert("DISCORD_BOT_TOKEN".into(), t.to_string());
                }
            }
        }
    }

    if let Some(tg) = ch_map.get(serde_yml::Value::String("telegram".into())) {
        if let Some(tg_map) = tg.as_mapping() {
            if let Some(enabled) = tg_map.get(serde_yml::Value::String("enabled".into())) {
                if let Some(b) = enabled.as_bool() {
                    result.insert("TELEGRAM_ENABLED".into(), b.to_string());
                }
            }
            if let Some(token) = tg_map.get(serde_yml::Value::String("bot_token".into())) {
                if let Some(t) = token.as_str() {
                    result.insert("TELEGRAM_BOT_TOKEN".into(), t.to_string());
                }
            }
            if let Some(username) = tg_map.get(serde_yml::Value::String("bot_username".into())) {
                if let Some(u) = username.as_str() {
                    result.insert("TELEGRAM_BOT_USERNAME".into(), u.to_string());
                }
            }
        }
    }
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

pub(crate) fn build_channel_configs(
    auth_token: String,
    discord_enabled: bool,
    discord_bot_token: String,
    telegram_enabled: bool,
    telegram_bot_token: String,
    telegram_bot_username: String,
) -> HashMap<String, crate::config::ChannelConfig> {
    use crate::config::ChannelConfig;

    let mut channels = HashMap::new();

    channels.insert(
        "web".to_string(),
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
            "discord".to_string(),
            ChannelConfig {
                enabled: Some(true),
                bot_token: Some(discord_bot_token),
                ..Default::default()
            },
        );
    }

    if telegram_enabled {
        let bot_username = if telegram_bot_username.is_empty() {
            None
        } else {
            Some(telegram_bot_username)
        };
        channels.insert(
            "telegram".to_string(),
            ChannelConfig {
                enabled: Some(true),
                bot_token: Some(telegram_bot_token),
                bot_username,
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

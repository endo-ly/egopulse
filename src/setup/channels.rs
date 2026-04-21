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
    use crate::config::secret_ref::{
        DISCORD_BOT_TOKEN_ENV_NAME, TELEGRAM_BOT_TOKEN_ENV_NAME, WEB_AUTH_TOKEN_ENV_NAME,
        env_resolved_value, env_yaml_value,
    };
    use crate::config::{ChannelConfig, ChannelName};

    let mut channels = HashMap::new();

    channels.insert(
        ChannelName::new("web"),
        ChannelConfig {
            enabled: Some(true),
            host: Some("127.0.0.1".to_string()),
            port: Some(10961),
            auth_token: Some(env_resolved_value(WEB_AUTH_TOKEN_ENV_NAME, auth_token)),
            file_auth_token: Some(env_yaml_value(WEB_AUTH_TOKEN_ENV_NAME)),
            ..Default::default()
        },
    );

    if discord_enabled {
        channels.insert(
            ChannelName::new("discord"),
            ChannelConfig {
                enabled: Some(true),
                bot_token: Some(env_resolved_value(
                    DISCORD_BOT_TOKEN_ENV_NAME,
                    discord_bot_token,
                )),
                file_bot_token: Some(env_yaml_value(DISCORD_BOT_TOKEN_ENV_NAME)),
                ..Default::default()
            },
        );
    }

    if telegram_enabled {
        channels.insert(
            ChannelName::new("telegram"),
            ChannelConfig {
                enabled: Some(true),
                bot_token: Some(env_resolved_value(
                    TELEGRAM_BOT_TOKEN_ENV_NAME,
                    telegram_bot_token,
                )),
                file_bot_token: Some(env_yaml_value(TELEGRAM_BOT_TOKEN_ENV_NAME)),
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

#[cfg(test)]
mod tests {
    use super::build_channel_configs;

    #[test]
    fn build_channel_configs_stores_channel_secrets_as_env_refs() {
        let channels = build_channel_configs(
            "web-token".to_string(),
            true,
            "discord-token".to_string(),
            true,
            "telegram-token".to_string(),
            "botname".to_string(),
        );

        let web = channels.get("web").expect("web");
        let web_file = serde_yml::to_string(web.file_auth_token.as_ref().expect("web file"))
            .expect("serialize web file");
        assert!(web_file.contains("source: env"));
        assert!(web_file.contains("id: WEB_AUTH_TOKEN"));

        let discord = channels.get("discord").expect("discord");
        let discord_file =
            serde_yml::to_string(discord.file_bot_token.as_ref().expect("discord file"))
                .expect("serialize discord file");
        assert!(discord_file.contains("id: DISCORD_BOT_TOKEN"));

        let telegram = channels.get("telegram").expect("telegram");
        let telegram_file =
            serde_yml::to_string(telegram.file_bot_token.as_ref().expect("telegram file"))
                .expect("serialize telegram file");
        assert!(telegram_file.contains("id: TELEGRAM_BOT_TOKEN"));
    }
}

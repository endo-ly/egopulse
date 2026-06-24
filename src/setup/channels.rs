use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rand::RngExt;

pub(crate) fn load_channel_fields(
    channels: &yaml_serde::Value,
    result: &mut HashMap<String, String>,
) {
    let Some(ch_map) = channels.as_mapping() else {
        return;
    };

    insert_channel_string(ch_map, "web", "auth_token", result, "WEB_AUTH_TOKEN");
    insert_channel_bool(ch_map, "discord", "enabled", result, "DISCORD_ENABLED");
    insert_channel_bool(ch_map, "telegram", "enabled", result, "TELEGRAM_ENABLED");

    insert_telegram_bot_field(ch_map, "token", result, "TELEGRAM_BOT_TOKEN");
}

pub(crate) fn extract_existing_state_root(
    original_yaml: &Option<yaml_serde::Value>,
) -> Option<String> {
    original_yaml
        .as_ref()
        .and_then(|v| v.as_mapping())
        .and_then(|m| m.get(yaml_serde::Value::String("state_root".into())))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn build_channel_configs(
    web_enabled: bool,
    auth_token: String,
    discord_enabled: bool,
    telegram_enabled: bool,
    telegram_bot_token: String,
) -> HashMap<crate::config::ChannelName, crate::config::ChannelConfig> {
    use crate::config::secret_ref::{
        TELEGRAM_BOT_TOKEN_ENV_NAME, WEB_AUTH_TOKEN_ENV_NAME, env_resolved_value, env_yaml_value,
    };
    use crate::config::{BotId, ChannelConfig, ChannelName, TelegramBotConfig};

    let mut channels = HashMap::new();

    if web_enabled {
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
    }

    if discord_enabled {
        channels.insert(
            ChannelName::new("discord"),
            ChannelConfig {
                enabled: Some(true),
                ..Default::default()
            },
        );
    }

    if telegram_enabled {
        let mut bots = HashMap::new();
        bots.insert(
            BotId::new("default"),
            TelegramBotConfig {
                token: Some(env_resolved_value(
                    TELEGRAM_BOT_TOKEN_ENV_NAME,
                    telegram_bot_token,
                )),
                file_token: Some(env_yaml_value(TELEGRAM_BOT_TOKEN_ENV_NAME)),
            },
        );
        channels.insert(
            ChannelName::new("telegram"),
            ChannelConfig {
                enabled: Some(true),
                telegram_bots: Some(bots),
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

fn yaml_key(value: &str) -> yaml_serde::Value {
    yaml_serde::Value::String(value.into())
}

fn channel_mapping<'a>(
    channels: &'a yaml_serde::Mapping,
    channel: &str,
) -> Option<&'a yaml_serde::Mapping> {
    channels.get(yaml_key(channel))?.as_mapping()
}

fn insert_channel_bool(
    channels: &yaml_serde::Mapping,
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

/// Try to read a field from `channels.telegram.telegram_bots.default.<field>`.
/// Returns `true` if the value was found and inserted.
fn insert_telegram_bot_field(
    channels: &yaml_serde::Mapping,
    field: &str,
    result: &mut HashMap<String, String>,
    result_key: &str,
) -> bool {
    let Some(value) = channel_mapping(channels, "telegram")
        .and_then(|tg| tg.get(yaml_key("bots")))
        .and_then(|bots| bots.as_mapping())
        .and_then(|bots_map| bots_map.get(yaml_key("default")))
        .and_then(|bot| bot.as_mapping())
        .and_then(|bot_map| bot_map.get(yaml_key(field)))
        .and_then(|v| v.as_str())
    else {
        return false;
    };
    result.insert(result_key.into(), value.to_string());
    true
}

fn insert_channel_string(
    channels: &yaml_serde::Mapping,
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
            true,
            "web-token".to_string(),
            true,
            true,
            "telegram-token".to_string(),
        );

        let web = channels.get("web").expect("web");
        let web_file = yaml_serde::to_string(web.file_auth_token.as_ref().expect("web file"))
            .expect("serialize web file");
        assert!(web_file.contains("source: env"));
        assert!(web_file.contains("id: WEB_AUTH_TOKEN"));

        let discord = channels.get("discord").expect("discord");
        assert!(discord.enabled == Some(true));

        let telegram = channels.get("telegram").expect("telegram");
        let bots = telegram.telegram_bots.as_ref().expect("telegram bots");
        let default_bot = bots
            .get(&crate::config::BotId::new("default"))
            .expect("default bot");
        let telegram_file =
            yaml_serde::to_string(default_bot.file_token.as_ref().expect("telegram file"))
                .expect("serialize telegram file");
        assert!(telegram_file.contains("id: TELEGRAM_BOT_TOKEN"));
    }

    #[test]
    fn build_channel_configs_includes_web_when_enabled() {
        let channels =
            build_channel_configs(true, "web-token".to_string(), false, false, String::new());
        assert!(channels.contains_key("web"));
    }

    #[test]
    fn build_channel_configs_omits_web_when_disabled() {
        let channels =
            build_channel_configs(false, "web-token".to_string(), false, false, String::new());
        assert!(
            !channels.contains_key("web"),
            "web entry must be absent when web_enabled is false"
        );
    }

    #[test]
    fn build_channel_configs_includes_discord_and_telegram_when_enabled() {
        let channels = build_channel_configs(
            false,
            "web-token".to_string(),
            true,
            true,
            "telegram-token".to_string(),
        );
        assert!(channels.contains_key("discord"));
        assert!(channels.contains_key("telegram"));
        assert!(!channels.contains_key("web"));
    }
}

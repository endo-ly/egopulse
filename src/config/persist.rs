use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use fs2::FileExt;
use serde::Serialize;

use super::Config;
use super::secret_ref::{ResolvedValue, dotenv_path, save_dotenv};
use crate::error::EgoPulseError;

static CONFIG_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Serialize)]
struct SerializableDiscordBot {
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_yaml_value"
    )]
    token: Option<serde_yml::Value>,
    default_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    channels: Option<HashMap<String, SerializableDiscordChannel>>,
}

#[derive(Serialize)]
struct SerializableDiscordChannel {
    #[serde(skip_serializing_if = "is_default")]
    require_mention: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
}

fn is_default(b: &bool) -> bool {
    !b
}

#[derive(Serialize)]
struct SerializableAgent {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

#[derive(Serialize)]
struct SerializableConfig {
    default_provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<String>,
    state_root: String,
    log_level: String,
    compaction_timeout_secs: u64,
    max_history_messages: usize,
    max_session_messages: usize,
    compact_keep_recent: usize,
    providers: HashMap<String, SerializableProvider>,
    channels: HashMap<String, SerializableChannel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_agent: Option<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    agents: HashMap<String, SerializableAgent>,
}

#[derive(Serialize)]
struct SerializableProvider {
    label: String,
    base_url: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_yaml_value"
    )]
    api_key: Option<serde_yml::Value>,
    default_model: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    models: Vec<String>,
}

#[derive(Serialize)]
struct SerializableChannel {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_yaml_value"
    )]
    auth_token: Option<serde_yml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_origins: Option<Vec<String>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_yaml_value"
    )]
    bot_token: Option<serde_yml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bot_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chats: Option<HashMap<String, SerializableTelegramChat>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    soul_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bots: Option<HashMap<String, SerializableDiscordBot>>,
}

#[derive(Serialize)]
struct SerializableTelegramChat {
    #[serde(skip_serializing_if = "is_default")]
    require_mention: bool,
}

fn serialize_optional_yaml_value<S>(
    value: &Option<serde_yml::Value>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(v) => serde_yml::Value::serialize(v, serializer),
        None => serializer.serialize_none(),
    }
}

impl From<&Config> for SerializableConfig {
    fn from(config: &Config) -> Self {
        let providers = config
            .providers
            .iter()
            .map(|(id, p)| {
                let api_key_value = p.api_key.as_ref().map(|rv| rv.to_yaml_value());
                (
                    id.to_string(),
                    SerializableProvider {
                        label: p.label.clone(),
                        base_url: p.base_url.clone(),
                        api_key: api_key_value,
                        default_model: p.default_model.clone(),
                        models: p.models.clone(),
                    },
                )
            })
            .collect();

        let channels = config
            .channels
            .iter()
            .map(|(id, c)| {
                (
                    id.to_string(),
                    SerializableChannel {
                        enabled: c.enabled,
                        port: c.port,
                        host: c.host.clone(),
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        auth_token: c.file_auth_token.clone(),
                        allowed_origins: c.allowed_origins.clone(),
                        bot_token: c.file_bot_token.clone(),
                        bot_username: c.bot_username.clone(),
                        chats: c.chats.as_ref().map(|chat_map| {
                            chat_map
                                .iter()
                                .map(|(chat_id, chat_config)| {
                                    (
                                        chat_id.to_string(),
                                        SerializableTelegramChat {
                                            require_mention: chat_config.require_mention,
                                        },
                                    )
                                })
                                .collect()
                        }),
                        soul_path: c.soul_path.clone(),
                        bots: c.discord_bots.as_ref().map(|bots| {
                            bots.iter()
                                .map(|(bot_id, bot)| {
                                    (
                                        bot_id.to_string(),
                                        SerializableDiscordBot {
                                            token: bot.file_token.clone(),
                                            default_agent: bot.default_agent.to_string(),
                                            channels: bot.channels.as_ref().map(|ch_map| {
                                                ch_map
                                                    .iter()
                                                    .map(|(ch_id, ch_config)| {
                                                        (
                                                            ch_id.to_string(),
                                                            SerializableDiscordChannel {
                                                                require_mention: ch_config
                                                                    .require_mention,
                                                                agent: ch_config
                                                                    .agent
                                                                    .as_ref()
                                                                    .map(|a| a.to_string()),
                                                            },
                                                        )
                                                    })
                                                    .collect()
                                            }),
                                        },
                                    )
                                })
                                .collect()
                        }),
                    },
                )
            })
            .collect();

        let agents = config
            .agents
            .iter()
            .map(|(id, a)| {
                (
                    id.to_string(),
                    SerializableAgent {
                        label: a.label.clone(),
                        provider: a.provider.clone(),
                        model: a.model.clone(),
                    },
                )
            })
            .collect();

        Self {
            default_provider: config.default_provider.to_string(),
            default_model: config.default_model.clone(),
            state_root: config.state_root.clone(),
            log_level: config.log_level.clone(),
            compaction_timeout_secs: config.compaction_timeout_secs,
            max_history_messages: config.max_history_messages,
            max_session_messages: config.max_session_messages,
            compact_keep_recent: config.compact_keep_recent,
            providers,
            channels,
            default_agent: Some(config.default_agent.to_string()),
            agents,
        }
    }
}

/// Atomically writes the current config to a YAML file.
///
/// Uses the global `CONFIG_WRITE_LOCK` for in-process mutual exclusion and an
/// file-level lock (`fs2`) for cross-process safety. The write is atomic via
/// temp-file + rename.
pub fn save_yaml(config: &Config, path: &Path) -> Result<(), EgoPulseError> {
    let _guard = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| EgoPulseError::Internal("config write lock poisoned".to_string()))?;
    let _lock_file = acquire_config_lock(path)?;

    let yaml = serde_yml::to_string(&SerializableConfig::from(config))
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    write_atomically(path, &yaml)
}

/// Saves config with SecretRef-aware YAML and .env file.
///
/// Writes the YAML with SecretRef objects for secrets, and writes actual values
/// for env-mode secrets to the .env file.
pub fn save_config_with_secrets(config: &Config, yaml_path: &Path) -> Result<(), EgoPulseError> {
    let dotenv_entries = collect_dotenv_entries(config);
    if !dotenv_entries.is_empty() {
        if let Some(config_dir) = yaml_path.parent() {
            let env_path = dotenv_path(config_dir);
            save_dotenv(&env_path, &dotenv_entries).map_err(EgoPulseError::Config)?;
        }
    }

    save_yaml(config, yaml_path)?;

    Ok(())
}

fn collect_dotenv_entries(config: &Config) -> Vec<(String, String)> {
    let mut entries = Vec::new();

    for (id, provider) in &config.providers {
        if let Some(ResolvedValue::EnvRef { value, id: env_id }) = &provider.api_key {
            entries.push((env_id.clone(), value.clone()));
        }
        let _ = id;
    }

    for channel in config.channels.values() {
        if let Some(ResolvedValue::EnvRef { value, id: env_id }) = &channel.auth_token {
            entries.push((env_id.clone(), value.clone()));
        }
        if let Some(ResolvedValue::EnvRef { value, id: env_id }) = &channel.bot_token {
            entries.push((env_id.clone(), value.clone()));
        }
        if let Some(bots) = &channel.discord_bots {
            for (bot_id, bot) in bots {
                if let Some(ResolvedValue::EnvRef { value, id: env_id }) = &bot.token {
                    entries.push((env_id.clone(), value.clone()));
                }
                let _ = bot_id;
            }
        }
    }

    entries
}

fn acquire_config_lock(path: &Path) -> Result<File, EgoPulseError> {
    let lock_path = {
        let parent = path
            .parent()
            .ok_or_else(|| EgoPulseError::Internal("config path has no parent".to_string()))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("egopulse.config.yaml");
        parent.join(format!(".{file_name}.lock"))
    };
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    }

    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    lock_file
        .lock_exclusive()
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    Ok(lock_file)
}

fn write_atomically(path: &Path, content: &str) -> Result<(), EgoPulseError> {
    let parent = path
        .parent()
        .ok_or_else(|| EgoPulseError::Internal("config path has no parent".to_string()))?;
    fs::create_dir_all(parent).map_err(|error| EgoPulseError::Internal(error.to_string()))?;

    let temp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("egopulse.config.yaml"),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let mut opts = OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut temp_file = opts
        .open(&temp_path)
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    temp_file
        .write_all(content.as_bytes())
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    temp_file
        .flush()
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    temp_file
        .sync_all()
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    drop(temp_file);

    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(EgoPulseError::Internal(error.to_string()));
    }

    if let Ok(dir) = OpenOptions::new().read(true).open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::secret_ref::{
        DISCORD_BOT_TOKEN_ENV_NAME, WEB_AUTH_TOKEN_ENV_NAME, env_resolved_value, env_yaml_value,
    };
    use crate::config::{
        BotId, ChannelConfig, ChannelName, DiscordBotConfig, ProviderConfig, ProviderId,
    };
    use serial_test::serial;

    fn sample_config() -> Config {
        use super::super::{AgentConfig, AgentId};
        let mut providers = HashMap::new();
        providers.insert(
            ProviderId::new("openai"),
            ProviderConfig {
                label: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: Some(env_resolved_value("OPENAI_API_KEY", "sk-test")),
                default_model: "gpt-5".to_string(),
                models: vec!["gpt-5".to_string()],
            },
        );

        let mut channels = HashMap::new();
        channels.insert(
            ChannelName::new("web"),
            ChannelConfig {
                enabled: Some(true),
                host: Some("127.0.0.1".to_string()),
                port: Some(10961),
                auth_token: Some(env_resolved_value(WEB_AUTH_TOKEN_ENV_NAME, "web-token")),
                file_auth_token: Some(env_yaml_value(WEB_AUTH_TOKEN_ENV_NAME)),
                ..Default::default()
            },
        );
        channels.insert(
            ChannelName::new("discord"),
            ChannelConfig {
                enabled: Some(true),
                bot_token: Some(env_resolved_value(
                    DISCORD_BOT_TOKEN_ENV_NAME,
                    "discord-token",
                )),
                file_bot_token: Some(env_yaml_value(DISCORD_BOT_TOKEN_ENV_NAME)),
                ..Default::default()
            },
        );

        let mut agents = HashMap::new();
        agents.insert(
            AgentId::new("default"),
            AgentConfig {
                label: "Default Agent".to_string(),
                ..Default::default()
            },
        );

        Config {
            default_provider: ProviderId::new("openai"),
            default_model: None,
            providers,
            state_root: "/tmp/egopulse".to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            max_session_messages: 40,
            compact_keep_recent: 20,
            channels,
            default_agent: AgentId::new("default"),
            agents,
        }
    }

    #[test]
    fn save_config_with_secrets_writes_dotenv_and_secret_refs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("egopulse.config.yaml");

        save_config_with_secrets(&sample_config(), &path).expect("save config");

        let yaml = fs::read_to_string(&path).expect("yaml");
        assert!(yaml.contains("source: env"));
        assert!(yaml.contains("id: OPENAI_API_KEY"));
        assert!(yaml.contains(&format!("id: {WEB_AUTH_TOKEN_ENV_NAME}")));
        assert!(yaml.contains(&format!("id: {DISCORD_BOT_TOKEN_ENV_NAME}")));
        assert!(!yaml.contains("sk-test"));
        assert!(!yaml.contains("web-token"));
        assert!(!yaml.contains("discord-token"));

        let dotenv = fs::read_to_string(dir.path().join(".env")).expect(".env");
        assert!(dotenv.contains("OPENAI_API_KEY=sk-test"));
        assert!(dotenv.contains(&format!("{WEB_AUTH_TOKEN_ENV_NAME}=web-token")));
        assert!(dotenv.contains(&format!("{DISCORD_BOT_TOKEN_ENV_NAME}=discord-token")));
    }

    #[test]
    #[serial]
    fn save_config_with_secrets_preserves_yaml_secret_refs_during_runtime_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("egopulse.config.yaml");
        let initial_yaml = format!(
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key:
      source: env
      id: OPENAI_API_KEY
    default_model: gpt-5
channels:
  web:
    enabled: true
    auth_token:
      source: env
      id: {WEB_AUTH_TOKEN_ENV_NAME}
  discord:
    enabled: true
    bot_token:
      source: env
      id: {DISCORD_BOT_TOKEN_ENV_NAME}
"#
        );
        fs::write(&path, initial_yaml).expect("write yaml");
        fs::write(
            dir.path().join(".env"),
            format!(
                "OPENAI_API_KEY=sk-test\n{WEB_AUTH_TOKEN_ENV_NAME}=web-file\n{DISCORD_BOT_TOKEN_ENV_NAME}=discord-file\n"
            ),
        )
        .expect("write dotenv");

        let previous_web = std::env::var_os(WEB_AUTH_TOKEN_ENV_NAME);
        let previous_discord = std::env::var_os(DISCORD_BOT_TOKEN_ENV_NAME);
        // SAFETY: this serial test mutates process environment in isolation and restores it below.
        unsafe {
            std::env::set_var(WEB_AUTH_TOKEN_ENV_NAME, "web-override");
            std::env::set_var(DISCORD_BOT_TOKEN_ENV_NAME, "discord-override");
        }

        let config = Config::load_allow_missing_api_key(Some(&path)).expect("load config");
        save_config_with_secrets(&config, &path).expect("save config");

        // SAFETY: restore original process environment values before assertions can unwind.
        unsafe {
            match previous_web {
                Some(value) => std::env::set_var(WEB_AUTH_TOKEN_ENV_NAME, value),
                None => std::env::remove_var(WEB_AUTH_TOKEN_ENV_NAME),
            }
            match previous_discord {
                Some(value) => std::env::set_var(DISCORD_BOT_TOKEN_ENV_NAME, value),
                None => std::env::remove_var(DISCORD_BOT_TOKEN_ENV_NAME),
            }
        }

        let yaml = fs::read_to_string(&path).expect("yaml");
        assert!(yaml.contains(&format!("id: {WEB_AUTH_TOKEN_ENV_NAME}")));
        assert!(yaml.contains(&format!("id: {DISCORD_BOT_TOKEN_ENV_NAME}")));
        assert!(!yaml.contains("web-override"));
        assert!(!yaml.contains("discord-override"));
    }

    #[test]
    #[serial]
    fn save_load_round_trip_preserves_discord_bots_and_default_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("egopulse.config.yaml");

        // Arrange: config with discord bots
        let mut config = sample_config();
        let discord_channel = config
            .channels
            .get_mut(&ChannelName::new("discord"))
            .expect("discord channel");
        discord_channel.discord_bots = Some({
            let mut bots = HashMap::new();
            bots.insert(
                BotId::new("main"),
                DiscordBotConfig {
                    token: Some(env_resolved_value("MY_DISCORD_BOT_TOKEN", "bot-secret-123")),
                    file_token: Some(env_yaml_value("MY_DISCORD_BOT_TOKEN")),
                    default_agent: crate::config::AgentId::new("default"),
                    channels: Some(
                        [
                            (111u64, crate::config::DiscordChannelConfig::default()),
                            (222u64, crate::config::DiscordChannelConfig::default()),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                },
            );
            bots
        });

        // Act: save → load
        save_config_with_secrets(&config, &path).expect("save");
        let loaded = Config::load_allow_missing_api_key(Some(&path)).expect("load");

        // Assert: bot is preserved
        let bots = loaded.discord_bots();
        assert_eq!(bots.len(), 1);
        assert_eq!(*bots[0].bot_id, BotId::new("main"));
        assert_eq!(
            *bots[0].default_agent,
            crate::config::AgentId::new("default")
        );
        assert_eq!(bots[0].allowed_channels.len(), 2);
        assert!(bots[0].allowed_channels.contains(&111));
        assert!(bots[0].allowed_channels.contains(&222));

        let dotenv = fs::read_to_string(dir.path().join(".env")).expect(".env");
        assert!(dotenv.contains("MY_DISCORD_BOT_TOKEN=bot-secret-123"));
    }
}

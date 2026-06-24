use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::channels::{
    build_channel_configs, extract_existing_state_root, generate_auth_token, load_channel_fields,
};
use super::inputs::{SetupInputs, validate_inputs};
use super::provider::{
    find_provider_preset, normalize_provider_id, provider_default_base_url, provider_default_model,
    provider_label_for,
};
use super::slugify::slugify_agent_id;
use crate::config::secret_ref::{
    DISCORD_BOT_TOKEN_ENV_NAME, env_resolved_value, env_yaml_value as yaml_value,
    provider_api_key_env_name,
};
use crate::config::{
    Config, ProviderConfig, ProviderId, default_state_root, default_workspace_dir,
};
use crate::error::EgoPulseError;

const CONFIG_BACKUP_DIR: &str = "egopulse.config.backups";
const MAX_CONFIG_BACKUPS: usize = 50;

/// Saves configuration derived from [`SetupInputs`], writing the YAML file and
/// associated `.env`, and generating a backup when an existing config is present.
///
/// Agent-First 設計に基づき以下を行う:
///
/// - `inputs.agent_label` を [`slugify_agent_id`] で agent id へ正規化し、
///   `agents.<id>.label` に label を保存、`default_agent` をその id に設定
/// - [`build_channel_configs`] に `inputs.web_enabled` を渡し、Web 無効化時に
///   `channels.web` エントリを保存しない (Discord/Telegram と一貫)
/// - 既存 `WEB_AUTH_TOKEN` を再利用 (新規生成しない)
/// - 既存 `state_root` および compaction 系パラメータを保持
///
/// # Errors
///
/// - [`validate_inputs`] が `Err` の場合
/// - 設定ディレクトリ / state_root / workspace ディレクトリの作成に失敗した場合
/// - 既存設定ファイルのバックアップ作成に失敗した場合
/// - 設定ファイル (YAML + `.env`) の保存に失敗した場合
pub(crate) fn save_config(
    inputs: &SetupInputs,
    original_yaml: Option<&yaml_serde::Value>,
    config_path: &Path,
) -> Result<(Option<String>, Vec<String>), String> {
    validate_inputs(inputs)?;

    let provider_id = normalize_provider_id(&inputs.provider_id);
    let provider_label = provider_label_for(&provider_id);
    let agent_id = slugify_agent_id(&inputs.agent_label);

    let model = non_empty_owned(&inputs.model)
        .or_else(|| provider_default_model(&provider_id).map(|value| value.to_string()))
        .unwrap_or_default();

    let base_url = non_empty_owned(&inputs.base_url)
        .or_else(|| provider_default_base_url(&provider_id).map(|value| value.to_string()))
        .unwrap_or_default();

    let api_key = inputs.api_key.trim().to_string();

    let existing_config = Config::load_allow_missing_api_key(Some(config_path)).ok();

    let existing_token = existing_config
        .as_ref()
        .and_then(|config| config.web_auth_token().map(str::to_string));
    let has_existing_token = existing_token.is_some();
    let auth_token = existing_token.unwrap_or_else(generate_auth_token);

    let owned_yaml = original_yaml.cloned();
    let existing_state_root = extract_existing_state_root(&owned_yaml);

    let discord_enabled = inputs.discord_enabled;
    let discord_bot_token = inputs.discord_bot_token.trim().to_string();
    let telegram_enabled = inputs.telegram_enabled;
    let telegram_bot_token = inputs.telegram_bot_token.trim().to_string();

    if let Some(config_dir) = config_path.parent() {
        fs::create_dir_all(config_dir)
            .map_err(|e| format!("Failed to create config directory: {e}"))?;
    }
    let default_root =
        default_state_root().map_err(|e| format!("Failed to resolve state root: {e}"))?;
    fs::create_dir_all(&default_root)
        .map_err(|e| format!("Failed to create state root directory: {e}"))?;
    let default_ws =
        default_workspace_dir().map_err(|e| format!("Failed to resolve workspace dir: {e}"))?;
    fs::create_dir_all(&default_ws)
        .map_err(|e| format!("Failed to create workspace directory: {e}"))?;

    let backup_path = if config_path.exists() {
        Some(backup_config(config_path)?)
    } else {
        None
    };

    let preset = find_provider_preset(&provider_id);
    let preset_default_model = preset
        .map(|p| p.default_model.to_string())
        .unwrap_or_else(|| model.clone());
    let mut preset_models: HashMap<String, crate::config::ModelConfig> = preset
        .map(|p| {
            p.models
                .iter()
                .map(|m| ((*m).to_string(), crate::config::ModelConfig::default()))
                .collect()
        })
        .unwrap_or_else(|| {
            let mut m = HashMap::new();
            m.insert(model.clone(), crate::config::ModelConfig::default());
            if !m.contains_key(&preset_default_model) {
                m.insert(
                    preset_default_model.clone(),
                    crate::config::ModelConfig::default(),
                );
            }
            m
        });
    // Ensure the user's chosen model is always in the models map.
    preset_models.entry(model.clone()).or_default();

    let mut providers = HashMap::new();
    providers.insert(
        ProviderId::new(&provider_id),
        ProviderConfig {
            label: provider_label.clone(),
            base_url: base_url.clone(),
            api_key: if api_key.is_empty() {
                None
            } else {
                Some(env_resolved_value(
                    provider_api_key_env_name(&provider_id),
                    api_key.clone(),
                ))
            },
            default_model: preset_default_model,
            models: preset_models,
        },
    );

    let discord_bots: Option<HashMap<crate::config::BotId, crate::config::DiscordBotConfig>> =
        if discord_enabled && !discord_bot_token.is_empty() {
            let mut bots = HashMap::new();
            bots.insert(
                crate::config::BotId::new("default"),
                crate::config::DiscordBotConfig {
                    token: Some(env_resolved_value(
                        DISCORD_BOT_TOKEN_ENV_NAME,
                        discord_bot_token,
                    )),
                    file_token: Some(yaml_value(DISCORD_BOT_TOKEN_ENV_NAME)),
                },
            );
            Some(bots)
        } else {
            None
        };

    let mut channels = build_channel_configs(
        inputs.web_enabled,
        auth_token,
        discord_enabled,
        telegram_enabled,
        telegram_bot_token,
    );

    if let Some(bots) = discord_bots {
        channels
            .entry(crate::config::ChannelName::new("discord"))
            .or_default()
            .discord_bots = Some(bots);
    }

    let agents: HashMap<crate::config::AgentId, crate::config::AgentConfig> = HashMap::from([(
        crate::config::AgentId::new(&agent_id),
        crate::config::AgentConfig {
            label: inputs.agent_label.clone(),
            ..Default::default()
        },
    )]);

    let config = Config {
        default_provider: ProviderId::new(&provider_id),
        default_model: Some(model.clone()),
        providers,
        state_root: existing_state_root
            .unwrap_or_else(|| default_root.to_string_lossy().into_owned()),
        log_level: "info".to_string(),
        compaction_timeout_secs: existing_config
            .as_ref()
            .map(|c| c.compaction_timeout_secs)
            .unwrap_or(180),
        max_history_messages: existing_config
            .as_ref()
            .map(|c| c.max_history_messages)
            .unwrap_or(50),
        compact_keep_recent: existing_config
            .as_ref()
            .map(|c| c.compact_keep_recent)
            .unwrap_or(20),
        default_context_window_tokens: existing_config
            .as_ref()
            .map(|c| c.default_context_window_tokens)
            .unwrap_or(32768),
        compaction_threshold_ratio: existing_config
            .as_ref()
            .map(|c| c.compaction_threshold_ratio)
            .unwrap_or(0.80),
        compaction_target_ratio: existing_config
            .as_ref()
            .map(|c| c.compaction_target_ratio)
            .unwrap_or(0.40),
        channels,
        default_agent: crate::config::AgentId::new(&agent_id),
        agents,
        timezone: existing_config
            .as_ref()
            .map(|c| c.timezone.clone())
            .unwrap_or_else(|| "UTC".to_string()),
        sleep_batch: existing_config
            .as_ref()
            .map(|c| c.sleep_batch.clone())
            .unwrap_or_default(),
        pulse: existing_config
            .as_ref()
            .map(|c| c.pulse.clone())
            .unwrap_or_default(),
        db: existing_config
            .as_ref()
            .map(|c| c.db.clone())
            .unwrap_or_default(),
        web_fetch: existing_config
            .as_ref()
            .map(|c| c.web_fetch.clone())
            .unwrap_or_default(),
    };

    config
        .save_config_with_secrets(config_path)
        .map_err(|e: EgoPulseError| format!("Failed to save config: {e}"))?;

    let mut completion_summary = vec![
        format!("Config saved to: {}", config_path.display()),
        format!("Agent: {} (id: {agent_id})", inputs.agent_label),
        format!("Provider: {provider_label} ({provider_id})"),
        format!("Model: {model}"),
        format!("Base URL: {base_url}"),
        if api_key.is_empty() {
            "API key: (empty - local endpoint)".into()
        } else {
            format!("API key: {}", mask_secret(&api_key))
        },
        if !inputs.web_enabled {
            "Web channel: disabled".into()
        } else if has_existing_token {
            "Web channel: enabled (auth_token reused)".into()
        } else {
            "Web channel: enabled (auth_token auto-generated)".into()
        },
        format!(
            "Discord channel: {}",
            if discord_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
        format!(
            "Telegram channel: {}",
            if telegram_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
    ];

    if let Some(ref backup) = backup_path {
        completion_summary.push(format!("Previous config backed up to: {backup}"));
    }

    let existing_non_default = original_yaml
        .and_then(|yaml| yaml.as_mapping())
        .and_then(|m| m.get(yaml_serde::Value::String("agents".into())))
        .and_then(|a| a.as_mapping())
        .map(|m| {
            m.keys()
                .filter_map(|k| k.as_str())
                .filter(|id| *id != agent_id)
                .count()
        })
        .unwrap_or(0);
    if existing_non_default > 0 {
        completion_summary.push(format!(
            "⚠ Existing {existing_non_default} custom agent(s) preserved in backup; \
             re-add them to agents in config YAML if needed"
        ));
    }

    Ok((backup_path, completion_summary))
}

pub(crate) fn mask_secret(value: &str) -> String {
    if value.chars().count() <= 8 {
        return "********".into();
    }
    let visible: String = value.chars().take(4).collect();
    format!("{visible}********")
}

fn non_empty_owned(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

pub(crate) fn backup_config(path: &Path) -> Result<String, String> {
    let backup_dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(CONFIG_BACKUP_DIR);
    fs::create_dir_all(&backup_dir).map_err(|e| format!("Failed to create backup dir: {e}"))?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("egopulse.config.yaml");
    let backup_name = format!("{file_name}.{timestamp}.bak");
    let backup_path = backup_dir.join(&backup_name);

    fs::copy(path, &backup_path).map_err(|e| format!("Failed to backup config: {e}"))?;

    cleanup_old_backups(&backup_dir, file_name)?;

    Ok(backup_path.to_string_lossy().to_string())
}

pub(crate) fn cleanup_old_backups(backup_dir: &Path, file_name: &str) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(backup_dir)
        .map_err(|e| format!("Failed to read backup dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name().to_str().is_some_and(|name| {
                name.strip_prefix(file_name)
                    .is_some_and(|rest| rest.starts_with('.'))
            })
        })
        .collect();

    entries.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());

    while entries.len() > MAX_CONFIG_BACKUPS {
        if let Some(oldest) = entries.first() {
            fs::remove_file(oldest.path())
                .map_err(|e| format!("Failed to remove old backup: {e}"))?;
            entries.remove(0);
        } else {
            break;
        }
    }

    Ok(())
}

/// 既存設定 YAML のパース結果。
///
/// `fields` はウィザードプロンプトのデフォルト値として事前入力に使用する平坦なマップ。
/// `root` は元の YAML ルートノード (state_root 抽出等に使用)。
pub(crate) struct ExistingConfig {
    pub fields: HashMap<String, String>,
    pub root: Option<yaml_serde::Value>,
}

/// YAML 文字列をパースし、プロバイダー・チャネル情報を抽出する純粋関数。
///
/// ファイル IO (`.env` からのトークン読み込み等) は行わず、YAML テキストのみを入力とする。
/// 既存 `SetupApp::load_existing_config` の YAML パース部分を切り出したもので、
/// パースエラーを黙殺せず `Err` で返す。
///
/// # Errors
///
/// YAML 構文エラーの場合、パースエラーの内容を含むメッセージを返す。
pub(crate) fn parse_existing_config(yaml_text: &str) -> Result<ExistingConfig, String> {
    let parsed: yaml_serde::Value = yaml_serde::from_str(yaml_text)
        .map_err(|e| format!("Failed to parse existing config YAML: {e}"))?;

    let mut fields = HashMap::new();

    if let Some(map) = parsed.as_mapping() {
        extract_provider_fields(map, &mut fields);
        if let Some(channels) = map.get(yaml_string_key("channels")) {
            load_channel_fields(channels, &mut fields);
        }
    }

    Ok(ExistingConfig {
        fields,
        root: Some(parsed),
    })
}

fn extract_provider_fields(map: &yaml_serde::Mapping, fields: &mut HashMap<String, String>) {
    let Some(default_provider) = map
        .get(yaml_string_key("default_provider"))
        .and_then(|v| v.as_str())
    else {
        return;
    };

    let provider_id = normalize_provider_id(default_provider);
    fields.insert("PROVIDER".into(), provider_id.clone());

    let provider_map = map
        .get(yaml_string_key("providers"))
        .and_then(|v| v.as_mapping())
        .and_then(|providers| providers.get(yaml_string_key(default_provider)))
        .and_then(|v| v.as_mapping());

    let model = map
        .get(yaml_string_key("default_model"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            provider_map
                .and_then(|pm| pm.get(yaml_string_key("default_model")))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .or_else(|| provider_default_model(&provider_id).map(str::to_string));
    if let Some(model) = model {
        fields.insert("MODEL".into(), model);
    }

    let base_url = provider_map
        .and_then(|pm| pm.get(yaml_string_key("base_url")))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| provider_default_base_url(&provider_id).map(str::to_string));
    if let Some(base_url) = base_url {
        fields.insert("BASE_URL".into(), base_url);
    }

    if let Some(api_key) = provider_map
        .and_then(|pm| pm.get(yaml_string_key("api_key")))
        .and_then(|v| v.as_str())
    {
        fields.insert("API_KEY".into(), api_key.to_string());
    }
}

fn yaml_string_key(value: &str) -> yaml_serde::Value {
    yaml_serde::Value::String(value.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{mask_secret, parse_existing_config, save_config};
    use crate::config::AgentId;
    use crate::config::Config;
    use crate::setup::inputs::SetupInputs;
    use serial_test::serial;

    fn ollama_inputs(agent_label: &str) -> SetupInputs {
        SetupInputs {
            agent_label: agent_label.into(),
            provider_id: "ollama".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "llama3.2".into(),
            api_key: String::new(),
            web_enabled: false,
            discord_enabled: false,
            discord_bot_token: String::new(),
            telegram_enabled: false,
            telegram_bot_token: String::new(),
        }
    }

    #[test]
    fn parse_existing_config_returns_err_for_invalid_yaml() {
        let result = parse_existing_config("default_provider: [unclosed");
        assert!(result.is_err());
    }

    #[test]
    fn parse_existing_config_extracts_provider_schema() {
        let yaml = r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-token
"#;
        let config = parse_existing_config(yaml).expect("valid yaml");
        assert_eq!(config.fields.get("PROVIDER"), Some(&"openai".to_string()));
        assert_eq!(config.fields.get("MODEL"), Some(&"gpt-4o-mini".to_string()));
        assert_eq!(
            config.fields.get("BASE_URL"),
            Some(&"https://api.openai.com/v1".to_string())
        );
        assert_eq!(
            config.fields.get("WEB_AUTH_TOKEN"),
            Some(&"web-token".to_string())
        );
    }

    #[test]
    #[serial]
    fn save_config_persists_agent_label() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let inputs = ollama_inputs("Partner");
        save_config(&inputs, None, &config_path).expect("save config");

        let loaded = Config::load_allow_missing_api_key(Some(&config_path)).expect("load config");
        let agent = loaded
            .agents
            .get(&AgentId::new("partner"))
            .expect("agent 'partner' should exist");
        assert_eq!(agent.label, "Partner");
    }

    #[test]
    #[serial]
    fn save_config_sets_default_agent_to_user_id() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let inputs = ollama_inputs("My Companion");
        save_config(&inputs, None, &config_path).expect("save config");

        let loaded = Config::load_allow_missing_api_key(Some(&config_path)).expect("load config");
        assert_eq!(loaded.default_agent, AgentId::new("my-companion"));
    }

    #[test]
    #[serial]
    fn save_config_omits_web_entry_when_disabled() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let inputs = ollama_inputs("Partner");
        save_config(&inputs, None, &config_path).expect("save config");

        let loaded = Config::load_allow_missing_api_key(Some(&config_path)).expect("load config");
        assert!(
            !loaded.channels.contains_key("web"),
            "channels.web must be absent when web_enabled is false"
        );
    }

    #[test]
    #[serial]
    fn save_config_creates_backup_when_existing_file_present() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        std::fs::write(
            &config_path,
            "default_provider: ollama\nproviders:\n  ollama:\n    label: Ollama\n    base_url: http://127.0.0.1:11434/v1\n    default_model: llama3.2\ndefault_agent: default\nagents:\n  default:\n    label: Default\n",
        )
        .expect("write existing config");

        let inputs = ollama_inputs("Partner");
        let (backup_path, _) = save_config(&inputs, None, &config_path).expect("save config");

        let backup = backup_path.expect("backup path should be Some when file existed");
        assert!(
            Path::new(&backup).exists(),
            "backup file should exist on disk"
        );
    }

    #[test]
    #[serial]
    fn save_config_roundtrips_with_config_load() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let inputs = SetupInputs {
            agent_label: "Partner".into(),
            provider_id: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o".into(),
            api_key: "sk-test-roundtrip-key".into(),
            web_enabled: true,
            discord_enabled: false,
            discord_bot_token: String::new(),
            telegram_enabled: false,
            telegram_bot_token: String::new(),
        };
        save_config(&inputs, None, &config_path).expect("save config");

        let loaded = Config::load(Some(&config_path)).expect("load config via Config::load");
        assert_eq!(loaded.default_agent, AgentId::new("partner"));
        assert!(loaded.agents.contains_key(&AgentId::new("partner")));
        assert_eq!(
            loaded.agents.get(&AgentId::new("partner")).unwrap().label,
            "Partner"
        );
        assert_eq!(loaded.default_provider.as_str(), "openai");
        assert!(loaded.providers.contains_key("openai"));
        assert_eq!(loaded.default_model, Some("gpt-4o".to_string()));
    }

    #[test]
    #[serial]
    fn save_config_reuses_existing_web_auth_token() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let web_token_env = crate::config::secret_ref::WEB_AUTH_TOKEN_ENV_NAME;
        let existing_yaml = format!(
            "default_provider: ollama\n\
             providers:\n\
             \x20 ollama:\n\
             \x20   label: Ollama\n\
             \x20   base_url: http://127.0.0.1:11434/v1\n\
             \x20   default_model: llama3.2\n\
             default_agent: default\n\
             agents:\n\
             \x20 default:\n\
             \x20   label: Default\n\
             channels:\n\
             \x20 web:\n\
             \x20   enabled: true\n\
             \x20   auth_token:\n\
             \x20     source: env\n\
             \x20     id: {web_token_env}\n"
        );
        std::fs::write(&config_path, &existing_yaml).expect("write existing config");
        std::fs::write(
            temp_dir.path().join(".env"),
            format!("{web_token_env}=existing-token-from-previous-setup\n"),
        )
        .expect("write .env");

        let parsed_yaml: yaml_serde::Value =
            yaml_serde::from_str(&existing_yaml).expect("parse existing yaml");
        let inputs = SetupInputs {
            web_enabled: true,
            ..ollama_inputs("Partner")
        };
        save_config(&inputs, Some(&parsed_yaml), &config_path).expect("save config");

        let dotenv =
            std::fs::read_to_string(temp_dir.path().join(".env")).expect("read .env after save");
        assert!(
            dotenv.contains("existing-token-from-previous-setup"),
            "existing WEB_AUTH_TOKEN must be reused, not regenerated"
        );
    }

    #[test]
    #[serial]
    fn save_config_preserves_existing_state_root() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");

        let custom_state_root = temp_dir.path().join("custom-state");
        let root_str = custom_state_root.to_string_lossy();
        let existing_yaml = format!(
            "state_root: {root_str}\n\
             default_provider: ollama\n\
             providers:\n\
             \x20 ollama:\n\
             \x20   label: Ollama\n\
             \x20   base_url: http://127.0.0.1:11434/v1\n\
             \x20   default_model: llama3.2\n\
             default_agent: default\n\
             agents:\n\
             \x20 default:\n\
             \x20   label: Default\n"
        );
        std::fs::write(&config_path, &existing_yaml).expect("write existing config");

        let parsed_yaml: yaml_serde::Value =
            yaml_serde::from_str(&existing_yaml).expect("parse existing yaml");
        let inputs = ollama_inputs("Partner");
        save_config(&inputs, Some(&parsed_yaml), &config_path).expect("save config");

        let loaded = Config::load_allow_missing_api_key(Some(&config_path)).expect("load config");
        assert_eq!(
            loaded.state_root,
            root_str.to_string(),
            "existing state_root must be preserved"
        );
    }

    #[test]
    fn mask_secret_fully_masks_short_values() {
        let short_secret = "sk-1234";

        let masked = mask_secret(short_secret);

        assert_eq!(masked, "********");
    }
}

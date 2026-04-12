use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use secrecy::SecretString;
use url::Url;

use super::channels::{
    build_channel_configs, extract_existing_web_auth_token, generate_auth_token,
};
use super::provider::{
    find_provider_preset, normalize_provider_id, provider_default_base_url, provider_default_model,
    provider_label_for,
};
use super::{Field, SetupApp};
use crate::config::{
    Config, ProviderConfig, base_url_allows_empty_api_key, default_data_dir, default_workspace_dir,
};
use crate::error::EgoPulseError;

const CONFIG_BACKUP_DIR: &str = "egopulse.config.backups";
const MAX_CONFIG_BACKUPS: usize = 50;

pub(crate) fn validate_fields(fields: &[Field]) -> Result<(), String> {
    let provider = field_value(fields, "PROVIDER");

    if provider.is_empty() {
        return Err("Provider profile ID is required".into());
    }

    let model = field_value(fields, "MODEL");
    let effective_model = if model.is_empty() {
        provider_default_model(provider).unwrap_or("")
    } else {
        model
    };

    let base_url = field_value(fields, "BASE_URL");
    let effective_base_url = if base_url.is_empty() {
        provider_default_base_url(provider).unwrap_or("")
    } else {
        base_url
    };

    if effective_base_url.is_empty() {
        return Err(format!(
            "API base URL is required for provider '{provider}'"
        ));
    }

    if Url::parse(effective_base_url).is_err() {
        return Err(format!("Invalid API base URL: {effective_base_url}"));
    }

    if effective_model.is_empty() {
        return Err(format!("LLM model is required for provider '{provider}'"));
    }

    let api_key = field_value(fields, "API_KEY");

    if !base_url_allows_empty_api_key(effective_base_url) && api_key.is_empty() {
        return Err(
            "API key is required for non-local endpoints. Use a local URL (localhost/127.0.0.1) to skip.".into(),
        );
    }

    validate_enabled_token(
        fields,
        "DISCORD_ENABLED",
        "DISCORD_BOT_TOKEN",
        "Discord bot token is required when Discord is enabled",
    )?;
    validate_enabled_token(
        fields,
        "TELEGRAM_ENABLED",
        "TELEGRAM_BOT_TOKEN",
        "Telegram bot token is required when Telegram is enabled",
    )?;
    validate_enabled_token(
        fields,
        "TELEGRAM_ENABLED",
        "TELEGRAM_BOT_USERNAME",
        "Telegram bot username is required when Telegram is enabled",
    )?;

    Ok(())
}

pub(crate) fn save_config(
    fields: &[Field],
    original_yaml: &Option<serde_yml::Value>,
    config_path: &Path,
) -> Result<(Option<String>, Vec<String>), String> {
    validate_fields(fields)?;

    let provider_id = normalize_provider_id(field_value(fields, "PROVIDER"));
    let provider_label = provider_label_for(&provider_id);

    let model = non_empty_owned(field_value(fields, "MODEL"))
        .or_else(|| provider_default_model(&provider_id).map(|value| value.to_string()))
        .unwrap_or_default();

    let base_url = non_empty_owned(field_value(fields, "BASE_URL"))
        .or_else(|| provider_default_base_url(&provider_id).map(|value| value.to_string()))
        .unwrap_or_default();

    let api_key = field_value(fields, "API_KEY").to_string();

    let existing_token = extract_existing_web_auth_token(original_yaml);
    let has_existing_token = existing_token.is_some();
    let auth_token = existing_token.unwrap_or_else(generate_auth_token);

    let discord_enabled = field_bool(fields, "DISCORD_ENABLED");
    let discord_bot_token = field_value(fields, "DISCORD_BOT_TOKEN").to_string();
    let telegram_enabled = field_bool(fields, "TELEGRAM_ENABLED");
    let telegram_bot_token = field_value(fields, "TELEGRAM_BOT_TOKEN").to_string();
    let telegram_bot_username = field_value(fields, "TELEGRAM_BOT_USERNAME").to_string();

    if let Some(config_dir) = config_path.parent() {
        fs::create_dir_all(config_dir)
            .map_err(|e| format!("Failed to create config directory: {e}"))?;
    }
    fs::create_dir_all(default_data_dir().map_err(|e| e.to_string())?)
        .map_err(|e| format!("Failed to create data directory: {e}"))?;
    fs::create_dir_all(default_workspace_dir().map_err(|e| e.to_string())?)
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
    let preset_models: Vec<String> = preset
        .map(|p| p.models.iter().map(|m| (*m).to_string()).collect())
        .unwrap_or_else(|| {
            let mut m = vec![model.clone()];
            if m[0] != preset_default_model {
                m.insert(0, preset_default_model.clone());
            }
            m
        });

    let mut providers = HashMap::new();
    providers.insert(
        provider_id.clone(),
        ProviderConfig {
            label: provider_label.clone(),
            base_url: base_url.clone(),
            api_key: if api_key.is_empty() {
                None
            } else {
                Some(SecretString::new(api_key.clone().into_boxed_str()))
            },
            default_model: preset_default_model,
            models: preset_models,
        },
    );

    let channels = build_channel_configs(
        auth_token,
        discord_enabled,
        discord_bot_token,
        telegram_enabled,
        telegram_bot_token,
        telegram_bot_username,
    );

    let config = Config {
        default_provider: provider_id.clone(),
        default_model: Some(model.clone()),
        providers,
        data_dir: default_data_dir()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned(),
        log_level: "info".to_string(),
        compaction_timeout_secs: 180,
        max_history_messages: 50,
        max_session_messages: 40,
        compact_keep_recent: 20,
        channels,
    };

    config
        .save_yaml(config_path)
        .map_err(|e: EgoPulseError| format!("Failed to save config: {e}"))?;

    let mut completion_summary = vec![
        format!("Config saved to: {}", config_path.display()),
        format!("Provider: {provider_label} ({provider_id})"),
        format!("Model: {model}"),
        format!("Base URL: {base_url}"),
        if api_key.is_empty() {
            "API key: (empty - local endpoint)".into()
        } else {
            format!("API key: {}", mask_secret(&api_key))
        },
        if has_existing_token {
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

    Ok((backup_path, completion_summary))
}

pub(crate) fn mask_secret(value: &str) -> String {
    if value.chars().count() <= 8 {
        return "********".into();
    }
    let visible: String = value.chars().take(4).collect();
    format!("{visible}********")
}

fn field_value<'a>(fields: &'a [Field], key: &str) -> &'a str {
    fields
        .iter()
        .find(|f| f.key == key)
        .map(|f| f.value.trim())
        .unwrap_or("")
}

fn field_bool(fields: &[Field], key: &str) -> bool {
    fields
        .iter()
        .find(|f| f.key == key)
        .and_then(|f| super::parse_bool(&f.value))
        .unwrap_or(false)
}

fn validate_enabled_token(
    fields: &[Field],
    enabled_key: &str,
    token_key: &str,
    error_message: &str,
) -> Result<(), String> {
    if !field_bool(fields, enabled_key) {
        return Ok(());
    }
    if !field_value(fields, token_key).is_empty() {
        return Ok(());
    }
    Err(error_message.into())
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

pub(crate) fn draw_completion_summary(frame: &mut ratatui::Frame<'_>, app: &SetupApp, area: Rect) {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        "Setup Complete!",
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(""));

    for item in &app.completion_summary {
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::raw(item),
        ]));
    }

    let body = Paragraph::new(lines)
        .block(Block::default().title("Summary").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(body, area);
}

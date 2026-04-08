//! 対話型セットアップウィザード。
//!
//! Ratatui ベースのローカル UI で設定値を収集し、既存 YAML を必要最小限だけ保ちながら
//! `egopulse.config.yaml` を生成・更新する。

use std::collections::HashMap;
use std::fs;
use std::io::{self};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use rand::Rng;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Terminal, backend::CrosstermBackend};
use url::Url;

use crate::config::{
    base_url_allows_empty_api_key, default_config_path, default_data_dir, default_workspace_dir,
};

const CONFIG_BACKUP_DIR: &str = "egopulse.config.backups";
const MAX_CONFIG_BACKUPS: usize = 50;

#[derive(Clone)]
struct Field {
    key: String,
    label: String,
    value: String,
    required: bool,
    secret: bool,
    help: Option<String>,
}

impl Field {
    fn display_value(&self, editing: bool) -> String {
        if editing || !self.secret {
            return self.value.clone();
        }
        if self.value.is_empty() {
            String::new()
        } else {
            mask_secret(&self.value)
        }
    }
}

struct SetupApp {
    fields: Vec<Field>,
    selected: usize,
    editing: bool,
    status: String,
    completed: bool,
    backup_path: Option<String>,
    completion_summary: Vec<String>,
    config_path: PathBuf,
    original_yaml: Option<serde_yml::Value>,
}

impl SetupApp {
    fn new(config_path: Option<PathBuf>) -> Self {
        let config_path = config_path.unwrap_or_else(default_config_path);
        let (existing, original_yaml) = Self::load_existing_config(&config_path);

        let mut fields = vec![
            Field {
                key: "MODEL".into(),
                label: "LLM model".into(),
                value: existing
                    .get("MODEL")
                    .cloned()
                    .unwrap_or_else(|| "gpt-4o-mini".into()),
                required: false,
                secret: false,
                help: Some("Model name for your LLM provider".into()),
            },
            Field {
                key: "BASE_URL".into(),
                label: "API base URL".into(),
                value: existing
                    .get("BASE_URL")
                    .cloned()
                    .unwrap_or_else(|| "https://api.openai.com/v1".into()),
                required: true,
                secret: false,
                help: Some("OpenAI-compatible API endpoint".into()),
            },
            Field {
                key: "API_KEY".into(),
                label: "API key".into(),
                value: existing.get("API_KEY").cloned().unwrap_or_default(),
                required: true,
                secret: true,
                help: Some("Leave empty for local endpoints (localhost/127.0.0.1)".into()),
            },
            Field {
                key: "DISCORD_ENABLED".into(),
                label: "Enable Discord channel".into(),
                value: existing
                    .get("DISCORD_ENABLED")
                    .cloned()
                    .unwrap_or_else(|| "false".into()),
                required: false,
                secret: false,
                help: Some("true/false".into()),
            },
            Field {
                key: "DISCORD_BOT_TOKEN".into(),
                label: "Discord bot token".into(),
                value: existing
                    .get("DISCORD_BOT_TOKEN")
                    .cloned()
                    .unwrap_or_default(),
                required: false,
                secret: true,
                help: Some("From Discord Developer Portal".into()),
            },
            Field {
                key: "TELEGRAM_ENABLED".into(),
                label: "Enable Telegram channel".into(),
                value: existing
                    .get("TELEGRAM_ENABLED")
                    .cloned()
                    .unwrap_or_else(|| "false".into()),
                required: false,
                secret: false,
                help: Some("true/false".into()),
            },
            Field {
                key: "TELEGRAM_BOT_TOKEN".into(),
                label: "Telegram bot token".into(),
                value: existing
                    .get("TELEGRAM_BOT_TOKEN")
                    .cloned()
                    .unwrap_or_default(),
                required: false,
                secret: true,
                help: Some("From @BotFather on Telegram".into()),
            },
            Field {
                key: "TELEGRAM_BOT_USERNAME".into(),
                label: "Telegram bot username".into(),
                value: existing
                    .get("TELEGRAM_BOT_USERNAME")
                    .cloned()
                    .unwrap_or_default(),
                required: false,
                secret: false,
                help: Some("Without @, e.g. my_egopulse_bot".into()),
            },
        ];

        // Hide channel-specific fields when channel is disabled
        Self::update_field_visibility(&mut fields);

        Self {
            fields,
            selected: 0,
            editing: false,
            status: "Enter: edit | Up/Down: navigate | Ctrl+S: save & exit | Ctrl+C: cancel".into(),
            completed: false,
            backup_path: None,
            completion_summary: Vec::new(),
            config_path,
            original_yaml,
        }
    }

    fn update_field_visibility(fields: &mut [Field]) {
        let discord_enabled = fields
            .iter()
            .find(|f| f.key == "DISCORD_ENABLED")
            .map(|f| parse_bool(&f.value).unwrap_or(false))
            .unwrap_or(false);

        let telegram_enabled = fields
            .iter()
            .find(|f| f.key == "TELEGRAM_ENABLED")
            .map(|f| parse_bool(&f.value).unwrap_or(false))
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

    fn load_existing_config(
        config_path: &Path,
    ) -> (HashMap<String, String>, Option<serde_yml::Value>) {
        let mut result = HashMap::new();

        let contents = match fs::read_to_string(config_path) {
            Ok(c) => c,
            Err(_) => return (result, None),
        };

        let parsed: serde_yml::Value = match serde_yml::from_str(&contents) {
            Ok(v) => v,
            Err(_) => return (result, None),
        };

        if let Some(map) = parsed.as_mapping() {
            for (key, value) in map {
                if let Some(key_str) = key.as_str() {
                    if let Some(val_str) = value.as_str() {
                        result.insert(key_str.to_ascii_uppercase(), val_str.to_string());
                    }
                }
            }

            // Web の auth_token は既存値を再利用し、ブラウザ側の再ログインを避ける。
            if let Some(channels) = map.get(serde_yml::Value::String("channels".into())) {
                if let Some(ch_map) = channels.as_mapping() {
                    if let Some(web) = ch_map.get(serde_yml::Value::String("web".into())) {
                        if let Some(web_map) = web.as_mapping() {
                            if let Some(token) =
                                web_map.get(serde_yml::Value::String("auth_token".into()))
                            {
                                if let Some(token_str) = token.as_str() {
                                    result.insert("WEB_AUTH_TOKEN".into(), token_str.to_string());
                                }
                            }
                        }
                    }

                    // Extract discord
                    if let Some(discord) = ch_map.get(serde_yml::Value::String("discord".into())) {
                        if let Some(d_map) = discord.as_mapping() {
                            if let Some(enabled) =
                                d_map.get(serde_yml::Value::String("enabled".into()))
                            {
                                if let Some(b) = enabled.as_bool() {
                                    result.insert("DISCORD_ENABLED".into(), b.to_string());
                                }
                            }
                            if let Some(token) =
                                d_map.get(serde_yml::Value::String("bot_token".into()))
                            {
                                if let Some(t) = token.as_str() {
                                    result.insert("DISCORD_BOT_TOKEN".into(), t.to_string());
                                }
                            }
                        }
                    }

                    // Extract telegram
                    if let Some(tg) = ch_map.get(serde_yml::Value::String("telegram".into())) {
                        if let Some(tg_map) = tg.as_mapping() {
                            if let Some(enabled) =
                                tg_map.get(serde_yml::Value::String("enabled".into()))
                            {
                                if let Some(b) = enabled.as_bool() {
                                    result.insert("TELEGRAM_ENABLED".into(), b.to_string());
                                }
                            }
                            if let Some(token) =
                                tg_map.get(serde_yml::Value::String("bot_token".into()))
                            {
                                if let Some(t) = token.as_str() {
                                    result.insert("TELEGRAM_BOT_TOKEN".into(), t.to_string());
                                }
                            }
                            if let Some(username) =
                                tg_map.get(serde_yml::Value::String("bot_username".into()))
                            {
                                if let Some(u) = username.as_str() {
                                    result.insert("TELEGRAM_BOT_USERNAME".into(), u.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        (result, Some(parsed))
    }

    fn visible_fields(&self) -> Vec<usize> {
        let mut indices = Vec::new();

        for field in self.fields.iter().enumerate() {
            let should_skip = match field.1.key.as_str() {
                "DISCORD_BOT_TOKEN" => !self
                    .fields
                    .iter()
                    .find(|f| f.key == "DISCORD_ENABLED")
                    .map(|f| parse_bool(&f.value).unwrap_or(false))
                    .unwrap_or(false),
                "TELEGRAM_BOT_TOKEN" | "TELEGRAM_BOT_USERNAME" => !self
                    .fields
                    .iter()
                    .find(|f| f.key == "TELEGRAM_ENABLED")
                    .map(|f| parse_bool(&f.value).unwrap_or(false))
                    .unwrap_or(false),
                _ => false,
            };

            if !should_skip {
                indices.push(field.0);
            }
        }

        indices
    }

    fn move_selection(&mut self, delta: isize) {
        let visible = self.visible_fields();
        if visible.is_empty() {
            return;
        }

        let current_pos = visible
            .iter()
            .position(|&idx| idx == self.selected)
            .unwrap_or(0);

        let next_pos = (current_pos as isize + delta).clamp(0, visible.len() as isize - 1) as usize;

        self.selected = visible[next_pos];
    }

    fn current_field(&self) -> Option<&Field> {
        self.fields.get(self.selected)
    }

    fn current_field_mut(&mut self) -> Option<&mut Field> {
        self.fields.get_mut(self.selected)
    }

    fn validate(&self) -> Result<(), String> {
        let base_url = self
            .fields
            .iter()
            .find(|f| f.key == "BASE_URL")
            .map(|f| f.value.trim())
            .unwrap_or("");

        if base_url.is_empty() {
            return Err("API base URL is required".into());
        }

        if Url::parse(base_url).is_err() {
            return Err(format!("Invalid API base URL: {base_url}"));
        }

        let api_key = self
            .fields
            .iter()
            .find(|f| f.key == "API_KEY")
            .map(|f| f.value.trim())
            .unwrap_or("");

        // ローカル推論サーバーだけは API キー未設定を許可する。
        if !base_url_allows_empty_api_key(base_url) && api_key.is_empty() {
            return Err(
                "API key is required for non-local endpoints. Use a local URL (localhost/127.0.0.1) to skip.".into(),
            );
        }

        let discord_enabled = self
            .fields
            .iter()
            .find(|f| f.key == "DISCORD_ENABLED")
            .map(|f| parse_bool(&f.value).unwrap_or(false))
            .unwrap_or(false);

        if discord_enabled {
            let discord_token = self
                .fields
                .iter()
                .find(|f| f.key == "DISCORD_BOT_TOKEN")
                .map(|f| f.value.trim())
                .unwrap_or("");
            // 有効化したチャネルだけ必須入力にし、未使用チャネルの秘密情報は求めない。
            if discord_token.is_empty() {
                return Err("Discord bot token is required when Discord is enabled".into());
            }
        }

        let telegram_enabled = self
            .fields
            .iter()
            .find(|f| f.key == "TELEGRAM_ENABLED")
            .map(|f| parse_bool(&f.value).unwrap_or(false))
            .unwrap_or(false);

        if telegram_enabled {
            let telegram_token = self
                .fields
                .iter()
                .find(|f| f.key == "TELEGRAM_BOT_TOKEN")
                .map(|f| f.value.trim())
                .unwrap_or("");
            // 有効化したチャネルだけ必須入力にし、未使用チャネルの秘密情報は求めない。
            if telegram_token.is_empty() {
                return Err("Telegram bot token is required when Telegram is enabled".into());
            }
        }

        Ok(())
    }

    fn save(&mut self) -> Result<(), String> {
        self.validate()?;

        let model = self
            .fields
            .iter()
            .find(|f| f.key == "MODEL")
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();

        let base_url = self
            .fields
            .iter()
            .find(|f| f.key == "BASE_URL")
            .map(|f| f.value.trim().to_string())
            .unwrap_or_else(|| "https://api.openai.com/v1".into());

        let api_key = self
            .fields
            .iter()
            .find(|f| f.key == "API_KEY")
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();

        let existing_token = self
            .original_yaml
            .as_ref()
            .and_then(|v| v.as_mapping())
            .and_then(|m| m.get(serde_yml::Value::String("channels".into())))
            .and_then(|c| c.as_mapping())
            .and_then(|m| m.get(serde_yml::Value::String("web".into())))
            .and_then(|w| w.as_mapping())
            .and_then(|m| m.get(serde_yml::Value::String("auth_token".into())))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        // 既存トークンがあれば維持し、初回作成時のみ新規生成する。
        let auth_token = existing_token.unwrap_or_else(generate_auth_token);

        let discord_enabled = self
            .fields
            .iter()
            .find(|f| f.key == "DISCORD_ENABLED")
            .map(|f| parse_bool(&f.value).unwrap_or(false))
            .unwrap_or(false);

        let discord_bot_token = self
            .fields
            .iter()
            .find(|f| f.key == "DISCORD_BOT_TOKEN")
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();

        let telegram_enabled = self
            .fields
            .iter()
            .find(|f| f.key == "TELEGRAM_ENABLED")
            .map(|f| parse_bool(&f.value).unwrap_or(false))
            .unwrap_or(false);

        let telegram_bot_token = self
            .fields
            .iter()
            .find(|f| f.key == "TELEGRAM_BOT_TOKEN")
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();

        let telegram_bot_username = self
            .fields
            .iter()
            .find(|f| f.key == "TELEGRAM_BOT_USERNAME")
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();

        let config_path = &self.config_path;
        if let Some(config_dir) = config_path.parent() {
            fs::create_dir_all(config_dir)
                .map_err(|e| format!("Failed to create config directory: {e}"))?;
        }
        fs::create_dir_all(default_data_dir())
            .map_err(|e| format!("Failed to create data directory: {e}"))?;
        fs::create_dir_all(default_workspace_dir())
            .map_err(|e| format!("Failed to create workspace directory: {e}"))?;

        if config_path.exists() {
            self.backup_path = Some(backup_config(config_path)?);
        }

        // 未知の top-level キーは保持しつつ、ウィザード管理対象だけを更新する。
        let mut yaml_value = self
            .original_yaml
            .clone()
            .unwrap_or(serde_yml::Value::Mapping(Default::default()));

        let map = yaml_value.as_mapping_mut().unwrap();

        map.insert(
            serde_yml::Value::String("model".into()),
            serde_yml::Value::String(model.clone()),
        );
        if !api_key.is_empty() {
            map.insert(
                serde_yml::Value::String("api_key".into()),
                serde_yml::Value::String(api_key.clone()),
            );
        } else {
            map.remove(serde_yml::Value::String("api_key".into()));
        }
        map.insert(
            serde_yml::Value::String("base_url".into()),
            serde_yml::Value::String(base_url.clone()),
        );
        map.remove(serde_yml::Value::String("data_dir".into()));
        map.remove(serde_yml::Value::String("workspace_dir".into()));
        map.insert(
            serde_yml::Value::String("log_level".into()),
            serde_yml::Value::String("info".into()),
        );

        let mut channels = serde_yml::Value::Mapping(Default::default());
        let channels_map = channels.as_mapping_mut().unwrap();

        let mut web = serde_yml::Value::Mapping(Default::default());
        let web_map = web.as_mapping_mut().unwrap();
        web_map.insert(
            serde_yml::Value::String("enabled".into()),
            serde_yml::Value::Bool(true),
        );
        web_map.insert(
            serde_yml::Value::String("host".into()),
            serde_yml::Value::String("127.0.0.1".into()),
        );
        web_map.insert(
            serde_yml::Value::String("port".into()),
            serde_yml::Value::Number(serde_yml::Number::from(10961)),
        );
        web_map.insert(
            serde_yml::Value::String("auth_token".into()),
            serde_yml::Value::String(auth_token),
        );
        channels_map.insert(serde_yml::Value::String("web".into()), web);

        if discord_enabled {
            let mut discord = serde_yml::Value::Mapping(Default::default());
            let d_map = discord.as_mapping_mut().unwrap();
            d_map.insert(
                serde_yml::Value::String("enabled".into()),
                serde_yml::Value::Bool(true),
            );
            d_map.insert(
                serde_yml::Value::String("bot_token".into()),
                serde_yml::Value::String(discord_bot_token),
            );
            channels_map.insert(serde_yml::Value::String("discord".into()), discord);
        }

        if telegram_enabled {
            let mut telegram = serde_yml::Value::Mapping(Default::default());
            let tg_map = telegram.as_mapping_mut().unwrap();
            tg_map.insert(
                serde_yml::Value::String("enabled".into()),
                serde_yml::Value::Bool(true),
            );
            tg_map.insert(
                serde_yml::Value::String("bot_token".into()),
                serde_yml::Value::String(telegram_bot_token),
            );
            if !telegram_bot_username.is_empty() {
                tg_map.insert(
                    serde_yml::Value::String("bot_username".into()),
                    serde_yml::Value::String(telegram_bot_username),
                );
            }
            channels_map.insert(serde_yml::Value::String("telegram".into()), telegram);
        }

        map.insert(serde_yml::Value::String("channels".into()), channels);

        let yaml = serde_yml::to_string(&yaml_value)
            .map_err(|e| format!("Failed to serialize config: {e}"))?;
        fs::write(config_path, &yaml).map_err(|e| format!("Failed to write config: {e}"))?;

        // 保存後の確認を端末上で完結できるよう、反映内容を要約して残す。
        self.completion_summary = vec![
            format!("Config saved to: {}", config_path.display()),
            format!("Model: {model}"),
            format!("Base URL: {base_url}"),
            if api_key.is_empty() {
                "API key: (empty - local endpoint)".into()
            } else {
                format!("API key: {}", mask_secret(&api_key))
            },
            "Web channel: enabled (auth_token auto-generated)".into(),
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

        if let Some(ref backup) = self.backup_path {
            self.completion_summary
                .push(format!("Previous config backed up to: {backup}"));
        }

        self.completed = true;
        Ok(())
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn generate_auth_token() -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random::<u8>()).collect();
    STANDARD.encode(&bytes)
}

fn mask_secret(value: &str) -> String {
    if value.len() <= 8 {
        return "********".into();
    }
    let visible = &value[..4];
    format!("{visible}********")
}

fn backup_config(path: &Path) -> Result<String, String> {
    let backup_dir = path
        .parent()
        .unwrap_or(Path::new("."))
        .join(CONFIG_BACKUP_DIR);
    fs::create_dir_all(&backup_dir).map_err(|e| format!("Failed to create backup dir: {e}"))?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("egopulse.config.yaml");
    let backup_name = format!("{file_name}.{timestamp}.bak");
    let backup_path = backup_dir.join(&backup_name);

    fs::copy(path, &backup_path).map_err(|e| format!("Failed to backup config: {e}"))?;

    // バックアップを無制限に増やさないため、古い世代から間引く。
    cleanup_old_backups(&backup_dir, file_name)?;

    Ok(backup_path.to_string_lossy().to_string())
}

fn cleanup_old_backups(backup_dir: &Path, file_name: &str) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(backup_dir)
        .map_err(|e| format!("Failed to read backup dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(file_name))
        .collect();

    entries.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());

    while entries.len() > MAX_CONFIG_BACKUPS {
        if let Some(oldest) = entries.first() {
            let _ = fs::remove_file(oldest.path());
            entries.remove(0);
        } else {
            break;
        }
    }

    Ok(())
}

fn draw(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &SetupApp) {
    let _ = terminal.draw(|frame| {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(area);

        // Header
        let header = Paragraph::new(vec![
            Line::from(vec![Span::styled(
                "EgoPulse Setup Wizard",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from("Configure egopulse.config.yaml interactively"),
        ])
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: true });
        frame.render_widget(header, chunks[0]);

        if app.completed {
            draw_completion_summary(frame, app, chunks[1]);
        } else {
            draw_fields(frame, app, chunks[1]);
        }

        // Footer
        let footer_text = if app.completed {
            vec![Line::from(
                "Setup complete. Run egopulse for the TUI, or egopulse run for channels.",
            )]
        } else {
            vec![
                Line::from(app.status.clone()),
                if let Some(field) = app.current_field() {
                    if let Some(ref help) = field.help {
                        Line::from(format!("hint: {help}"))
                    } else {
                        Line::from("")
                    }
                } else {
                    Line::from("")
                },
            ]
        };

        let footer = Paragraph::new(footer_text)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        frame.render_widget(footer, chunks[2]);

        // 編集中だけカーソルを明示し、非編集時のノイズを避ける。
        if app.editing && !app.completed {
            if let Some(field) = app.current_field() {
                let visible = app.visible_fields();
                let field_pos = visible
                    .iter()
                    .position(|&idx| idx == app.selected)
                    .unwrap_or(0);

                let content_height = chunks[1].height.saturating_sub(2) as usize;
                let mut window_start = 0usize;
                if field_pos < window_start {
                    window_start = field_pos;
                } else if field_pos >= window_start + content_height {
                    window_start = field_pos - content_height + 1;
                }
                let window_end = window_start + content_height;

                if (window_start..window_end).contains(&field_pos) {
                    let row = chunks[1].y + 1 + (field_pos - window_start) as u16;
                    let label_width = max_label_width(&app.fields, &visible);
                    let displayed_len = if field.value.is_empty() {
                        "(type value...)".chars().count()
                    } else {
                        field.value.chars().count()
                    };
                    let cursor_x = chunks[1].x + label_width + 3 + displayed_len as u16;
                    let cursor_y = row;
                    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
                }
            }
        }
    });
}

fn max_label_width(fields: &[Field], visible: &[usize]) -> u16 {
    let mut max = 0;
    for &idx in visible {
        if let Some(f) = fields.get(idx) {
            let len = f.label.chars().count();
            if len > max {
                max = len;
            }
        }
    }
    (max + 2) as u16
}

fn draw_fields(frame: &mut ratatui::Frame<'_>, app: &SetupApp, area: Rect) {
    let visible = app.visible_fields();
    if visible.is_empty() {
        return;
    }

    let content_height = area.height.saturating_sub(2) as usize;
    if content_height == 0 {
        return;
    }

    let field_pos = visible
        .iter()
        .position(|&idx| idx == app.selected)
        .unwrap_or(0);

    let mut window_start = 0usize;
    if field_pos < window_start {
        window_start = field_pos;
    } else if field_pos >= window_start + content_height {
        window_start = field_pos - content_height + 1;
    }

    let label_width = max_label_width(&app.fields, &visible);
    let window_end = (window_start + content_height).min(visible.len());

    let mut lines = Vec::new();
    for &idx in visible.iter().take(window_end).skip(window_start) {
        let field = &app.fields[idx];
        let is_selected = idx == app.selected;
        let is_editing = is_selected && app.editing;

        let display = field.display_value(is_editing);
        let prefix = if is_selected { "> " } else { "  " };

        let mut spans = vec![
            Span::raw(prefix),
            Span::styled(
                &field.label,
                Style::default().fg(if is_selected {
                    Color::Yellow
                } else {
                    Color::White
                }),
            ),
        ];

        // Separator
        let sep_len = label_width.saturating_sub(field.label.chars().count() as u16);
        if sep_len > 0 {
            spans.push(Span::raw(" ".repeat(sep_len as usize)));
        }

        spans.push(Span::raw(" "));

        // Value
        if is_editing {
            spans.push(Span::styled(
                if display.is_empty() {
                    "(type value...)".into()
                } else {
                    display
                },
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::UNDERLINED),
            ));
        } else if field.secret && !display.is_empty() {
            spans.push(Span::styled(display, Style::default().fg(Color::DarkGray)));
        } else if display.is_empty() {
            spans.push(Span::styled(
                "(empty)",
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            spans.push(Span::raw(display));
        }

        if field.required && !is_editing {
            spans.push(Span::styled(" *", Style::default().fg(Color::Red)));
        }

        lines.push(Line::from(spans));
    }

    let body = Paragraph::new(lines)
        .block(
            Block::default()
                .title("Configuration Fields")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(body, area);
}

fn draw_completion_summary(frame: &mut ratatui::Frame<'_>, app: &SetupApp, area: Rect) {
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

/// Runs the interactive setup wizard and writes the resulting configuration file.
pub async fn run_setup_wizard(config_path: Option<PathBuf>) -> Result<(), String> {
    let mut app = SetupApp::new(config_path);
    let terminal = init_terminal()?;

    run_loop(terminal, &mut app).await
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>, String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    if let Err(e) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e.to_string());
    }
    let backend = CrosstermBackend::new(stdout);
    match Terminal::new(backend) {
        Ok(t) => Ok(t),
        Err(e) => {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            Err(e.to_string())
        }
    }
}

async fn run_loop(
    mut terminal: Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut SetupApp,
) -> Result<(), String> {
    let result = run_inner(&mut terminal, app).await;
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    result
}

async fn run_inner(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut SetupApp,
) -> Result<(), String> {
    loop {
        draw(terminal, app);

        if app.completed {
            // 完了後はサマリーを読めるよう即終了せず、任意キーで閉じる。
            if event::poll(std::time::Duration::from_millis(200)).map_err(|e| e.to_string())? {
                if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                    if key.kind == KeyEventKind::Press {
                        return Ok(());
                    }
                }
            }
            continue;
        }

        if event::poll(std::time::Duration::from_millis(200)).map_err(|e| e.to_string())? {
            let Event::Key(key) = event::read().map_err(|e| e.to_string())? else {
                continue;
            };

            if key.kind != KeyEventKind::Press {
                continue;
            }

            if app.editing {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => {
                        app.editing = false;
                        if let Some(field) = app.current_field() {
                            // トグル変更は編集終了時にだけ反映し、入力中の項目飛びを防ぐ。
                            if field.key == "DISCORD_ENABLED" || field.key == "TELEGRAM_ENABLED" {
                                SetupApp::update_field_visibility(&mut app.fields);
                            }
                        }
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        match app.save() {
                            Ok(()) => {
                                app.editing = false;
                                app.status = "Config saved successfully!".into();
                            }
                            Err(e) => {
                                app.status = format!("Save failed: {e}");
                            }
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Err("Setup cancelled".into());
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(field) = app.current_field_mut() {
                            field.value.push(c);
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(field) = app.current_field_mut() {
                            field.value.pop();
                        }
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Err("Setup cancelled".into());
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        match app.save() {
                            Ok(()) => {
                                app.status = "Config saved successfully!".into();
                            }
                            Err(e) => {
                                app.status = format!("Save failed: {e}");
                            }
                        }
                    }
                    KeyCode::Enter => {
                        if app.current_field().is_some() {
                            app.editing = true;
                            // 画面遷移を増やさず、その場でインライン編集に入る。
                            app.status = "Editing... (Enter/Esc to finish)".into();
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.move_selection(-1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.move_selection(1);
                    }
                    _ => {}
                }
            }
        }
    }
}

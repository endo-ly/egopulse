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

use secrecy::SecretString;

use crate::config::{
    base_url_allows_empty_api_key, default_config_path, default_data_dir, default_workspace_dir,
    ChannelConfig, Config, ProviderConfig,
};
use crate::error::EgoPulseError;

const CONFIG_BACKUP_DIR: &str = "egopulse.config.backups";
const MAX_CONFIG_BACKUPS: usize = 50;

#[derive(Clone, Copy)]
struct ProviderPreset {
    id: &'static str,
    label: &'static str,
    default_base_url: &'static str,
    default_model: &'static str,
    models: &'static [&'static str],
}

const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "openai",
        label: "OpenAI",
        default_base_url: "https://api.openai.com/v1",
        default_model: "gpt-5.2",
        models: &["gpt-5.2", "gpt-5", "gpt-5-mini"],
    },
    ProviderPreset {
        id: "openrouter",
        label: "OpenRouter",
        default_base_url: "https://openrouter.ai/api/v1",
        default_model: "openrouter/auto",
        models: &[
            "openrouter/auto",
            "openai/gpt-5.2",
            "anthropic/claude-sonnet-4.5",
        ],
    },
    ProviderPreset {
        id: "ollama",
        label: "Ollama (local)",
        default_base_url: "http://127.0.0.1:11434/v1",
        default_model: "llama3.2",
        models: &["llama3.2", "qwen2.5-coder:7b", "mistral"],
    },
    ProviderPreset {
        id: "google",
        label: "Google DeepMind",
        default_base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        default_model: "gemini-2.5-pro",
        models: &[
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
        ],
    },
    ProviderPreset {
        id: "aliyun-bailian",
        label: "Alibaba Cloud Bailian",
        default_base_url: "https://coding.dashscope.aliyuncs.com/v1",
        default_model: "qwen3.5-plus",
        models: &["qwen3.5-plus", "qwen3-max", "qwen-plus-latest"],
    },
    ProviderPreset {
        id: "alibaba",
        label: "Alibaba Cloud (Qwen / DashScope)",
        default_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        default_model: "qwen3-max",
        models: &["qwen3-max", "qwen3-plus", "qwen-max-latest"],
    },
    ProviderPreset {
        id: "qwen-portal",
        label: "Qwen Portal (OAuth)",
        default_base_url: "https://portal.qwen.ai/v1",
        default_model: "coder-model",
        models: &["coder-model", "vision-model", "qwen3.5-plus"],
    },
    ProviderPreset {
        id: "deepseek",
        label: "DeepSeek",
        default_base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        models: &["deepseek-chat", "deepseek-reasoner", "deepseek-v3"],
    },
    ProviderPreset {
        id: "synthetic",
        label: "Synthetic",
        default_base_url: "https://api.synthetic.new/openai/v1",
        default_model: "hf:openai/gpt-oss-120b",
        models: &["hf:openai/gpt-oss-120b", "hf:deepseek-ai/DeepSeek-V3-0324"],
    },
    ProviderPreset {
        id: "chutes",
        label: "Chutes",
        default_base_url: "https://llm.chutes.ai/v1",
        default_model: "deepseek-ai/DeepSeek-V3-0324",
        models: &[
            "deepseek-ai/DeepSeek-V3-0324",
            "Qwen/Qwen3-Coder-480B-A35B-Instruct",
        ],
    },
    ProviderPreset {
        id: "moonshot",
        label: "Moonshot AI (Kimi)",
        default_base_url: "https://api.moonshot.cn/v1",
        default_model: "kimi-k2.5",
        models: &["kimi-k2.5", "kimi-k2", "kimi-latest"],
    },
    ProviderPreset {
        id: "mistral",
        label: "Mistral AI",
        default_base_url: "https://api.mistral.ai/v1",
        default_model: "mistral-large-latest",
        models: &[
            "mistral-large-latest",
            "mistral-medium-latest",
            "ministral-8b-latest",
        ],
    },
    ProviderPreset {
        id: "azure",
        label: "Microsoft Azure AI",
        default_base_url: "https://YOUR-RESOURCE.openai.azure.com/openai/deployments/YOUR-DEPLOYMENT",
        default_model: "gpt-5.2",
        models: &["gpt-5.2", "gpt-5", "gpt-4.1"],
    },
    ProviderPreset {
        id: "bedrock",
        label: "Amazon AWS Bedrock",
        default_base_url: "https://bedrock-runtime.YOUR-REGION.amazonaws.com/openai/v1",
        default_model: "anthropic.claude-opus-4-6-v1",
        models: &[
            "anthropic.claude-opus-4-6-v1",
            "anthropic.claude-sonnet-4-5-v2",
            "anthropic.claude-haiku-4-5-v1",
        ],
    },
    ProviderPreset {
        id: "zhipu",
        label: "Zhipu AI (GLM / Z.AI)",
        default_base_url: "https://open.bigmodel.cn/api/paas/v4",
        default_model: "glm-4.7",
        models: &["glm-4.7", "glm-4.7-flash", "glm-4.5-air"],
    },
    ProviderPreset {
        id: "zai",
        label: "Z.AI Coding",
        default_base_url: "https://api.z.ai/api/coding/paas/v4",
        default_model: "glm-5.1",
        models: &["glm-5.1", "glm-5"],
    },
    ProviderPreset {
        id: "minimax",
        label: "MiniMax",
        default_base_url: "https://api.minimax.io/v1",
        default_model: "MiniMax-M2.5",
        models: &["MiniMax-M2.5", "MiniMax-M2.5-Thinking", "MiniMax-M2.1"],
    },
    ProviderPreset {
        id: "cohere",
        label: "Cohere",
        default_base_url: "https://api.cohere.ai/compatibility/v1",
        default_model: "command-a-03-2025",
        models: &[
            "command-a-03-2025",
            "command-r-plus-08-2024",
            "command-r-08-2024",
        ],
    },
    ProviderPreset {
        id: "tencent",
        label: "Tencent AI Lab",
        default_base_url: "https://api.hunyuan.cloud.tencent.com/v1",
        default_model: "hunyuan-t1-latest",
        models: &[
            "hunyuan-t1-latest",
            "hunyuan-turbos-latest",
            "hunyuan-standard-latest",
        ],
    },
    ProviderPreset {
        id: "xai",
        label: "xAI",
        default_base_url: "https://api.x.ai/v1",
        default_model: "grok-4",
        models: &["grok-4", "grok-4-fast", "grok-3"],
    },
    ProviderPreset {
        id: "nvidia",
        label: "NVIDIA NIM",
        default_base_url: "https://integrate.api.nvidia.com/v1",
        default_model: "meta/llama-3.3-70b-instruct",
        models: &[
            "meta/llama-3.3-70b-instruct",
            "meta/llama-3.1-70b-instruct",
            "nvidia/llama-3.1-nemotron-ultra-253b-v1",
        ],
    },
    ProviderPreset {
        id: "huggingface",
        label: "Hugging Face",
        default_base_url: "https://router.huggingface.co/v1",
        default_model: "Qwen/Qwen3-Coder-Next",
        models: &[
            "Qwen/Qwen3-Coder-Next",
            "meta-llama/Llama-3.3-70B-Instruct",
            "deepseek-ai/DeepSeek-V3",
        ],
    },
    ProviderPreset {
        id: "together",
        label: "Together AI",
        default_base_url: "https://api.together.xyz/v1",
        default_model: "deepseek-ai/DeepSeek-V3",
        models: &[
            "deepseek-ai/DeepSeek-V3",
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "Qwen/Qwen3-Coder-480B-A35B-Instruct-FP8",
        ],
    },
    ProviderPreset {
        id: "local",
        label: "Local OpenAI-compatible",
        default_base_url: "http://127.0.0.1:1234/v1",
        default_model: "qwen2.5-coder",
        models: &["qwen2.5-coder"],
    },
    ProviderPreset {
        id: "custom",
        label: "Custom OpenAI-compatible",
        default_base_url: "",
        default_model: "custom-model",
        models: &["custom-model"],
    },
];

fn find_provider_preset(provider: &str) -> Option<&'static ProviderPreset> {
    PROVIDER_PRESETS
        .iter()
        .find(|preset| preset.id.eq_ignore_ascii_case(provider))
}

fn provider_default_base_url(provider: &str) -> Option<&'static str> {
    find_provider_preset(provider)
        .map(|preset| preset.default_base_url)
        .filter(|value| !value.is_empty())
}

fn provider_default_model(provider: &str) -> Option<&'static str> {
    find_provider_preset(provider).map(|preset| preset.default_model)
}

fn provider_label_for(provider: &str) -> String {
    find_provider_preset(provider)
        .map(|preset| preset.label.to_string())
        .unwrap_or_else(|| provider.to_string())
}

fn provider_choices() -> String {
    PROVIDER_PRESETS
        .iter()
        .map(|preset| {
            if preset.models.is_empty() {
                preset.id.to_string()
            } else {
                format!("{} (e.g. {})", preset.id, preset.models.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn normalize_provider_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if find_provider_preset(trimmed).is_some() {
        return trimmed.to_ascii_lowercase();
    }
    trimmed.to_string()
}

fn provider_selector_items() -> Vec<SelectorItem> {
    PROVIDER_PRESETS
        .iter()
        .map(|preset| {
            let models_str = if preset.models.len() > 2 {
                format!(
                    "{}, ... ({} total)",
                    preset.models[..2].join(", "),
                    preset.models.len()
                )
            } else {
                preset.models.join(", ")
            };
            SelectorItem {
                display: format!("{} ({})", preset.id, models_str),
                value: preset.id.to_string(),
            }
        })
        .collect()
}

fn model_selector_items(provider_id: &str) -> Vec<SelectorItem> {
    find_provider_preset(provider_id)
        .map(|preset| {
            preset
                .models
                .iter()
                .map(|model| SelectorItem {
                    display: model.to_string(),
                    value: model.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn filtered_items<'a>(items: &'a [SelectorItem], filter: &str) -> Vec<&'a SelectorItem> {
    if filter.is_empty() {
        return items.iter().collect();
    }
    let lower = filter.to_ascii_lowercase();
    items
        .iter()
        .filter(|item| {
            item.display.to_ascii_lowercase().contains(&lower)
                || item.value.to_ascii_lowercase().contains(&lower)
        })
        .collect()
}

#[derive(Clone)]
struct Field {
    key: String,
    label: String,
    value: String,
    required: bool,
    secret: bool,
    help: Option<String>,
}

enum SetupMode {
    Navigate,
    Edit,
    Selector(SelectorState),
}

struct SelectorState {
    field_key: String,
    filter: String,
    items: Vec<SelectorItem>,
    selected: usize,
    original_value: String,
}

struct SelectorItem {
    display: String,
    value: String,
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
    mode: SetupMode,
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
        let provider_id = existing
            .get("PROVIDER")
            .cloned()
            .unwrap_or_else(|| "openai".into());
        let provider_model = existing
            .get("MODEL")
            .cloned()
            .or_else(|| provider_default_model(&provider_id).map(|value| value.to_string()))
            .unwrap_or_default();
        let provider_base_url = existing
            .get("BASE_URL")
            .cloned()
            .or_else(|| provider_default_base_url(&provider_id).map(|value| value.to_string()))
            .unwrap_or_default();

        let mut fields = vec![
            Field {
                key: "PROVIDER".into(),
                label: "Provider profile ID".into(),
                value: provider_id.clone(),
                required: true,
                secret: false,
                help: Some(format!(
                    "Profile id used as default_provider ({})",
                    provider_choices()
                )),
            },
            Field {
                key: "MODEL".into(),
                label: "LLM model".into(),
                value: provider_model,
                required: false,
                secret: false,
                help: Some("Model name for the selected provider profile".into()),
            },
            Field {
                key: "BASE_URL".into(),
                label: "API base URL".into(),
                value: provider_base_url,
                required: true,
                secret: false,
                help: Some(
                    "OpenAI-compatible API endpoint for the selected provider profile".into(),
                ),
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
            mode: SetupMode::Navigate,
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
            if let Some(default_provider) = map
                .get(serde_yml::Value::String("default_provider".into()))
                .and_then(|value| value.as_str())
            {
                let provider_id = normalize_provider_id(default_provider);
                result.insert("PROVIDER".into(), provider_id.clone());
                if let Some(providers) = map
                    .get(serde_yml::Value::String("providers".into()))
                    .and_then(|value| value.as_mapping())
                    && let Some(provider) = providers
                        .get(serde_yml::Value::String(default_provider.into()))
                        .and_then(|value| value.as_mapping())
                {
                    if let Some(model) = provider
                        .get(serde_yml::Value::String("default_model".into()))
                        .and_then(|value| value.as_str())
                    {
                        result.insert("MODEL".into(), model.to_string());
                    } else if let Some(model) = provider_default_model(&provider_id) {
                        result.insert("MODEL".into(), model.to_string());
                    }
                    if let Some(base_url) = provider
                        .get(serde_yml::Value::String("base_url".into()))
                        .and_then(|value| value.as_str())
                    {
                        result.insert("BASE_URL".into(), base_url.to_string());
                    } else if let Some(base_url) = provider_default_base_url(&provider_id) {
                        result.insert("BASE_URL".into(), base_url.to_string());
                    }
                    if let Some(api_key) = provider
                        .get(serde_yml::Value::String("api_key".into()))
                        .and_then(|value| value.as_str())
                    {
                        result.insert("API_KEY".into(), api_key.to_string());
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
        let provider = self
            .fields
            .iter()
            .find(|f| f.key == "PROVIDER")
            .map(|f| f.value.trim())
            .unwrap_or("");

        if provider.is_empty() {
            return Err("Provider profile ID is required".into());
        }

        let model = self
            .fields
            .iter()
            .find(|f| f.key == "MODEL")
            .map(|f| f.value.trim())
            .unwrap_or("");
        let effective_model = if model.is_empty() {
            provider_default_model(provider).unwrap_or("")
        } else {
            model
        };

        let base_url = self
            .fields
            .iter()
            .find(|f| f.key == "BASE_URL")
            .map(|f| f.value.trim())
            .unwrap_or("");
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

        let api_key = self
            .fields
            .iter()
            .find(|f| f.key == "API_KEY")
            .map(|f| f.value.trim())
            .unwrap_or("");

        // ローカル推論サーバーだけは API キー未設定を許可する。
        if !base_url_allows_empty_api_key(effective_base_url) && api_key.is_empty() {
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

        let provider_id = normalize_provider_id(
            self.fields
                .iter()
                .find(|f| f.key == "PROVIDER")
                .map(|f| f.value.trim())
                .unwrap_or(""),
        );
        let provider_label = provider_label_for(&provider_id);

        let model = self
            .fields
            .iter()
            .find(|f| f.key == "MODEL")
            .map(|f| f.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| provider_default_model(&provider_id).map(|value| value.to_string()))
            .unwrap_or_default();

        let base_url = self
            .fields
            .iter()
            .find(|f| f.key == "BASE_URL")
            .map(|f| f.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| provider_default_base_url(&provider_id).map(|value| value.to_string()))
            .unwrap_or_default();

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

        // プリセットの default_model / models は ProviderConfig にそのまま反映する。
        // ユーザーが選択したモデルは Config.default_model（YAML トップレベル）に置く。
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

        let config = Config {
            default_provider: provider_id.clone(),
            default_model: model.clone(),
            providers,
            data_dir: default_data_dir().to_string_lossy().into_owned(),
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

        // 保存後の確認を端末上で完結できるよう、反映内容を要約して残す。
        self.completion_summary = vec![
            format!("Config saved to: {}", config_path.display()),
            format!("Provider: {provider_label} ({provider_id})"),
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

    fn enter_selector(&self, field_key: &str) -> SelectorState {
        let items = match field_key {
            "PROVIDER" => provider_selector_items(),
            "MODEL" => {
                let provider = self
                    .fields
                    .iter()
                    .find(|f| f.key == "PROVIDER")
                    .map(|f| f.value.as_str())
                    .unwrap_or("");
                model_selector_items(provider)
            }
            _ => Vec::new(),
        };
        let original_value = self
            .fields
            .iter()
            .find(|f| f.key == field_key)
            .map(|f| f.value.clone())
            .unwrap_or_default();
        SelectorState {
            field_key: field_key.to_string(),
            filter: String::new(),
            items,
            selected: 0,
            original_value,
        }
    }

    fn apply_selector_selection(&mut self, field_key: &str) {
        if field_key == "PROVIDER" {
            let provider_id = self
                .fields
                .iter()
                .find(|f| f.key == "PROVIDER")
                .map(|f| f.value.clone())
                .unwrap_or_default();
            if let Some(preset) = find_provider_preset(&provider_id) {
                if let Some(model_field) = self.fields.iter_mut().find(|f| f.key == "MODEL") {
                    if model_field.value.is_empty() {
                        model_field.value = preset.default_model.to_string();
                    }
                }
                if let Some(url_field) = self.fields.iter_mut().find(|f| f.key == "BASE_URL") {
                    if url_field.value.is_empty() && !preset.default_base_url.is_empty() {
                        url_field.value = preset.default_base_url.to_string();
                    }
                }
            }
        }
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

        if let SetupMode::Selector(ref state) = app.mode {
            draw_selector_popup(frame, state, area);
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
        if matches!(app.mode, SetupMode::Edit) && !app.completed {
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

    let is_selector_active = matches!(app.mode, SetupMode::Selector(_));

    let mut lines = Vec::new();
    for &idx in visible.iter().take(window_end).skip(window_start) {
        let field = &app.fields[idx];
        let is_selected = idx == app.selected;
        let is_editing = is_selected && matches!(app.mode, SetupMode::Edit);

        let display = field.display_value(is_editing);
        let prefix = if is_selected { "> " } else { "  " };

        let base_style = if is_selector_active {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };

        let mut spans = vec![
            Span::styled(prefix, base_style),
            Span::styled(
                &field.label,
                if is_selector_active {
                    base_style
                } else if is_selected {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ];

        let sep_len = label_width.saturating_sub(field.label.chars().count() as u16);
        if sep_len > 0 {
            spans.push(Span::raw(" ".repeat(sep_len as usize)));
        }

        spans.push(Span::raw(" "));

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
            spans.push(Span::styled(
                display,
                if is_selector_active {
                    base_style
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
        } else if display.is_empty() {
            spans.push(Span::styled(
                "(empty)",
                if is_selector_active {
                    base_style
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
        } else {
            spans.push(Span::styled(display, base_style));
        }

        if field.required && !is_editing {
            spans.push(Span::styled(
                " *",
                if is_selector_active {
                    base_style
                } else {
                    Style::default().fg(Color::Red)
                },
            ));
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

fn draw_selector_popup(frame: &mut ratatui::Frame<'_>, state: &SelectorState, area: Rect) {
    let title = match state.field_key.as_str() {
        "PROVIDER" => "Select Provider",
        "MODEL" => "Select Model",
        _ => "Select",
    };

    let filtered = filtered_items(&state.items, &state.filter);

    let popup_width = (area.width as usize).clamp(40, 70);
    let popup_height = (7 + filtered.len()).clamp(10, 20) as u16;
    let max_list_height = (popup_height as usize).saturating_sub(7);

    let popup_x = (area.width as usize).saturating_sub(popup_width) / 2;
    let popup_y = (area.height as usize).saturating_sub(popup_height as usize) / 2;

    let popup_area = Rect::new(
        popup_x as u16,
        popup_y as u16,
        popup_width as u16,
        popup_height,
    );

    let inner_width = popup_width.saturating_sub(2);

    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "─".repeat(inner_width),
        Style::default().fg(Color::DarkGray),
    )]));

    let filter_display = format!("Filter: {}", state.filter);
    lines.push(Line::from(vec![Span::styled(
        filter_display,
        Style::default().fg(Color::White),
    )]));

    lines.push(Line::from(vec![Span::styled(
        "─".repeat(inner_width),
        Style::default().fg(Color::DarkGray),
    )]));

    if filtered.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "No matches. Enter to use as free input.",
            Style::default().fg(Color::Yellow),
        )]));
    } else {
        let mut window_start = 0usize;
        if state.selected >= max_list_height {
            window_start = state.selected - max_list_height + 1;
        }
        let window_end = (window_start + max_list_height).min(filtered.len());

        for (i, item) in filtered
            .iter()
            .enumerate()
            .skip(window_start)
            .take(window_end - window_start)
        {
            let is_selected = i == state.selected;
            let prefix = if is_selected { "▸ " } else { "  " };
            let display_text = if item.display.chars().count() > inner_width.saturating_sub(4) {
                let truncated: String = item
                    .display
                    .chars()
                    .take(inner_width.saturating_sub(5))
                    .collect();
                format!("{truncated}…")
            } else {
                item.display.clone()
            };

            if is_selected {
                lines.push(Line::from(vec![Span::styled(
                    format!("{prefix}{display_text}"),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )]));
            } else {
                lines.push(Line::from(vec![Span::styled(
                    format!("{prefix}{display_text}"),
                    Style::default().fg(Color::White),
                )]));
            }
        }
    }

    let remaining = (popup_height as usize).saturating_sub(lines.len() + 3);
    for _ in 0..remaining {
        lines.push(Line::from(""));
    }

    let match_info = format!(
        "{} matches │ Esc:cancel Enter:select ↑↓:navigate",
        filtered.len()
    );
    lines.push(Line::from(vec![Span::styled(
        match_info,
        Style::default().fg(Color::DarkGray),
    )]));

    let popup = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(popup, popup_area);

    let filter_cursor_x = popup_area.x + 1 + 8 + state.filter.chars().count() as u16;
    let filter_cursor_y = popup_area.y + 3;
    frame.set_cursor_position(Position::new(filter_cursor_x, filter_cursor_y));
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

            match app.mode {
                SetupMode::Selector(ref mut state) => {
                    match key.code {
                        KeyCode::Esc => {
                            if let Some(field) =
                                app.fields.iter_mut().find(|f| f.key == state.field_key)
                            {
                                field.value = state.original_value.clone();
                            }
                            app.mode = SetupMode::Navigate;
                            app.status =
                                "Enter: edit | Up/Down: navigate | Ctrl+S: save & exit | Ctrl+C: cancel"
                                    .into();
                        }
                        KeyCode::Enter => {
                            let filtered = filtered_items(&state.items, &state.filter);
                            if filtered.is_empty() {
                                if let Some(field) =
                                    app.fields.iter_mut().find(|f| f.key == state.field_key)
                                {
                                    field.value = state.filter.clone();
                                }
                            } else {
                                state.selected =
                                    (state.selected).min(filtered.len() - 1);
                                let selected_value = filtered[state.selected].value.clone();
                                if let Some(field) =
                                    app.fields.iter_mut().find(|f| f.key == state.field_key)
                                {
                                    field.value = selected_value;
                                }
                            }
                            let field_key = state.field_key.clone();
                            app.apply_selector_selection(&field_key);
                            app.mode = SetupMode::Navigate;
                            app.status =
                                "Enter: edit | Up/Down: navigate | Ctrl+S: save & exit | Ctrl+C: cancel"
                                    .into();
                        }
                        KeyCode::Up | KeyCode::Char('k')
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            let filtered = filtered_items(&state.items, &state.filter);
                            if !filtered.is_empty() && state.selected > 0 {
                                state.selected -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j')
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            let filtered = filtered_items(&state.items, &state.filter);
                            if !filtered.is_empty() {
                                state.selected =
                                    (state.selected + 1).min(filtered.len() - 1);
                            }
                        }
                        KeyCode::Backspace => {
                            state.filter.pop();
                            let filtered = filtered_items(&state.items, &state.filter);
                            if !filtered.is_empty() {
                                state.selected = state.selected.min(filtered.len() - 1);
                            } else {
                                state.selected = 0;
                            }
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.filter.push(c);
                            let filtered = filtered_items(&state.items, &state.filter);
                            if !filtered.is_empty() {
                                state.selected = state.selected.min(filtered.len() - 1);
                            } else {
                                state.selected = 0;
                            }
                        }
                        _ => {}
                    }
                }
                SetupMode::Edit => {
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter => {
                            if let Some(field) = app.current_field() {
                                if field.key == "DISCORD_ENABLED"
                                    || field.key == "TELEGRAM_ENABLED"
                                {
                                    SetupApp::update_field_visibility(&mut app.fields);
                                }
                            }
                            app.mode = SetupMode::Navigate;
                            app.status =
                                "Enter: edit | Up/Down: navigate | Ctrl+S: save & exit | Ctrl+C: cancel"
                                    .into();
                        }
                        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            match app.save() {
                                Ok(()) => {
                                    app.mode = SetupMode::Navigate;
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
                }
                SetupMode::Navigate => {
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
                            if let Some(field) = app.current_field() {
                                let key_name = field.key.clone();
                                match key_name.as_str() {
                                    "PROVIDER" | "MODEL" => {
                                        app.mode = SetupMode::Selector(
                                            app.enter_selector(&key_name),
                                        );
                                        app.status = "Selector: type to filter, Enter: select, Esc: cancel"
                                            .into();
                                    }
                                    _ => {
                                        app.mode = SetupMode::Edit;
                                        app.status =
                                            "Editing... (Enter/Esc to finish)".into();
                                    }
                                }
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k')
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            app.move_selection(-1);
                        }
                        KeyCode::Down | KeyCode::Char('j')
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            app.move_selection(1);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SetupApp, filtered_items, model_selector_items, provider_selector_items};
    use super::{SelectorItem, SelectorState, SetupMode};
    use std::fs;

    #[test]
    fn load_existing_config_prefers_new_provider_schema() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");
        fs::write(
            &config_path,
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
    models:
      - gpt-4o-mini
      - gpt-5
channels:
  web:
    enabled: true
    auth_token: web-token
"#,
        )
        .expect("write config");

        let (existing, _) = SetupApp::load_existing_config(&config_path);

        assert_eq!(existing.get("PROVIDER"), Some(&"openai".to_string()));
        assert_eq!(existing.get("MODEL"), Some(&"gpt-4o-mini".to_string()));
        assert_eq!(
            existing.get("BASE_URL"),
            Some(&"https://api.openai.com/v1".to_string())
        );
        assert_eq!(existing.get("API_KEY"), Some(&"sk-openai".to_string()));
        assert_eq!(
            existing.get("WEB_AUTH_TOKEN"),
            Some(&"web-token".to_string())
        );
    }

    #[test]
    fn load_existing_config_ignores_legacy_top_level_llm_fields() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");
        fs::write(
            &config_path,
            r#"model: gpt-4o-mini
base_url: https://api.openai.com/v1
api_key: sk-legacy
"#,
        )
        .expect("write config");

        let (existing, _) = SetupApp::load_existing_config(&config_path);

        assert!(!existing.contains_key("PROVIDER"));
        assert!(!existing.contains_key("MODEL"));
        assert!(!existing.contains_key("BASE_URL"));
        assert!(!existing.contains_key("API_KEY"));
    }

    #[test]
    fn filtered_items_returns_all_when_filter_empty() {
        let items = vec![
            SelectorItem {
                display: "openai (gpt-5.2, gpt-5)".into(),
                value: "openai".into(),
            },
            SelectorItem {
                display: "ollama (llama3.2)".into(),
                value: "ollama".into(),
            },
        ];
        let result = filtered_items(&items, "");
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filtered_items_matches_substring_case_insensitive() {
        let items = vec![
            SelectorItem {
                display: "openai (gpt-5.2, gpt-5)".into(),
                value: "openai".into(),
            },
            SelectorItem {
                display: "Ollama (local)".into(),
                value: "ollama".into(),
            },
            SelectorItem {
                display: "OpenRouter".into(),
                value: "openrouter".into(),
            },
        ];
        let result = filtered_items(&items, "OPEN");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].value, "openai");
        assert_eq!(result[1].value, "openrouter");
    }

    #[test]
    fn filtered_items_returns_none_when_no_match() {
        let items = vec![SelectorItem {
            display: "openai".into(),
            value: "openai".into(),
        }];
        let result = filtered_items(&items, "zzzzz");
        assert!(result.is_empty());
    }

    #[test]
    fn provider_selector_items_includes_key_presets() {
        let items = provider_selector_items();
        assert!(!items.is_empty());
        assert!(items.iter().any(|i| i.value == "openai"));
        assert!(items.iter().any(|i| i.value == "custom"));
    }

    #[test]
    fn model_selector_items_returns_models_for_known_provider() {
        let items = model_selector_items("openai");
        assert!(!items.is_empty());
        assert!(items.iter().any(|i| i.value == "gpt-5.2"));
    }

    #[test]
    fn model_selector_items_returns_empty_for_unknown_provider() {
        let items = model_selector_items("nonexistent");
        assert!(items.is_empty());
    }

    #[test]
    fn setup_mode_navigate_default() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");
        let app = SetupApp::new(Some(config_path));
        assert!(matches!(app.mode, SetupMode::Navigate));
    }

    #[test]
    fn selector_state_holds_original_value() {
        let state = SelectorState {
            field_key: "PROVIDER".into(),
            filter: String::new(),
            items: vec![],
            selected: 0,
            original_value: "openai".into(),
        };
        assert_eq!(state.field_key, "PROVIDER");
        assert_eq!(state.original_value, "openai");
    }
}

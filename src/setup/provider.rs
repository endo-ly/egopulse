use super::{SelectorItem, SelectorState, SetupApp};

#[derive(Clone, Copy)]
pub(crate) struct ProviderPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub default_base_url: &'static str,
    pub default_model: &'static str,
    pub models: &'static [&'static str],
}

pub(crate) const PROVIDER_PRESETS: &[ProviderPreset] = &[
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
        models: &["glm-5.1", "glm-5", "glm-4.7"],
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
        id: "lmstudio",
        label: "LM Studio (local)",
        default_base_url: "",
        default_model: "custom-model",
        models: &["google/gemma-4-e4b"],
    },
];

pub(crate) fn find_provider_preset(provider: &str) -> Option<&'static ProviderPreset> {
    PROVIDER_PRESETS
        .iter()
        .find(|preset| preset.id.eq_ignore_ascii_case(provider))
}

pub(crate) fn provider_default_base_url(provider: &str) -> Option<&'static str> {
    find_provider_preset(provider)
        .map(|preset| preset.default_base_url)
        .filter(|value| !value.is_empty())
}

pub(crate) fn provider_default_model(provider: &str) -> Option<&'static str> {
    find_provider_preset(provider).map(|preset| preset.default_model)
}

pub(crate) fn provider_label_for(provider: &str) -> String {
    find_provider_preset(provider)
        .map(|preset| preset.label.to_string())
        .unwrap_or_else(|| provider.to_string())
}

pub(crate) fn provider_choices() -> String {
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

pub(crate) fn normalize_provider_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if find_provider_preset(trimmed).is_some() {
        return trimmed.to_ascii_lowercase();
    }
    trimmed.to_string()
}

pub(crate) fn provider_selector_items() -> Vec<SelectorItem> {
    PROVIDER_PRESETS
        .iter()
        .map(|preset| SelectorItem {
            display: format!("{} ({})", preset.id, preset_models_preview(preset)),
            value: preset.id.to_string(),
        })
        .collect()
}

fn preset_models_preview(preset: &ProviderPreset) -> String {
    if preset.models.len() <= 2 {
        return preset.models.join(", ");
    }

    format!(
        "{}, ... ({} total)",
        preset.models[..2].join(", "),
        preset.models.len()
    )
}

pub(crate) fn model_selector_items(provider_id: &str) -> Vec<SelectorItem> {
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

impl SetupApp {
    pub(crate) fn enter_selector(&self, field_key: &str) -> SelectorState {
        let items = match field_key {
            "PROVIDER" => provider_selector_items(),
            "MODEL" => model_selector_items(self.provider_field_value()),
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

    pub(crate) fn apply_selector_selection(&mut self, field_key: &str) {
        if field_key != "PROVIDER" {
            return;
        }

        let Some(preset) = find_provider_preset(self.provider_field_value()) else {
            return;
        };

        if let Some(model_field) = self.fields.iter_mut().find(|f| f.key == "MODEL") {
            model_field.value = preset.default_model.to_string();
        }
        if let Some(url_field) = self.fields.iter_mut().find(|f| f.key == "BASE_URL") {
            url_field.value = preset.default_base_url.to_string();
        }
    }

    fn provider_field_value(&self) -> &str {
        self.fields
            .iter()
            .find(|f| f.key == "PROVIDER")
            .map(|f| f.value.as_str())
            .unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::{model_selector_items, provider_selector_items};

    #[test]
    fn provider_selector_items_includes_key_presets() {
        let items = provider_selector_items();
        assert!(!items.is_empty());
        assert!(items.iter().any(|i| i.value == "openai"));
        assert!(items.iter().any(|i| i.value == "lmstudio"));
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
}

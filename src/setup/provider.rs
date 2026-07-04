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
        id: "openai-codex",
        label: "OpenAI Codex (OAuth)",
        default_base_url: "https://chatgpt.com/backend-api/codex",
        default_model: "gpt-5.3-codex",
        models: &["gpt-5.3-codex", "codex-mini"],
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
        default_base_url: "http://127.0.0.1:1234/v1",
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

#[cfg(test)]
mod tests {
    #[test]
    fn find_provider_preset_matches_known_id() {
        assert!(super::find_provider_preset("openai").is_some());
        assert!(super::find_provider_preset("lmstudio").is_some());
    }

    #[test]
    fn find_provider_preset_returns_none_for_unknown() {
        assert!(super::find_provider_preset("nonexistent").is_none());
    }

    #[test]
    fn provider_default_base_url_returns_lmstudio_default() {
        assert_eq!(
            super::provider_default_base_url("lmstudio"),
            Some("http://127.0.0.1:1234/v1")
        );
    }

    #[test]
    fn normalize_provider_id_lowercases_known_preset() {
        assert_eq!(super::normalize_provider_id("OpenAI"), "openai");
    }
}

//! Bundled metadata for well-known provider/model pairs.
//!
//! The table is compiled into the binary so startup, every turn, and offline
//! runs resolve the same limits without a network request. A model that is not
//! listed falls back to the custom-model defaults and is reported as such;
//! users can override the context window or the output limit per model in
//! `providers.<id>.models.<model>`.
//!
//! The values are a snapshot of the providers' published limits. Update an
//! entry when the provider changes a model; an unknown model is never silently
//! assigned another model's limits.

/// Metadata of one known model.
struct CatalogModel {
    /// Canonical provider key (after alias resolution).
    provider: &'static str,
    /// Model id sent to the provider.
    model: &'static str,
    /// Maximum input + output tokens.
    context_window: usize,
    /// Maximum output tokens before any context-based clamp.
    max_output_tokens: usize,
}

/// Known provider/model metadata.
///
/// Only entries whose limits are known are listed. Leaving a model out is
/// safer than guessing: it gets the custom-model defaults, which the user can
/// override explicitly.
const CATALOG: &[CatalogModel] = &[
    // OpenAI (also used for the Codex endpoint and Azure deployments).
    catalog("openai", "chat-latest", 400_000, 128_000),
    catalog("openai", "gpt-4.1", 1_047_576, 32_768),
    catalog("openai", "gpt-4.1-mini", 1_047_576, 32_768),
    catalog("openai", "gpt-4.1-nano", 1_047_576, 32_768),
    catalog("openai", "gpt-4o", 128_000, 16_384),
    catalog("openai", "gpt-4o-mini", 128_000, 16_384),
    catalog("openai", "gpt-5", 400_000, 128_000),
    catalog("openai", "gpt-5-mini", 400_000, 128_000),
    catalog("openai", "gpt-5-nano", 400_000, 128_000),
    catalog("openai", "gpt-5.1", 400_000, 128_000),
    catalog("openai", "gpt-5.2", 400_000, 128_000),
    catalog("openai", "gpt-5.2-pro", 400_000, 128_000),
    catalog("openai", "gpt-5.3-codex", 400_000, 128_000),
    catalog("openai", "gpt-5.3-codex-spark", 128_000, 32_000),
    catalog("openai", "gpt-5.4", 272_000, 128_000),
    catalog("openai", "gpt-5.4-mini", 400_000, 128_000),
    catalog("openai", "gpt-5.5", 272_000, 128_000),
    catalog("openai", "o1", 200_000, 100_000),
    catalog("openai", "o3", 200_000, 100_000),
    catalog("openai", "o3-mini", 200_000, 100_000),
    catalog("openai", "o3-pro", 200_000, 100_000),
    catalog("openai", "o4-mini", 200_000, 100_000),
    // OpenRouter passes the upstream model limits through.
    catalog("openrouter", "openrouter/auto", 2_000_000, 4_096),
    catalog("openrouter", "openai/gpt-5", 400_000, 128_000),
    catalog("openrouter", "openai/gpt-5.2", 400_000, 128_000),
    catalog(
        "openrouter",
        "anthropic/claude-sonnet-4.5",
        1_000_000,
        64_000,
    ),
    catalog(
        "openrouter",
        "anthropic/claude-opus-4.6",
        1_000_000,
        128_000,
    ),
    catalog("openrouter", "google/gemini-2.5-pro", 1_048_576, 65_536),
    catalog("openrouter", "google/gemini-2.5-flash", 1_048_576, 65_535),
    catalog("openrouter", "deepseek/deepseek-chat", 128_000, 16_000),
    catalog("openrouter", "qwen/qwen3-coder", 262_144, 65_536),
    catalog(
        "openrouter",
        "meta-llama/llama-3.3-70b-instruct",
        131_072,
        16_384,
    ),
    catalog("openrouter", "mistralai/mistral-large", 128_000, 102_400),
    catalog("openrouter", "moonshotai/kimi-k2.5", 262_144, 4_096),
    catalog("openrouter", "minimax/minimax-m2.5", 198_000, 32_768),
    catalog("openrouter", "z-ai/glm-4.7", 202_752, 131_072),
    // Anthropic's OpenAI-compatible endpoint.
    catalog("anthropic", "claude-sonnet-4-5", 200_000, 64_000),
    catalog("anthropic", "claude-sonnet-4-6", 1_000_000, 128_000),
    catalog("anthropic", "claude-opus-4-5", 200_000, 64_000),
    catalog("anthropic", "claude-opus-4-6", 1_000_000, 128_000),
    catalog("anthropic", "claude-haiku-4-5", 200_000, 64_000),
    // Google Gemini.
    catalog("google", "gemini-2.5-pro", 1_048_576, 65_536),
    catalog("google", "gemini-2.5-flash", 1_048_576, 65_536),
    catalog("google", "gemini-2.5-flash-lite", 1_048_576, 65_536),
    catalog("google", "gemini-3-flash-preview", 1_048_576, 65_536),
    catalog("google", "gemini-3.1-pro-preview", 1_048_576, 65_536),
    catalog("google", "gemini-pro-latest", 1_048_576, 65_536),
    catalog("google", "gemini-flash-latest", 1_048_576, 65_536),
    catalog("google", "gemma-4-31b-it", 262_144, 32_768),
    // DeepSeek.
    catalog("deepseek", "deepseek-chat", 131_072, 8_192),
    catalog("deepseek", "deepseek-reasoner", 131_072, 65_536),
    catalog("deepseek", "deepseek-v3", 131_072, 8_192),
    catalog("deepseek", "deepseek-v4-flash", 1_000_000, 384_000),
    catalog("deepseek", "deepseek-v4-pro", 1_000_000, 384_000),
    // Qwen (DashScope, Bailian, Qwen Portal).
    catalog("qwen", "qwen3-max", 262_144, 65_536),
    catalog("qwen", "qwen-plus-latest", 131_072, 8_192),
    catalog("qwen", "qwen3.5-plus", 262_144, 65_536),
    catalog("qwen", "qwen3.6-plus", 1_000_000, 65_536),
    catalog("qwen", "qwen3.7-max", 1_000_000, 131_072),
    catalog("qwen", "qwen3.7-plus", 1_000_000, 65_536),
    catalog("qwen", "qwen3.8-max", 1_000_000, 131_072),
    // Moonshot / Kimi.
    catalog("moonshotai", "kimi-k2.5", 262_144, 65_536),
    catalog("moonshotai", "kimi-k2.6", 262_144, 262_144),
    catalog("moonshotai", "kimi-k3", 1_048_576, 1_048_576),
    // MiniMax.
    catalog("minimax", "MiniMax-M2.5", 196_608, 32_768),
    catalog("minimax", "MiniMax-M2.5-Thinking", 196_608, 32_768),
    catalog("minimax", "MiniMax-M2.7", 204_800, 131_072),
    catalog("minimax", "MiniMax-M3", 1_000_000, 524_288),
    // Z.AI / Zhipu GLM.
    catalog("zai", "glm-4.7", 204_800, 131_072),
    catalog("zai", "glm-5", 202_752, 16_384),
    catalog("zai", "glm-5.1", 202_752, 128_000),
    catalog("zai", "glm-5.2", 1_000_000, 131_072),
    catalog("zai", "glm-5.3", 1_000_000, 131_072),
    // Mistral.
    catalog("mistral", "mistral-large-latest", 262_144, 262_144),
    catalog("mistral", "mistral-medium-latest", 262_144, 262_144),
    catalog("mistral", "ministral-8b-latest", 262_144, 128_000),
    // xAI Grok.
    catalog("xai", "grok-3", 131_072, 8_192),
    catalog("xai", "grok-4", 256_000, 64_000),
    catalog("xai", "grok-4-fast", 2_000_000, 30_000),
    catalog("xai", "grok-4.3", 1_000_000, 30_000),
    catalog("xai", "grok-4.5", 500_000, 500_000),
    // Amazon Bedrock (model ids include the vendor prefix).
    catalog(
        "amazon-bedrock",
        "anthropic.claude-opus-4-6-v1",
        1_000_000,
        128_000,
    ),
    catalog(
        "amazon-bedrock",
        "anthropic.claude-sonnet-4-5-v2",
        200_000,
        64_000,
    ),
    catalog(
        "amazon-bedrock",
        "anthropic.claude-haiku-4-5-v1",
        200_000,
        64_000,
    ),
    catalog("amazon-bedrock", "amazon.nova-pro-v1:0", 300_000, 10_000),
    catalog("amazon-bedrock", "amazon.nova-lite-v1:0", 300_000, 10_000),
    // Open-weight models served by hosted providers.
    catalog("huggingface", "Qwen/Qwen3-Coder-Next", 262_144, 65_536),
    catalog(
        "huggingface",
        "meta-llama/Llama-3.3-70B-Instruct",
        131_072,
        4_096,
    ),
    catalog("huggingface", "deepseek-ai/DeepSeek-V3", 64_000, 8_192),
    catalog(
        "huggingface",
        "Qwen/Qwen3-Coder-480B-A35B-Instruct",
        262_144,
        65_536,
    ),
    catalog("together", "deepseek-ai/DeepSeek-V3", 64_000, 8_192),
    catalog("together", "Qwen/Qwen3.7-Max", 1_000_000, 131_072),
    catalog("nvidia", "openai/gpt-oss-20b", 131_072, 32_768),
    catalog(
        "nvidia",
        "nvidia/llama-3.1-nemotron-ultra-253b-v1",
        128_000,
        16_384,
    ),
];

const fn catalog(
    provider: &'static str,
    model: &'static str,
    context_window: usize,
    max_output_tokens: usize,
) -> CatalogModel {
    CatalogModel {
        provider,
        model,
        context_window,
        max_output_tokens,
    }
}

/// Returns the catalog metadata for a provider/model pair.
///
/// The provider is matched by its canonical name or a recognized alias; the
/// model id must match exactly so two providers cannot claim the same limits
/// through a shared id.
pub(crate) fn lookup(provider: &str, model: &str) -> Option<(usize, usize)> {
    let provider = canonical_provider(provider)?;
    CATALOG
        .iter()
        .find(|entry| entry.provider == provider && entry.model == model)
        .map(|entry| (entry.context_window, entry.max_output_tokens))
}

/// Maps a configured or preset provider id to the catalog provider it uses.
fn canonical_provider(provider: &str) -> Option<&'static str> {
    Some(match provider.trim().to_ascii_lowercase().as_str() {
        "openai" | "openai-codex" | "azure" | "azure-openai" => "openai",
        "openrouter" => "openrouter",
        "anthropic" | "claude" => "anthropic",
        "google" | "google-vertex" | "gemini" | "google-deepmind" => "google",
        "deepseek" => "deepseek",
        "qwen" | "qwen-portal" | "alibaba" | "aliyun-bailian" | "dashscope" => "qwen",
        "moonshot" | "moonshotai" | "kimi" | "kimi-coding" => "moonshotai",
        "minimax" | "minimax-cn" => "minimax",
        "zai" | "z-ai" | "zhipu" | "zai-coding" | "zai-coding-cn" => "zai",
        "mistral" | "mistral-ai" => "mistral",
        "xai" | "x-ai" | "grok" => "xai",
        "bedrock" | "amazon-bedrock" | "aws-bedrock" => "amazon-bedrock",
        "huggingface" | "hf" => "huggingface",
        "together" | "together-ai" => "together",
        "nvidia" | "nim" => "nvidia",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_keep_their_provider_specific_limits() {
        // Act
        let openai = lookup("openai", "gpt-4o");
        let anthropic = lookup("anthropic", "claude-sonnet-4-5");
        let google = lookup("google", "gemini-2.5-pro");
        let deepseek = lookup("deepseek", "deepseek-chat");

        // Assert
        assert_eq!(openai, Some((128_000, 16_384)));
        assert_eq!(anthropic, Some((200_000, 64_000)));
        assert_eq!(google, Some((1_048_576, 65_536)));
        assert_eq!(deepseek, Some((131_072, 8_192)));
    }

    #[test]
    fn provider_aliases_resolve_to_the_same_catalog() {
        // Act
        let azure = lookup("Azure", "gpt-4.1");
        let codex = lookup("openai-codex", "gpt-5.3-codex");
        let zhipu = lookup("zhipu", "glm-4.7");
        let bailian = lookup("aliyun-bailian", "qwen3.5-plus");

        // Assert
        assert_eq!(azure, Some((1_047_576, 32_768)));
        assert_eq!(codex, Some((400_000, 128_000)));
        assert_eq!(zhipu, Some((204_800, 131_072)));
        assert_eq!(bailian, Some((262_144, 65_536)));
    }

    #[test]
    fn unknown_provider_or_model_has_no_catalog_entry() {
        // Act & Assert
        assert_eq!(lookup("my-gateway", "gpt-4o"), None);
        assert_eq!(lookup("openai", "gpt-4o-on-my-gateway"), None);
        assert_eq!(lookup("ollama", "llama3.2"), None);
    }
}

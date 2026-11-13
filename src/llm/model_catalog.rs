//! Bundled metadata for known provider/model pairs.
//!
//! The table is generated from the reference implementation's bundled provider
//! data (MIT) by
//! `scripts/import-model-catalog.mjs` and compiled into the binary, so startup,
//! every turn, and offline runs resolve the same limits without a network
//! request. A model that is not listed falls back to the custom-model defaults
//! and is reported as such; users can override the context window or the output
//! limit per model in `providers.<id>.models.<model>`.
//!
//! Updating the catalog means regenerating the JSON, not editing model entries
//! by hand. Dynamic routing models such as `openrouter/auto` are excluded: a
//! fixed window cannot describe where the router will send the request.

use std::collections::HashMap;
use std::sync::LazyLock;

const CATALOG_JSON: &str = include_str!("model_catalog.json");

/// Limits of one imported model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Limits {
    /// Maximum input + output tokens.
    context_window: usize,
    /// Maximum output tokens before any context-based clamp.
    max_output_tokens: usize,
}

#[derive(serde::Deserialize)]
struct CatalogFile {
    /// Source package and version the generated file was built from.
    source: String,
    models: Vec<(String, String, usize, usize)>,
}

/// Imported models keyed by provider and then by model id.
static CATALOG: LazyLock<HashMap<String, HashMap<String, Limits>>> = LazyLock::new(|| {
    let file: CatalogFile =
        serde_json::from_str(CATALOG_JSON).expect("the bundled model catalog must be valid JSON");
    assert!(
        file.source.starts_with("@earendil-works/pi-ai@"),
        "the bundled model catalog must record its source package"
    );
    let mut catalog: HashMap<String, HashMap<String, Limits>> = HashMap::new();
    for (provider, model, context_window, max_output_tokens) in file.models {
        catalog.entry(provider).or_default().insert(
            model,
            Limits {
                context_window,
                max_output_tokens,
            },
        );
    }
    catalog
});

/// Returns the catalog metadata for a provider/model pair.
///
/// The provider is matched by its configured id first, then by a recognized
/// alias. The model id must match exactly so two providers cannot claim the
/// same limits through a shared id.
pub(crate) fn lookup(provider: &str, model: &str) -> Option<(usize, usize)> {
    let provider = provider.trim().to_ascii_lowercase();
    if let Some(limits) = CATALOG.get(&provider).and_then(|models| models.get(model)) {
        return Some((limits.context_window, limits.max_output_tokens));
    }
    let canonical = canonical_provider(&provider);
    if canonical == provider {
        return None;
    }
    let limits = CATALOG.get(&canonical)?.get(model)?;
    Some((limits.context_window, limits.max_output_tokens))
}

/// Maps a provider id used by EgoPulse to the provider key in the imported data.
///
/// Every other id is looked up as-is; custom providers that use a provider id
/// known to the source data therefore resolve without an entry here.
fn canonical_provider(provider: &str) -> String {
    match provider {
        "openai" | "openai-codex" | "azure" | "azure-openai" => "openai",
        "qwen" | "qwen-portal" | "alibaba" | "aliyun-bailian" | "dashscope" => "qwen-token-plan",
        "moonshot" | "kimi" | "kimi-coding" => "moonshotai",
        "zhipu" | "z-ai" | "zai-coding" => "zai",
        "bedrock" | "aws-bedrock" => "amazon-bedrock",
        "mistral-ai" => "mistral",
        "x-ai" | "grok" => "xai",
        "hf" => "huggingface",
        "together-ai" => "together",
        "nim" => "nvidia",
        "gemini" | "google-deepmind" => "google",
        "claude" => "anthropic",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_maps_known_models_and_provider_aliases() {
        // Pick any entry so a catalog refresh does not invalidate the test.
        let (model, limits) = CATALOG
            .get("anthropic")
            .and_then(|models| models.iter().next())
            .expect("the catalog contains an Anthropic model");
        let expected = Some((limits.context_window, limits.max_output_tokens));
        assert_eq!(lookup("Anthropic", model), expected);
        assert_eq!(lookup("claude", model), expected);

        let (zai_model, zai_limits) = CATALOG
            .get("zai")
            .and_then(|models| models.iter().next())
            .expect("the catalog contains a z.ai model");
        assert_eq!(
            lookup("zhipu", zai_model),
            Some((zai_limits.context_window, zai_limits.max_output_tokens))
        );
    }
}

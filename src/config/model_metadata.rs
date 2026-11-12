//! Resolves the limits used to size a request and to trigger compaction.
//!
//! Three sources apply, in order: an explicit per-model setting, the bundled
//! catalog, and the custom-model defaults. The defaults are a defined value for
//! models the binary does not know, not a global fallback that hides every known
//! model's limit.

use crate::llm::model_catalog;

use super::{ProviderConfig, ProviderId};

/// Context window assumed for a model that is neither configured nor known.
///
/// This is the custom-model default: a model definition without metadata is
/// treated exactly like a user-defined model with these limits.
pub(crate) const CUSTOM_MODEL_CONTEXT_WINDOW_TOKENS: usize = 128_000;

/// Output limit assumed for a model that is neither configured nor known.
pub(crate) const CUSTOM_MODEL_MAX_OUTPUT_TOKENS: usize = 16_384;

/// Where one resolved value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelValueSource {
    /// `providers.<provider>.models.<model>` in the configuration.
    Configured,
    /// The bundled provider/model catalog.
    Catalog,
    /// The custom-model defaults, because nothing else matched.
    Default,
}

impl ModelValueSource {
    /// Returns `true` when the value is the custom-model default.
    pub(crate) fn is_default(self) -> bool {
        matches!(self, Self::Default)
    }
}

/// Limits applied to one provider/model pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModelMetadata {
    /// Maximum input + output tokens.
    pub(crate) context_window: usize,
    /// Maximum output tokens before any context-based clamp.
    pub(crate) max_output_tokens: usize,
    /// Source of `context_window`.
    pub(crate) context_window_source: ModelValueSource,
    /// Source of `max_output_tokens`.
    pub(crate) max_output_tokens_source: ModelValueSource,
}

impl Default for ModelMetadata {
    fn default() -> Self {
        Self {
            context_window: CUSTOM_MODEL_CONTEXT_WINDOW_TOKENS,
            max_output_tokens: CUSTOM_MODEL_MAX_OUTPUT_TOKENS,
            context_window_source: ModelValueSource::Default,
            max_output_tokens_source: ModelValueSource::Default,
        }
    }
}

impl ModelMetadata {
    /// Metadata for a provider/model pair.
    pub(crate) fn resolve(provider_id: &ProviderId, model: &str) -> Self {
        let catalog = model_catalog::lookup(provider_id.as_str(), model);
        Self::build(None, catalog, None)
    }
}

/// Resolves the limits of one model from its configuration and the catalog.
pub(crate) fn resolve_for_provider(
    provider_id: &str,
    provider: &ProviderConfig,
    model: &str,
) -> ModelMetadata {
    let configured = provider.models.get(model);
    ModelMetadata::build(
        configured.and_then(|config| config.context_window_tokens),
        model_catalog::lookup(provider_id, model),
        configured.and_then(|config| config.max_output_tokens),
    )
}

impl ModelMetadata {
    fn build(
        configured_context_window: Option<usize>,
        catalog: Option<(usize, usize)>,
        configured_max_output_tokens: Option<usize>,
    ) -> Self {
        let (context_window, context_window_source) = match configured_context_window {
            Some(value) => (value, ModelValueSource::Configured),
            None => match catalog {
                Some((value, _)) => (value, ModelValueSource::Catalog),
                None => (
                    CUSTOM_MODEL_CONTEXT_WINDOW_TOKENS,
                    ModelValueSource::Default,
                ),
            },
        };
        let (max_output_tokens, max_output_tokens_source) = match configured_max_output_tokens {
            Some(value) => (value, ModelValueSource::Configured),
            None => match catalog {
                Some((_, value)) => (value, ModelValueSource::Catalog),
                None => (CUSTOM_MODEL_MAX_OUTPUT_TOKENS, ModelValueSource::Default),
            },
        };
        Self {
            context_window,
            max_output_tokens,
            context_window_source,
            max_output_tokens_source,
        }
    }

    /// Returns `true` when either limit is the custom-model default.
    pub(crate) fn uses_custom_defaults(&self) -> bool {
        self.context_window_source.is_default() || self.max_output_tokens_source.is_default()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::types::ModelConfig;

    fn provider(models: &[(&str, ModelConfig)]) -> ProviderConfig {
        let models = models
            .iter()
            .map(|(name, config)| ((*name).to_string(), config.clone()))
            .collect::<HashMap<_, _>>();
        ProviderConfig::new(
            "Test".to_string(),
            "https://example.com/v1".to_string(),
            None,
            "test-model".to_string(),
            models,
        )
    }

    #[test]
    fn catalog_values_apply_without_configuration() {
        // Arrange
        let provider = provider(&[]);

        // Act
        let metadata = resolve_for_provider("openai", &provider, "gpt-4o");

        // Assert
        assert_eq!(metadata.context_window, 128_000);
        assert_eq!(metadata.max_output_tokens, 16_384);
        assert_eq!(metadata.context_window_source, ModelValueSource::Catalog);
        assert!(!metadata.uses_custom_defaults());
    }

    #[test]
    fn configured_values_win_over_the_catalog() {
        // Arrange
        let provider = provider(&[(
            "gpt-4o",
            ModelConfig {
                context_window_tokens: Some(32_768),
                max_output_tokens: Some(4_096),
                ..ModelConfig::default()
            },
        )]);

        // Act
        let metadata = resolve_for_provider("openai", &provider, "gpt-4o");

        // Assert
        assert_eq!(metadata.context_window, 32_768);
        assert_eq!(metadata.max_output_tokens, 4_096);
        assert_eq!(metadata.context_window_source, ModelValueSource::Configured);
        assert_eq!(
            metadata.max_output_tokens_source,
            ModelValueSource::Configured
        );
    }

    #[test]
    fn a_single_configured_field_keeps_the_catalog_value_for_the_other() {
        // Arrange
        let provider = provider(&[(
            "gpt-4o",
            ModelConfig {
                context_window_tokens: Some(64_000),
                ..ModelConfig::default()
            },
        )]);

        // Act
        let metadata = resolve_for_provider("openai", &provider, "gpt-4o");

        // Assert
        assert_eq!(metadata.context_window, 64_000);
        assert_eq!(metadata.max_output_tokens, 16_384);
        assert_eq!(metadata.context_window_source, ModelValueSource::Configured);
        assert_eq!(metadata.max_output_tokens_source, ModelValueSource::Catalog);
    }

    #[test]
    fn an_unknown_model_uses_the_custom_defaults() {
        // Arrange
        let provider = provider(&[]);

        // Act
        let metadata = resolve_for_provider("my-gateway", &provider, "house-model");

        // Assert
        assert_eq!(metadata.context_window, CUSTOM_MODEL_CONTEXT_WINDOW_TOKENS);
        assert_eq!(metadata.max_output_tokens, CUSTOM_MODEL_MAX_OUTPUT_TOKENS);
        assert!(metadata.uses_custom_defaults());
    }
}

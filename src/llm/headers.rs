//! Provider header values that depend on the conversation.
//!
//! A `providers.<id>.headers` entry written without a value names a header
//! whose value must be unique per conversation: upstream gateways use such a
//! header for routing and prompt caching, so sharing one value across every
//! conversation pins all traffic to a single upstream assignment. The value is
//! a digest of the conversation identity, which keeps it stable across
//! restarts without storing anything.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::config::ResolvedLlmConfig;
use crate::conversation::SurfaceContext;

/// Digest bytes rendered as hex. Eight bytes keep the value short while
/// leaving collisions out of reach within one deployment.
const SESSION_VALUE_BYTES: usize = 8;

/// Conversation identity used to derive valueless header values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeaderContext {
    session_key: String,
}

impl HeaderContext {
    /// Uses the conversation identified by `context`.
    pub(crate) fn from_surface(context: &SurfaceContext) -> Self {
        Self {
            session_key: context.session_key(),
        }
    }

    /// Uses an agent-wide identity for work that has no conversation surface.
    pub(crate) fn for_agent(agent_id: &str) -> Self {
        Self {
            session_key: format!("agent:{agent_id}"),
        }
    }

    /// Uses a single process-wide identity.
    pub(crate) fn global() -> Self {
        Self {
            session_key: "global".to_string(),
        }
    }
}

fn derive_session_value(session_key: &str) -> String {
    let digest = Sha256::digest(session_key.as_bytes());
    digest[..SESSION_VALUE_BYTES]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Returns `headers` with every valueless entry filled in for `context`.
pub(crate) fn render_headers(
    headers: &BTreeMap<String, Option<String>>,
    context: &HeaderContext,
) -> BTreeMap<String, String> {
    let mut derived: Option<String> = None;
    headers
        .iter()
        .map(|(name, value)| {
            let value = match value {
                Some(value) => value.clone(),
                None => derived
                    .get_or_insert_with(|| derive_session_value(&context.session_key))
                    .clone(),
            };
            (name.clone(), value)
        })
        .collect()
}

/// Returns `config` with every valueless header replaced by its concrete value.
///
/// The result carries the same header keys and no `None` values, so it can be
/// used both for provider construction and for cache keying.
pub(crate) fn rendered_config(
    config: &ResolvedLlmConfig,
    context: &HeaderContext,
) -> ResolvedLlmConfig {
    let mut rendered = config.clone();
    rendered.headers = render_headers(&config.headers, context)
        .into_iter()
        .map(|(name, value)| (name, Some(value)))
        .collect();
    rendered
}

#[cfg(test)]
mod tests {
    use super::{HeaderContext, render_headers, rendered_config};
    use crate::config::ResolvedLlmConfig;
    use std::collections::BTreeMap;

    fn headers(entries: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.map(String::from)))
            .collect()
    }

    fn config(headers: BTreeMap<String, Option<String>>) -> ResolvedLlmConfig {
        ResolvedLlmConfig {
            provider: "provider".to_string(),
            label: "Provider".to_string(),
            base_url: "https://example.com/v1".to_string(),
            api_key: None,
            model: "model".to_string(),
            reasoning_effort: None,
            headers,
        }
    }

    #[test]
    fn valueless_header_becomes_sixteen_hex_characters() {
        // Arrange
        let context = HeaderContext::from_surface(&crate::test_util::cli_context("hex"));

        // Act
        let rendered = render_headers(&headers(&[("x-session", None)]), &context);

        // Assert
        let value = rendered.get("x-session").expect("rendered header");
        assert_eq!(value.len(), 16, "{value}");
        assert!(value.chars().all(|c| c.is_ascii_hexdigit()), "{value}");
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn same_conversation_renders_the_same_value() {
        // Arrange
        let context = HeaderContext::from_surface(&crate::test_util::cli_context("stable"));

        // Act
        let first = render_headers(&headers(&[("x-session", None)]), &context);
        let second = render_headers(&headers(&[("x-session", None)]), &context);

        // Assert
        assert_eq!(first, second);
    }

    #[test]
    fn different_conversations_render_different_values() {
        // Arrange
        let first = HeaderContext::for_agent("musa");
        let second = HeaderContext::for_agent("lyre");

        // Act
        let first = render_headers(&headers(&[("x-session", None)]), &first);
        let second = render_headers(&headers(&[("x-session", None)]), &second);

        // Assert
        assert_ne!(first["x-session"], second["x-session"]);
    }

    #[test]
    fn literal_values_are_sent_verbatim_and_keys_are_preserved() {
        // Arrange
        let context = HeaderContext::global();

        // Act
        let rendered = render_headers(
            &headers(&[("x-session", None), ("authorization", Some("Bearer t"))]),
            &context,
        );

        // Assert
        assert_eq!(
            rendered.keys().collect::<Vec<_>>(),
            vec!["authorization", "x-session"]
        );
        assert_eq!(rendered["authorization"], "Bearer t");
        assert_eq!(rendered["x-session"].len(), 16);
    }

    #[test]
    fn rendered_config_keeps_no_valueless_headers() {
        // Arrange
        let context = HeaderContext::global();
        let config = config(headers(&[("x-session", None), ("x-fixed", Some("fixed"))]));

        // Act
        let rendered = rendered_config(&config, &context);

        // Assert
        assert_eq!(rendered.headers.len(), 2);
        assert!(rendered.headers.values().all(Option::is_some));
        assert_eq!(rendered.headers["x-fixed"].as_deref(), Some("fixed"));
    }

    #[test]
    fn rendered_config_changes_the_cache_key_per_conversation() {
        // Arrange
        let config = config(headers(&[("x-session", None)]));

        // Act
        let musa =
            rendered_config(&config, &HeaderContext::for_agent("musa")).cache_key_with_revision(1);
        let lyre =
            rendered_config(&config, &HeaderContext::for_agent("lyre")).cache_key_with_revision(1);

        // Assert
        assert_ne!(musa, lyre);
    }
}

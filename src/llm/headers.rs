//! Provider header values that depend on the request scope.
//!
//! A `providers.<id>.headers` entry written without a value names a header
//! whose value must identify one conversation: upstream gateways use such a
//! header for routing and prompt caching, so sharing one value across every
//! conversation pins all traffic to a single upstream assignment. The value is
//! a digest of the scope key, which keeps it stable across restarts without
//! storing anything.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::{ProviderHeaderValue, ResolvedLlmConfig};
use crate::conversation::SurfaceContext;

/// Scope identity used to derive valueless header values.
///
/// The scope key combines the installation identity, the storage scope, and the
/// conversation (or a coarser unit for work without one), so two installations
/// sharing an upstream account never resolve to the same identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeaderContext {
    scope_key: String,
}

impl HeaderContext {
    /// Uses the conversation identified by `context` on `installation_id`.
    pub(crate) fn for_surface(installation_id: &str, context: &SurfaceContext) -> Self {
        Self {
            scope_key: format!(
                "{installation_id}:{}:{}",
                context.scope,
                context.session_key()
            ),
        }
    }

    /// Uses an agent-wide identity for work that has no conversation surface.
    pub(crate) fn for_agent(installation_id: &str, agent_id: &str) -> Self {
        Self {
            scope_key: format!("{installation_id}:agent:{agent_id}"),
        }
    }

    /// Uses an identity unique to this process, for callers without a conversation.
    pub(crate) fn process_global() -> Self {
        static PROCESS_NONCE: OnceLock<String> = OnceLock::new();
        Self {
            scope_key: PROCESS_NONCE
                .get_or_init(|| Uuid::new_v4().to_string())
                .clone(),
        }
    }
}

/// Renders a deterministic digest of `scope_key` in UUID form.
///
/// The value is not random: the same scope key always yields the same
/// identifier. UUID shape keeps it acceptable to gateways that expect a
/// session UUID.
fn derive_session_value(scope_key: &str) -> String {
    let digest = Sha256::digest(scope_key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

/// Returns `headers` with every valueless entry filled in for `context`.
pub(crate) fn render_headers(
    headers: &BTreeMap<String, ProviderHeaderValue>,
    context: &HeaderContext,
) -> BTreeMap<String, String> {
    let mut derived: Option<String> = None;
    headers
        .iter()
        .map(|(name, value)| {
            let value = match value {
                ProviderHeaderValue::Literal(value) => value.clone(),
                ProviderHeaderValue::Generated => derived
                    .get_or_insert_with(|| derive_session_value(&context.scope_key))
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
        .map(|(name, value)| (name, ProviderHeaderValue::Literal(value)))
        .collect();
    rendered
}

#[cfg(test)]
mod tests {
    use super::{HeaderContext, render_headers, rendered_config};
    use crate::config::{ProviderHeaderValue, ResolvedLlmConfig};
    use std::collections::BTreeMap;

    fn headers(entries: &[(&str, Option<&str>)]) -> BTreeMap<String, ProviderHeaderValue> {
        entries
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    Some(value) => ProviderHeaderValue::Literal((*value).to_string()),
                    None => ProviderHeaderValue::Generated,
                };
                ((*name).to_string(), value)
            })
            .collect()
    }

    fn config(headers: BTreeMap<String, ProviderHeaderValue>) -> ResolvedLlmConfig {
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

    const INSTALLATION: &str = "installation-a";

    fn assert_uuid_shaped(value: &str) {
        assert_eq!(value.len(), 36, "{value}");
        let parts: Vec<&str> = value.split('-').collect();
        assert_eq!(
            parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "{value}"
        );
        assert!(
            parts
                .iter()
                .all(|part| part.chars().all(|c| c.is_ascii_hexdigit())),
            "{value}"
        );
        assert_eq!(&value[14..15], "4", "version nibble: {value}");
    }

    #[test]
    fn valueless_header_becomes_a_uuid_shaped_value() {
        // Arrange
        let context =
            HeaderContext::for_surface(INSTALLATION, &crate::test_util::cli_context("uuid"));

        // Act
        let rendered = render_headers(&headers(&[("x-session", None)]), &context);

        // Assert
        assert_uuid_shaped(rendered.get("x-session").expect("rendered header"));
    }

    #[test]
    fn same_conversation_renders_the_same_value() {
        // Arrange
        let context =
            HeaderContext::for_surface(INSTALLATION, &crate::test_util::cli_context("stable"));

        // Act
        let first = render_headers(&headers(&[("x-session", None)]), &context);
        let second = render_headers(&headers(&[("x-session", None)]), &context);

        // Assert
        assert_eq!(first, second);
    }

    #[test]
    fn different_conversations_render_different_values() {
        // Arrange
        let first = HeaderContext::for_agent(INSTALLATION, "musa");
        let second = HeaderContext::for_agent(INSTALLATION, "lyre");

        // Act
        let first = render_headers(&headers(&[("x-session", None)]), &first);
        let second = render_headers(&headers(&[("x-session", None)]), &second);

        // Assert
        assert_ne!(first["x-session"], second["x-session"]);
    }

    #[test]
    fn different_installations_render_different_values() {
        // Arrange
        let first = HeaderContext::for_agent("installation-a", "musa");
        let second = HeaderContext::for_agent("installation-b", "musa");

        // Act
        let first = render_headers(&headers(&[("x-session", None)]), &first);
        let second = render_headers(&headers(&[("x-session", None)]), &second);

        // Assert
        assert_ne!(first["x-session"], second["x-session"]);
    }

    #[test]
    fn storage_scopes_render_different_values() {
        // Arrange
        let mut normal = crate::test_util::cli_context("scope");
        normal.scope = crate::conversation::ConversationScope::Normal;
        let mut secret = normal.clone();
        secret.scope = crate::conversation::ConversationScope::Secret;

        // Act
        let normal = HeaderContext::for_surface(INSTALLATION, &normal);
        let secret = HeaderContext::for_surface(INSTALLATION, &secret);

        // Assert
        let first = render_headers(&headers(&[("x-session", None)]), &normal);
        let second = render_headers(&headers(&[("x-session", None)]), &secret);
        assert_ne!(first["x-session"], second["x-session"]);
    }

    #[test]
    fn process_global_is_stable_within_the_process() {
        // Arrange
        let first = HeaderContext::process_global();
        let second = HeaderContext::process_global();

        // Act
        let first = render_headers(&headers(&[("x-session", None)]), &first);
        let second = render_headers(&headers(&[("x-session", None)]), &second);

        // Assert
        assert_eq!(first["x-session"], second["x-session"]);
        assert_uuid_shaped(&first["x-session"]);
    }

    #[test]
    fn literal_values_are_sent_verbatim_and_keys_are_preserved() {
        // Arrange
        let context = HeaderContext::process_global();

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
        assert_uuid_shaped(&rendered["x-session"]);
    }

    #[test]
    fn rendered_config_keeps_no_valueless_headers() {
        // Arrange
        let context = HeaderContext::process_global();
        let config = config(headers(&[("x-session", None), ("x-fixed", Some("fixed"))]));

        // Act
        let rendered = rendered_config(&config, &context);

        // Assert
        assert_eq!(rendered.headers.len(), 2);
        assert!(
            rendered
                .headers
                .values()
                .all(|value| matches!(value, ProviderHeaderValue::Literal(_)))
        );
        assert_eq!(
            rendered.headers["x-fixed"],
            ProviderHeaderValue::Literal("fixed".to_string())
        );
    }

    #[test]
    fn rendered_config_changes_the_cache_key_per_conversation() {
        // Arrange
        let config = config(headers(&[("x-session", None)]));

        // Act
        let musa = rendered_config(&config, &HeaderContext::for_agent(INSTALLATION, "musa"))
            .cache_key_with_revision(1);
        let lyre = rendered_config(&config, &HeaderContext::for_agent(INSTALLATION, "lyre"))
            .cache_key_with_revision(1);

        // Assert
        assert_ne!(musa, lyre);
    }
}

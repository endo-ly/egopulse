//! Bounded cache of constructed LLM providers.
//!
//! Providers are keyed by their effective request identity: the resolved
//! configuration revision plus the rendered headers. Conversations therefore
//! get their own provider, which makes the cache grow with the number of
//! active conversations. The HTTP connection pool is shared between providers,
//! so the cap bounds bookkeeping rather than sockets.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::LlmProvider;

/// Providers kept alive at once.
pub(crate) const PROVIDER_CACHE_CAP: usize = 128;

/// Insertion-ordered cache that drops its oldest provider when full.
#[derive(Default)]
pub(crate) struct ProviderCache {
    entries: HashMap<u64, Arc<dyn LlmProvider>>,
    order: VecDeque<u64>,
    cap: usize,
}

impl ProviderCache {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap,
            ..Self::default()
        }
    }

    pub(crate) fn get(&self, key: u64) -> Option<Arc<dyn LlmProvider>> {
        self.entries.get(&key).cloned()
    }

    pub(crate) fn insert(&mut self, key: u64, provider: Arc<dyn LlmProvider>) {
        if self.entries.insert(key, provider).is_none() {
            self.order.push_back(key);
        }
        while self.entries.len() > self.cap {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
            tracing::debug!(
                providers = self.len(),
                cap = self.cap,
                "evicted cached llm provider"
            );
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::ProviderCache;

    struct NullProvider {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl crate::llm::LlmProvider for NullProvider {
        fn provider_name(&self) -> &str {
            self.name
        }

        fn model_name(&self) -> &str {
            self.name
        }

        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<crate::llm::Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            Err(crate::error::LlmError::InvalidResponse("null".to_string()))
        }

        async fn send_message_streaming(
            &self,
            _system: &str,
            _messages: Arc<Vec<crate::llm::Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
            _on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
            Err(crate::error::LlmError::InvalidResponse("null".to_string()))
        }
    }

    fn provider(name: &'static str) -> Arc<dyn crate::llm::LlmProvider> {
        Arc::new(NullProvider { name })
    }

    #[test]
    fn returns_the_provider_inserted_for_a_key() {
        // Arrange
        let mut cache = ProviderCache::new(2);

        // Act
        cache.insert(7, provider("seven"));

        // Assert
        assert_eq!(cache.get(7).expect("cached").provider_name(), "seven");
        assert!(cache.get(8).is_none());
    }

    #[test]
    fn drops_the_oldest_provider_when_full() {
        // Arrange
        let mut cache = ProviderCache::new(2);

        // Act
        cache.insert(1, provider("one"));
        cache.insert(2, provider("two"));
        cache.insert(3, provider("three"));

        // Assert
        assert_eq!(cache.len(), 2);
        assert!(cache.get(1).is_none());
        assert!(cache.get(2).is_some());
        assert!(cache.get(3).is_some());
    }

    #[test]
    fn replacing_an_existing_key_keeps_it_once() {
        // Arrange
        let mut cache = ProviderCache::new(2);
        cache.insert(1, provider("old"));

        // Act
        cache.insert(1, provider("new"));

        // Assert
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(1).expect("cached").provider_name(), "new");
    }
}

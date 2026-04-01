use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::channel::ConversationKind;

#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)>;

    fn is_local_only(&self) -> bool {
        false
    }

    fn allows_cross_chat(&self) -> bool {
        true
    }

    async fn send_text(&self, external_chat_id: &str, text: &str) -> Result<(), String>;
}

#[derive(Default)]
pub struct ChannelRegistry {
    adapters: HashMap<String, Arc<dyn ChannelAdapter>>,
    type_to_channel: HashMap<String, String>,
    type_to_conversation: HashMap<String, ConversationKind>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, adapter: Arc<dyn ChannelAdapter>) {
        let name = adapter.name().to_string();
        for (chat_type, kind) in adapter.chat_type_routes() {
            self.type_to_channel
                .insert(chat_type.to_string(), name.clone());
            self.type_to_conversation
                .insert(chat_type.to_string(), kind);
        }
        self.adapters.insert(name, adapter);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn ChannelAdapter>> {
        self.adapters.get(name)
    }

    pub fn resolve(
        &self,
        db_chat_type: &str,
    ) -> Option<(&Arc<dyn ChannelAdapter>, ConversationKind)> {
        let channel_name = self.type_to_channel.get(db_chat_type)?;
        let adapter = self.adapters.get(channel_name)?;
        let kind = self.type_to_conversation.get(db_chat_type)?;
        Some((adapter, *kind))
    }

    pub fn resolve_routing(&self, db_chat_type: &str) -> Option<(&str, ConversationKind)> {
        let channel_name = self.type_to_channel.get(db_chat_type)?;
        let kind = self.type_to_conversation.get(db_chat_type)?;
        Some((channel_name.as_str(), *kind))
    }

    pub fn has_any(&self) -> bool {
        !self.adapters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::channels::WebAdapter;

    #[test]
    fn registry_registers_and_resolves() {
        let mut registry = ChannelRegistry::new();
        registry.register(Arc::new(WebAdapter));

        assert!(registry.has_any());
        assert!(registry.get("web").is_some());

        let (adapter, kind) = registry.resolve("web").expect("resolve web");
        assert_eq!(adapter.name(), "web");
        assert_eq!(kind, ConversationKind::Private);
    }

    #[test]
    fn resolve_routing_returns_name_and_kind() {
        let mut registry = ChannelRegistry::new();
        registry.register(Arc::new(WebAdapter));

        let (name, kind) = registry.resolve_routing("web").expect("resolve routing");
        assert_eq!(name, "web");
        assert_eq!(kind, ConversationKind::Private);
    }

    #[test]
    fn unknown_type_returns_none() {
        let registry = ChannelRegistry::new();
        assert!(registry.resolve("unknown").is_none());
        assert!(registry.resolve_routing("unknown").is_none());
    }
}

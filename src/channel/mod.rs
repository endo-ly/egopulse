//! Channel adapter abstraction for multi-channel support.
//!
//! This module provides the ChannelAdapter trait and ChannelRegistry for
//! routing messages to different channels (web, discord, telegram, etc.).
//!
//! Based on Microclaw's implementation:
//! https://github.com/microclaw/microclaw

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

mod web;

pub use web::WebAdapter;

/// Conversation kind for routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    Private,
    Group,
    Channel,
}

/// Channel adapter trait for multi-channel abstraction.
///
/// Each channel (web, discord, telegram, etc.) implements this trait
/// to handle message routing and delivery.
#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    /// Unique name: "telegram", "discord", "slack", "web"
    fn name(&self) -> &str;

    /// DB chat_type strings this adapter handles + whether each is private/group.
    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)>;

    /// Whether this channel stores messages only (no external delivery). Web = true.
    fn is_local_only(&self) -> bool {
        false
    }

    /// Whether chats on this channel can operate on other chats. Web = false.
    fn allows_cross_chat(&self) -> bool {
        true
    }

    /// Send text to external chat. Called by deliver_and_store_bot_message.
    async fn send_text(&self, external_chat_id: &str, text: &str) -> Result<(), String>;
}

/// Registry for channel adapters.
///
/// Manages registration and routing of messages to appropriate channel adapters.
#[derive(Default)]
pub struct ChannelRegistry {
    adapters: HashMap<String, Arc<dyn ChannelAdapter>>,
    /// "slack_dm" -> "slack", "telegram_private" -> "telegram", etc.
    type_to_channel: HashMap<String, String>,
    /// "slack_dm" -> Private, "group" -> Group, etc.
    type_to_conversation: HashMap<String, ConversationKind>,
}

impl ChannelRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a channel adapter.
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

    /// Get an adapter by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn ChannelAdapter>> {
        self.adapters.get(name)
    }

    /// Resolve a DB chat_type string to the adapter and conversation kind.
    pub fn resolve(&self, db_chat_type: &str) -> Option<(&Arc<dyn ChannelAdapter>, ConversationKind)> {
        let channel_name = self.type_to_channel.get(db_chat_type)?;
        let adapter = self.adapters.get(channel_name)?;
        let kind = self.type_to_conversation.get(db_chat_type)?;
        Some((adapter, *kind))
    }

    /// Resolve only the channel name and conversation kind (without needing the adapter).
    pub fn resolve_routing(&self, db_chat_type: &str) -> Option<(&str, ConversationKind)> {
        let channel_name = self.type_to_channel.get(db_chat_type)?;
        let kind = self.type_to_conversation.get(db_chat_type)?;
        Some((channel_name.as_str(), *kind))
    }

    /// Check if any adapters are registered.
    pub fn has_any(&self) -> bool {
        !self.adapters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

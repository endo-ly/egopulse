//! チャネルアダプターの登録とルーティング。
//!
//! 各チャネル (Web / Discord / Telegram) が実装する `ChannelAdapter` トレイトと、
//! データベース上の chat_type 文字列から適切なアダプターへ解決する `ChannelRegistry` を提供する。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Conversation type a channel can represent.
pub(crate) enum ConversationKind {
    Private,
    Group,
}

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

/// Trait that each channel (Web, Discord, Telegram) implements for outbound message delivery.
#[async_trait]
pub(crate) trait ChannelAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)>;

    async fn send_text(&self, external_chat_id: &str, text: &str) -> Result<(), String>;

    /// Sends a file attachment to the specified chat.
    ///
    /// Returns an error if the channel does not support file attachments.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the channel lacks attachment support, the file cannot
    /// be read, or the upstream API rejects the request.
    async fn send_attachment(
        &self,
        external_chat_id: &str,
        text: Option<&str>,
        file_path: &Path,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let _ = (external_chat_id, text, file_path, caption);
        Err("file attachments not supported on this channel".to_string())
    }
}

/// Registry mapping database chat types to their channel adapters.
#[derive(Default)]
pub(crate) struct ChannelRegistry {
    adapters: HashMap<String, Arc<dyn ChannelAdapter>>,
    type_to_channel: HashMap<String, String>,
    type_to_conversation: HashMap<String, ConversationKind>,
}

impl ChannelRegistry {
    /// Create an empty registry.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register a channel adapter and index all its chat type routes.
    pub(crate) fn register(&mut self, adapter: Arc<dyn ChannelAdapter>) {
        let name = adapter.name().to_string();
        for (chat_type, kind) in adapter.chat_type_routes() {
            self.type_to_channel
                .insert(chat_type.to_string(), name.clone());
            self.type_to_conversation
                .insert(chat_type.to_string(), kind);
        }
        self.adapters.insert(name, adapter);
    }

    /// Look up an adapter by its channel name.
    pub(crate) fn get(&self, name: &str) -> Option<&Arc<dyn ChannelAdapter>> {
        self.adapters.get(name)
    }

}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::channels::web::WebAdapter;

    #[test]
    fn registry_registers_and_resolves() {
        let mut registry = ChannelRegistry::new();
        registry.register(Arc::new(WebAdapter));

        assert!(registry.get("web").is_some());
    }
}

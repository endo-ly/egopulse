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

/// Keeps a channel-specific turn activity indicator alive until dropped.
pub(crate) trait TurnActivity: Send {}

struct NoopTurnActivity;

impl TurnActivity for NoopTurnActivity {}

/// Displays tool execution progress for a single turn.
///
/// A channel that supports progress indicators (Discord, Telegram) returns an
/// `Arc<dyn ToolProgressSink>` from [`ChannelAdapter::tool_progress_sink`]. The
/// coordinator clones this `Arc` and hands it to a `tokio::spawn`-ed task, so
/// the sink must be `Send + Sync + 'static`.
#[async_trait]
pub(crate) trait ToolProgressSink: Send + Sync {
    /// Posts the initial progress message and returns a handle for later edits.
    ///
    /// `external_chat_id` identifies the target chat (e.g. `discord:123:agent:lyre`)
    /// and `body` is the initial cumulative log text.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a human-readable message if the channel rejects the post.
    async fn begin(
        &self,
        external_chat_id: &str,
        body: &str,
    ) -> Result<Box<dyn ToolProgressHandle>, String>;
}

/// Handle returned by [`ToolProgressSink::begin`], used to edit and close the
/// posted progress message.
#[async_trait]
pub(crate) trait ToolProgressHandle: Send {
    /// Replaces the progress body by editing the posted message.
    ///
    /// Implementations truncate over-length bodies to the channel limit.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the edit API call fails.
    async fn update(&mut self, body: &str) -> Result<(), String>;

    /// Closes the progress display. The message is left in place as a log.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the channel rejects the final edit.
    async fn close(self: Box<Self>) -> Result<(), String>;
}

/// Trait that each channel (Web, Discord, Telegram) implements for outbound message delivery.
#[async_trait]
pub(crate) trait ChannelAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)>;

    /// Starts a channel-specific activity indicator for a running turn.
    async fn begin_turn_activity(
        &self,
        external_chat_id: &str,
    ) -> Result<Box<dyn TurnActivity>, String> {
        let _ = external_chat_id;
        Ok(Box::new(NoopTurnActivity))
    }

    async fn send_text(&self, external_chat_id: &str, text: &str) -> Result<(), String>;

    /// Returns the channel's tool-progress sink, if supported.
    ///
    /// The default returns `None`, leaving Voice / CLI / TUI / Web unaffected.
    /// Discord and Telegram override this to return their edit-based sink.
    fn tool_progress_sink(&self) -> Option<Arc<dyn ToolProgressSink>> {
        None
    }

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

    /// Returns registered channel names.
    pub(crate) fn names(&self) -> Vec<&str> {
        self.adapters.keys().map(String::as_str).collect()
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

    struct NoopAdapter;

    #[async_trait]
    impl ChannelAdapter for NoopAdapter {
        fn name(&self) -> &str {
            "noop"
        }
        fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
            Vec::new()
        }
        async fn send_text(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn default_tool_progress_sink_is_none() {
        // Arrange
        let adapter = NoopAdapter;

        // Act
        let sink = adapter.tool_progress_sink();

        // Assert
        assert!(
            sink.is_none(),
            "default adapter must not advertise tool progress"
        );
    }
}

//! Web channel adapter for local-only web UI.

use async_trait::async_trait;

use crate::channel::ConversationKind;
use crate::channel_adapter::ChannelAdapter;

/// Web adapter for local-only web UI.
///
/// This adapter is used for the web-based chat interface.
/// It does not send messages to external services.
pub struct WebAdapter;

#[async_trait]
impl ChannelAdapter for WebAdapter {
    fn name(&self) -> &str {
        "web"
    }

    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
        vec![("web", ConversationKind::Private)]
    }

    fn is_local_only(&self) -> bool {
        true
    }

    fn allows_cross_chat(&self) -> bool {
        false
    }

    async fn send_text(&self, _external_chat_id: &str, _text: &str) -> Result<(), String> {
        // Web adapter is local-only, no external delivery needed.
        // Messages are stored in the database and served via the web API.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_adapter_properties() {
        let adapter = WebAdapter;

        assert_eq!(adapter.name(), "web");
        assert!(adapter.is_local_only());
        assert!(!adapter.allows_cross_chat());

        let routes = adapter.chat_type_routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0], ("web", ConversationKind::Private));
    }

    #[tokio::test]
    async fn web_adapter_send_text_succeeds() {
        let adapter = WebAdapter;
        let result = adapter.send_text("any-id", "any text").await;
        assert!(result.is_ok());
    }
}

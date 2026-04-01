//! HTTP server module for EgoPulse Gateway.
//!
//! This module provides the axum-based HTTP server with:
//! - Health endpoints
//! - Sessions API
//! - SSE streaming chat
//! - WebUI

mod chat;
mod health;
mod router;
mod sessions;
pub mod sse;
mod ui;

pub use router::build_router;

use std::net::SocketAddr;

use async_trait::async_trait;

use crate::channel::ConversationKind;
use crate::channel_adapter::ChannelAdapter;
use crate::error::EgoPulseError;
use crate::runtime::AppState;

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
        Ok(())
    }
}

/// Run the HTTP server with graceful shutdown.
pub async fn run_server(state: AppState, host: &str, port: u16) -> Result<(), EgoPulseError> {
    let addr: SocketAddr = format!("{}:{}", host, port).parse().map_err(|e| {
        EgoPulseError::Channel(crate::error::ChannelError::SendFailed(format!(
            "Invalid address: {}",
            e
        )))
    })?;

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        EgoPulseError::Channel(crate::error::ChannelError::SendFailed(format!(
            "Failed to bind: {}",
            e
        )))
    })?;

    tracing::info!("EgoPulse server listening on http://{}", addr);

    let app = build_router(state);

    let shutdown_signal = async {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to install signal handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }

        tracing::info!("Shutdown signal received");
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .map_err(|e| {
            EgoPulseError::Channel(crate::error::ChannelError::SendFailed(format!(
                "Server error: {}",
                e
            )))
        })?;

    Ok(())
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

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

use crate::error::EgoPulseError;
use crate::runtime::AppState;

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

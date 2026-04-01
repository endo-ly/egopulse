//! Router configuration for EgoPulse HTTP server.

use axum::Router;
use axum::routing::{get, post};

use crate::runtime::AppState;

use super::handlers;

/// Build the main application router.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Health endpoints
        .route("/health", get(handlers::health::health))
        .route("/api/health", get(handlers::health::health))
        // Sessions API
        .route("/api/sessions", get(handlers::sessions::list_sessions))
        .route("/api/history", get(handlers::sessions::get_history))
        // Chat API with SSE
        .route("/api/send_stream", post(handlers::chat::send_stream))
        .route("/api/stream", get(handlers::chat::stream))
        // WebUI fallback
        .fallback(handlers::ui::serve_ui)
        .with_state(state)
}

//! Router configuration for EgoPulse HTTP server.

use axum::Router;
use axum::routing::{get, post};

use crate::runtime::AppState;

use super::{chat, health, sessions, ui};

/// Build the main application router.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Health endpoints
        .route("/health", get(health::health))
        .route("/api/health", get(health::health))
        // Sessions API
        .route("/api/sessions", get(sessions::list_sessions))
        .route("/api/history", get(sessions::get_history))
        // Chat API with SSE
        .route("/api/send_stream", post(chat::send_stream))
        .route("/api/stream", get(chat::stream))
        // WebUI fallback
        .fallback(ui::serve_ui)
        .with_state(state)
}

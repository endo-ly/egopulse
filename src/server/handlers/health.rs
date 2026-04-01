//! Health check handlers.

use axum::extract::State;
use axum::Json;
use serde_json::json;

use crate::runtime::AppState;

/// Health check endpoint.
pub async fn health(_state: State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION")
    }))
}

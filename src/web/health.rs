use axum::Json;
use axum::extract::State;

use super::WebState;

pub(super) async fn health(_state: State<WebState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION")
    }))
}

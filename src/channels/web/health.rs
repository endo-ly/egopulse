//! Web 層のヘルスチェック API。
//!
//! バージョン付きの最小レスポンスを返し、稼働確認に使う。

use axum::Json;
use axum::extract::State;

use super::WebState;

/// Returns a minimal health payload for the web server.
pub(super) async fn health(_state: State<WebState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION")
    }))
}

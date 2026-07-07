use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

pub(super) fn webhook_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        axum::Json(json!({
            "ok": false,
            "error": code,
            "message": message,
        })),
    )
        .into_response()
}

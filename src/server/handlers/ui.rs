//! WebUI static file serving.

use axum::response::{Html, IntoResponse, Response};
use include_dir::{Dir, include_dir};

static WEB_ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/webui");

/// Serve WebUI static files.
pub async fn serve_ui() -> Response {
    match WEB_ASSETS.get_file("index.html") {
        Some(file) => Html(String::from_utf8_lossy(file.contents()).to_string()).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

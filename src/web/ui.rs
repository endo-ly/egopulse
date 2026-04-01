//! WebUI static file serving.

use axum::extract::OriginalUri;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use include_dir::{Dir, include_dir};

static WEB_ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/web");

/// Serve WebUI static files.
pub async fn serve_ui(uri: OriginalUri) -> Response {
    let asset_path = asset_path_for_uri(&uri);
    let content_type = content_type_for_path(&asset_path);

    match WEB_ASSETS.get_file(&asset_path) {
        Some(file) => (
            [(header::CONTENT_TYPE, content_type)],
            String::from_utf8_lossy(file.contents()).to_string(),
        )
            .into_response(),
        None if !asset_path.contains('.') => serve_index(),
        None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

fn serve_index() -> Response {
    match WEB_ASSETS.get_file("index.html") {
        Some(file) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            String::from_utf8_lossy(file.contents()).to_string(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

fn asset_path_for_uri(uri: &OriginalUri) -> String {
    let path = uri.path().trim_start_matches('/');
    if path.is_empty() {
        "index.html".to_string()
    } else {
        path.to_string()
    }
}

fn content_type_for_path(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    }
}
